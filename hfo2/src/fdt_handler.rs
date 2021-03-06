/*
 * Copyright 2019 Sanguk Park.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use core::convert::TryInto;
use core::mem;
use core::ptr;
use core::slice;

use crate::addr::*;
use crate::arch::*;
use crate::boot_params::*;
use crate::fdt::*;
use crate::layout::*;
use crate::mm::*;
use crate::mpool::*;
use crate::page::*;
use crate::types::*;

use scopeguard::{guard, ScopeGuard};

fn convert_number(data: &[u8]) -> Option<u64> {
    let ret = match data.len() {
        4 => u64::from(u32::from_be_bytes(data.try_into().unwrap())),
        8 => u64::from_be_bytes(data.try_into().unwrap()),
        _ => return None,
    };

    Some(ret)
}

impl<'a> FdtNode<'a> {
    fn read_number(&self, name: *const u8) -> Result<u64, ()> {
        let data = self.read_property(name)?;

        convert_number(data).ok_or(())
    }

    unsafe fn write_number(&mut self, name: *const u8, value: u64) -> Result<(), ()> {
        let data = self.read_property(name)?;

        #[allow(clippy::cast_ptr_alignment)]
        match data.len() {
            4 => {
                *(data as *const _ as *mut u8 as *mut u32) = u32::from_be(value as u32);
            }
            8 => {
                *(data as *const _ as *mut u8 as *mut u64) = u64::from_be(value);
            }
            _ => return Err(()),
        }

        Ok(())
    }

    /// Finds the memory region where initrd is stored, and updates the fdt node
    /// cursor to the node called "chosen".
    pub fn find_initrd(&mut self) -> Option<(paddr_t, paddr_t)> {
        if self.find_child("chosen\0".as_ptr()).is_none() {
            dlog!("Unable to find 'chosen'\n");
            return None;
        }

        let initrd_begin = ok_or!(self.read_number("linux,initrd-start\0".as_ptr()), {
            dlog!("Unable to read linux,initrd-start\n");
            return None;
        });

        let initrd_end = ok_or!(self.read_number("linux,initrd-end\0".as_ptr()), {
            dlog!("Unable to read linux,initrd-end\n");
            return None;
        });

        let begin = pa_init(initrd_begin as usize);
        let end = pa_init(initrd_end as usize);

        Some((begin, end))
    }

    pub fn find_cpus(&self, cpu_ids: &mut [cpu_id_t]) -> Option<usize> {
        let mut node = self.clone();
        let mut cpu_count = 0;

        node.find_child("cpus\0".as_ptr()).or_else(|| {
            dlog!("Unable to find 'cpus'\n");
            None
        })?;

        let address_size = node
            .read_number("#address-cells\0".as_ptr())
            .map(|size| size as usize * mem::size_of::<u32>())
            .unwrap_or(mem::size_of::<u32>());

        node.first_child()?;

        // TODO(HfO2): this loop was do-while in C. Make an interator for this.
        loop {
            if node
                .read_property("device_type\0".as_ptr())
                .ok()
                .filter(|data| *data == "cpu\0".as_bytes())
                .is_none()
            {
                if node.next_sibling().is_none() {
                    break;
                } else {
                    continue;
                }
            }

            let mut data = ok_or!(node.read_property("reg\0".as_ptr()), {
                if node.next_sibling().is_none() {
                    break;
                } else {
                    continue;
                }
            });

            // Get all entries for this CPU.
            while data.len() as usize >= address_size {
                if cpu_count >= MAX_CPUS {
                    dlog!("Found more than {} CPUs\n", MAX_CPUS);
                    return None;
                }

                cpu_ids[cpu_count] = convert_number(&data[..address_size]).unwrap() as cpu_id_t;
                cpu_count += 1;

                data = &data[address_size..];
            }

            if node.next_sibling().is_none() {
                break;
            }
        }

        Some(cpu_count)
    }

    pub fn find_memory_ranges(&self, p: &mut BootParams) -> Option<()> {
        let mut node = self.clone();

        // Get the sizes of memory range addresses and sizes.
        let address_size = node
            .read_number("#address-cells\0".as_ptr())
            .map(|size| size as usize * mem::size_of::<u32>())
            .unwrap_or(mem::size_of::<u32>());

        let size_size = node
            .read_number("#size-cells\0".as_ptr())
            .map(|size| size as usize * mem::size_of::<u32>())
            .unwrap_or(mem::size_of::<u32>());

        let entry_size = address_size + size_size;

        // Look for nodes with the device_type set to "memory".
        node.first_child()?;
        let mut mem_range_index = 0;

        // TODO(HfO2): this loop was do-while in C. Make an interator for this.
        loop {
            if node
                .read_property("device_type\0".as_ptr())
                .ok()
                .filter(|data| *data == "memory\0".as_bytes())
                .is_none()
            {
                if node.next_sibling().is_none() {
                    break;
                } else {
                    continue;
                }
            }
            let mut data = ok_or!(node.read_property("reg\0".as_ptr()), {
                if node.next_sibling().is_none() {
                    break;
                } else {
                    continue;
                }
            });

            // Traverse all memory ranges within this node.
            while data.len() >= entry_size {
                let addr = convert_number(&data[..address_size]).unwrap() as usize;
                let len = convert_number(&data[address_size..entry_size]).unwrap() as usize;

                if mem_range_index < MAX_MEM_RANGES {
                    p.mem_ranges[mem_range_index].begin = pa_init(addr);
                    p.mem_ranges[mem_range_index].end = pa_init(addr + len);

                    mem_range_index += 1;
                } else {
                    dlog!("Found memory range {} in FDT but only {} supported, ignoring additional range of size {}.\n", mem_range_index, MAX_MEM_RANGES, len);
                }

                data = &data[entry_size..];
            }

            if node.next_sibling().is_none() {
                break;
            }
        }

        p.mem_ranges_count = mem_range_index;
        Some(())
    }
}

pub unsafe fn map(
    stage1_ptable: &mut PageTable<Stage1>,
    fdt_addr: paddr_t,
    ppool: &MPool,
) -> Option<FdtNode<'static>> {
    if stage1_ptable
        .identity_map(
            fdt_addr,
            pa_add(fdt_addr, mem::size_of::<FdtHeader>()),
            Mode::R,
            ppool,
        )
        .is_err()
    {
        dlog!("Unable to map FDT header.\n");
        return None;
    }

    let mut stage1_ptable = guard(stage1_ptable, |ptable| {
        let _ = ptable.unmap(
            fdt_addr,
            pa_add(fdt_addr, mem::size_of::<FdtHeader>()),
            ppool,
        );
    });

    let fdt = pa_addr(fdt_addr) as *mut FdtHeader;

    let node = some_or!(FdtNode::new_root(&*fdt), {
        dlog!("FDT failed validation.\n");
        return None;
    });

    // Map the rest of the fdt in.
    if stage1_ptable
        .identity_map(
            fdt_addr,
            pa_add(fdt_addr, (*fdt).total_size() as usize),
            Mode::R,
            ppool,
        )
        .is_err()
    {
        dlog!("Unable to map full FDT.\n");
        return None;
    }

    mem::forget(stage1_ptable);
    Some(node)
}

pub unsafe fn unmap(
    stage1_ptable: &mut PageTable<Stage1>,
    fdt: &FdtHeader,
    ppool: &MPool,
) -> Result<(), ()> {
    let fdt_addr = pa_init(fdt as *const _ as usize);

    stage1_ptable.unmap(fdt_addr, pa_add(fdt_addr, fdt.total_size() as usize), ppool)
}

pub unsafe fn patch(
    stage1_ptable: &mut PageTable<Stage1>,
    fdt_addr: paddr_t,
    p: &BootParamsUpdate,
    ppool: &MPool,
) -> Result<(), ()> {
    // Map the fdt header in.
    if stage1_ptable
        .identity_map(
            fdt_addr,
            pa_add(fdt_addr, mem::size_of::<FdtHeader>()),
            Mode::R,
            ppool,
        )
        .is_err()
    {
        dlog!("Unable to map FDT header.\n");
        return Err(());
    }

    let mut stage1_ptable = guard(stage1_ptable, |ptable| {
        let _ = ptable.unmap(
            fdt_addr,
            pa_add(fdt_addr, mem::size_of::<FdtHeader>()),
            ppool,
        );
    });

    let fdt = pa_addr(fdt_addr) as *mut FdtHeader;

    let mut node = FdtNode::new_root(&*fdt)
        .or_else(|| {
            dlog!("FDT failed validation.\n");
            None
        })
        .ok_or(())?;
    let total_size = (*fdt).total_size();

    // Map the fdt (+ a page) in r/w mode in preparation for updating it.
    if stage1_ptable
        .identity_map(
            fdt_addr,
            pa_add(fdt_addr, total_size as usize + PAGE_SIZE),
            Mode::R | Mode::W,
            ppool,
        )
        .is_err()
    {
        dlog!("Unable to map FDT in r/w mode.\n");
        return Err(());
    }

    let stage1_ptable = guard(ScopeGuard::into_inner(stage1_ptable), |ptable| {
        if ptable
            .unmap(
                fdt_addr,
                pa_add(fdt_addr, total_size as usize + PAGE_SIZE),
                ppool,
            )
            .is_err()
        {
            dlog!("Unable to unmap writable FDT.\n");
        }
    });

    if node.find_child("\0".as_ptr()).is_none() {
        dlog!("Unable to find FDT root node.\n");
        return Err(());
    }

    if node.find_child("chosen\0".as_ptr()).is_none() {
        dlog!("Unable to find 'chosen'\n");
        return Err(());
    }

    // Patch FDT to point to new ramdisk.
    if node
        .write_number(
            "linux,initrd-start\0".as_ptr(),
            pa_addr(p.initrd_begin) as u64,
        )
        .is_err()
    {
        dlog!("Unable to write linux,initrd-start\n");
        return Err(());
    }

    if node
        .write_number("linux,initrd-end\0".as_ptr(), pa_addr(p.initrd_end) as u64)
        .is_err()
    {
        dlog!("Unable to write linux,initrd-end\n");
        return Err(());
    }

    // Patch FDT to reserve hypervisor memory so the primary VM doesn't try to
    // use it.
    (*fdt).add_mem_reservation(
        pa_addr(layout_text_begin()) as u64,
        pa_difference(layout_text_begin(), layout_text_end()) as u64,
    );
    (*fdt).add_mem_reservation(
        pa_addr(layout_rodata_begin()) as u64,
        pa_difference(layout_rodata_begin(), layout_rodata_end()) as u64,
    );
    (*fdt).add_mem_reservation(
        pa_addr(layout_data_begin()) as u64,
        pa_difference(layout_data_begin(), layout_data_end()) as u64,
    );

    // Patch FDT to reserve memory for secondary VMs.
    for i in 0..p.reserved_ranges_count {
        (*fdt).add_mem_reservation(
            pa_addr(p.reserved_ranges[i].begin) as u64,
            pa_addr(p.reserved_ranges[i].end) as u64 - pa_addr(p.reserved_ranges[i].begin) as u64,
        );
    }

    let stage1_ptable = ScopeGuard::into_inner(stage1_ptable);
    if stage1_ptable
        .unmap(
            fdt_addr,
            pa_add(fdt_addr, (*fdt).total_size() as usize + PAGE_SIZE),
            ppool,
        )
        .is_err()
    {
        dlog!("Unable to unmap writable FDT.\n");
        return Err(());
    }

    Ok(())
}

#[no_mangle]
pub unsafe extern "C" fn fdt_map(
    mut stage1_locked: mm_stage1_locked,
    fdt_addr: paddr_t,
    n: *mut fdt_node,
    ppool: *const MPool,
) -> *const FdtHeader {
    let node = some_or!(
        map(&mut stage1_locked, fdt_addr, &*ppool),
        return ptr::null()
    );
    ptr::write(n, node.into());
    pa_addr(fdt_addr) as _
}

#[no_mangle]
pub unsafe extern "C" fn fdt_unmap(
    mut stage1_locked: mm_stage1_locked,
    fdt: *const FdtHeader,
    ppool: *const MPool,
) -> bool {
    unmap(&mut stage1_locked, &*fdt, &*ppool).is_ok()
}

#[no_mangle]
pub unsafe extern "C" fn fdt_find_cpus(
    root: *const fdt_node,
    cpu_ids: *mut cpu_id_t,
    cpu_count: *mut usize,
) {
    let count = FdtNode::from((*root).clone())
        .find_cpus(slice::from_raw_parts_mut(cpu_ids, MAX_CPUS))
        .unwrap_or(0);

    ptr::write(cpu_count, count);
}

#[no_mangle]
pub unsafe extern "C" fn fdt_find_memory_ranges(root: *const fdt_node, p: *mut BootParams) {
    FdtNode::from((*root).clone()).find_memory_ranges(&mut *p);
}

#[no_mangle]
pub unsafe extern "C" fn fdt_find_initrd(
    n: *mut fdt_node,
    begin: *mut paddr_t,
    end: *mut paddr_t,
) -> bool {
    let mut node = FdtNode::from((*n).clone());
    let (b, e) = some_or!(node.find_initrd(), return false);
    ptr::write(begin, b);
    ptr::write(end, e);
    ptr::write(n, node.into());
    true
}

#[no_mangle]
pub unsafe extern "C" fn fdt_patch(
    mut stage1_locked: mm_stage1_locked,
    fdt_addr: paddr_t,
    p: *const BootParamsUpdate,
    ppool: *const MPool,
) -> bool {
    patch(&mut stage1_locked, fdt_addr, &*p, &*ppool).is_ok()
}

#[cfg(test)]
mod test {
    extern crate std;
    use core::mem::MaybeUninit;
    use std::boxed::Box;

    use super::*;

    #[link(name = "fake_arch", kind = "static")]
    extern "C" {}

    #[repr(align(4))]
    struct AlignedDtb {
        data: [u8; 32 * 12 - 1],
    }

    static TEST_DTB: AlignedDtb = AlignedDtb {
        data: [
            0xd0, 0x0d, 0xfe, 0xed, 0x00, 0x00, 0x01, 0x7f, 0x00, 0x00, 0x00, 0x38, 0x00, 0x00,
            0x01, 0x30, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x10,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4f, 0x00, 0x00, 0x00, 0xf8, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00,
            0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
            0x00, 0x01, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x40, 0x30, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x1b, 0x6d, 0x65,
            0x6d, 0x6f, 0x72, 0x79, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x20,
            0x00, 0x00, 0x00, 0x27, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
            0x00, 0x01, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x40, 0x31, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x1b, 0x6d, 0x65,
            0x6d, 0x6f, 0x72, 0x79, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x10,
            0x00, 0x00, 0x00, 0x27, 0x00, 0x00, 0x00, 0x00, 0x30, 0x02, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01,
            0x63, 0x68, 0x6f, 0x73, 0x65, 0x6e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00,
            0x00, 0x04, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x3e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x09, 0x23, 0x61, 0x64, 0x64,
            0x72, 0x65, 0x73, 0x73, 0x2d, 0x63, 0x65, 0x6c, 0x6c, 0x73, 0x00, 0x23, 0x73, 0x69,
            0x7a, 0x65, 0x2d, 0x63, 0x65, 0x6c, 0x6c, 0x73, 0x00, 0x64, 0x65, 0x76, 0x69, 0x63,
            0x65, 0x5f, 0x74, 0x79, 0x70, 0x65, 0x00, 0x72, 0x65, 0x67, 0x00, 0x6c, 0x69, 0x6e,
            0x75, 0x78, 0x2c, 0x69, 0x6e, 0x69, 0x74, 0x72, 0x64, 0x2d, 0x73, 0x74, 0x61, 0x72,
            0x74, 0x00, 0x6c, 0x69, 0x6e, 0x75, 0x78, 0x2c, 0x69, 0x6e, 0x69, 0x74, 0x72, 0x64,
            0x2d, 0x65, 0x6e, 0x64, 0x00,
        ],
    };

    const TEST_HEAP_SIZE: usize = PAGE_SIZE * 10;

    #[test]
    fn find_memory_ranges() {
        let mut test_heap: Box<[u8; TEST_HEAP_SIZE]> =
            Box::new(unsafe { MaybeUninit::uninit().assume_init() });

        let mut ppool: MPool = MPool::new();
        ppool.free_pages(
            unsafe { Pages::from_raw_u8(test_heap.as_mut_ptr(), TEST_HEAP_SIZE) }.unwrap(),
        );

        let mm = MemoryManager::new(&ppool).unwrap();
        let mut ptable = mm.hypervisor_ptable.lock();
        let mut n: FdtNode = unsafe {
            map(
                &mut ptable,
                pa_init(&TEST_DTB.data as *const _ as _),
                &mut ppool,
            )
            .unwrap()
        };

        let fdt = &TEST_DTB.data as *const _ as *const FdtHeader;

        assert!(n.find_child("\0".as_ptr()).is_some());

        let mut params = BootParams {
            cpu_ids: [0; MAX_CPUS],
            cpu_count: 0,
            mem_ranges: [MemRange::new(pa_init(0), pa_init(0)); MAX_MEM_RANGES],
            mem_ranges_count: 0,
            initrd_begin: pa_init(0),
            initrd_end: pa_init(0),
            kernel_arg: 0,
        };

        n.find_memory_ranges(&mut params);

        assert!(unsafe { unmap(&mut ptable, &*fdt, &mut ppool) }.is_ok());

        assert_eq!(params.mem_ranges_count, 3);
        assert_eq!(pa_addr(params.mem_ranges[0].begin), 0x0000_0000);
        assert_eq!(pa_addr(params.mem_ranges[0].end), 0x2000_0000);
        assert_eq!(pa_addr(params.mem_ranges[1].begin), 0x3000_0000);
        assert_eq!(pa_addr(params.mem_ranges[1].end), 0x3001_0000);
        assert_eq!(pa_addr(params.mem_ranges[2].begin), 0x3002_0000);
        assert_eq!(pa_addr(params.mem_ranges[2].end), 0x3003_0000);

        mem::drop(ptable);
        mem::forget(mm); // Do not drop PageTable.
    }
}
