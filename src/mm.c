#include "mm.h"

#include <stdatomic.h>
#include <stdint.h>

#include "alloc.h"
#include "dlog.h"

/* Keep macro alignment */
/* clang-format off */

#define MAP_FLAG_SYNC   0x01
#define MAP_FLAG_COMMIT 0x02

/* clang-format on */

extern uint8_t text_begin[];
extern uint8_t text_end[];
extern uint8_t rodata_begin[];
extern uint8_t rodata_end[];
extern uint8_t data_begin[];
extern uint8_t data_end[];

static struct mm_ptable ptable;

/**
 * Calculates the size of the address space represented by a page table entry at
 * the given level.
 */
static inline size_t mm_entry_size(int level)
{
	return 1ull << (PAGE_BITS + level * PAGE_LEVEL_BITS);
}

/**
 * For a given virtual address, calculates the maximum (plus one) address that
 * can be represented by the same table at the given level.
 */
static inline vaddr_t mm_level_end(vaddr_t va, int level)
{
	size_t offset = PAGE_BITS + (level + 1) * PAGE_LEVEL_BITS;
	return va_init(((va_addr(va) >> offset) + 1) << offset);
}

/**
 * For a given virtual address, calculates the index at which its entry is
 * stored in a table at the given level.
 */
static inline size_t mm_index(vaddr_t va, int level)
{
	uintvaddr_t v = va_addr(va) >> (PAGE_BITS + level * PAGE_LEVEL_BITS);
	return v & ((1ull << PAGE_LEVEL_BITS) - 1);
}

/**
 * Populates the provided page table entry with a reference to another table if
 * needed, that is, if it does not yet point to another table.
 *
 * Returns a pointer to the table the entry now points to.
 */
static pte_t *mm_populate_table_pte(pte_t *pte, int level, bool sync_alloc)
{
	pte_t *ntable;
	pte_t v = *pte;
	pte_t new_pte;
	size_t i;
	size_t inc;

	/* Just return pointer to table if it's already populated. */
	if (arch_mm_pte_is_table(v)) {
		return arch_mm_pte_to_table(v);
	}

	/* Allocate a new table. */
	ntable = (sync_alloc ? halloc_aligned : halloc_aligned_nosync)(
		PAGE_SIZE, PAGE_SIZE);
	if (!ntable) {
		dlog("Failed to allocate memory for page table\n");
		return NULL;
	}

	/* Determine template for new pte and its increment. */
	if (!arch_mm_pte_is_block(v)) {
		inc = 0;
		new_pte = arch_mm_absent_pte();
	} else {
		inc = mm_entry_size(level - 1);
		if (level == 1) {
			new_pte = arch_mm_block_to_page_pte(v);
		} else {
			new_pte = v;
		}
	}

	/* Initialise entries in the new table. */
	for (i = 0; i < PAGE_SIZE / sizeof(paddr_t); i++) {
		ntable[i] = new_pte;
		new_pte += inc;
	}

	/*
	 * Ensure initialisation is visible before updating the actual pte, then
	 * update it.
	 */
	atomic_thread_fence(memory_order_release);
	*pte = arch_mm_pa_to_table_pte(pa_init((uintpaddr_t)ntable));

	return ntable;
}

/**
 * Frees all page-table-related memory associated with the given pte at the
 * given level.
 */
static void mm_free_page_pte(pte_t pte, int level, bool sync)
{
	(void)pte;
	(void)level;
	(void)sync;
	/* TODO: Implement.
	if (!arch_mm_pte_is_present(pte) || level < 1)
		return;
	*/
}

/**
 * Updates the page table at the given level to map the given virtual address
 * range to a physical range using the provided (architecture-specific)
 * attributes.
 *
 * This function calls itself recursively if it needs to update additional
 * levels, but the recursion is bound by the maximum number of levels in a page
 * table.
 */
static bool mm_map_level(vaddr_t va_begin, vaddr_t va_end, paddr_t pa,
			 uint64_t attrs, pte_t *table, int level, int flags)
{
	pte_t *pte = table + mm_index(va_begin, level);
	uintvaddr_t level_end = va_addr(mm_level_end(va_begin, level));
	uintvaddr_t begin = va_addr(va_begin);
	uintvaddr_t end = va_addr(va_end);
	size_t entry_size = mm_entry_size(level);
	bool commit = flags & MAP_FLAG_COMMIT;
	bool sync = flags & MAP_FLAG_SYNC;

	/* Cap end so that we don't go over the current level max. */
	if (end > level_end) {
		end = level_end;
	}

	/* Fill each entry in the table. */
	while (begin < end) {
		if (level == 0) {
			if (commit) {
				*pte = arch_mm_pa_to_page_pte(pa, attrs);
			}
		} else if ((end - begin) >= entry_size &&
			   arch_mm_is_block_allowed(level) &&
			   (begin & (entry_size - 1)) == 0) {
			if (commit) {
				pte_t v = *pte;
				*pte = arch_mm_pa_to_block_pte(pa, attrs);
				/* TODO: Add barrier. How do we ensure this
				 * isn't in use by another CPU? Send IPI? */
				mm_free_page_pte(v, level, sync);
			}
		} else {
			pte_t *nt = mm_populate_table_pte(pte, level, sync);
			if (!nt) {
				return false;
			}

			if (!mm_map_level(va_begin, va_end, pa, attrs, nt,
					  level - 1, flags)) {
				return false;
			}
		}

		begin = (begin + entry_size) & ~(entry_size - 1);
		pa = pa_init((pa_addr(pa) + entry_size) & ~(entry_size - 1));
		pte++;
	}

	return true;
}

/**
 * Invalidates the TLB for the given virtual address range.
 */
static void mm_invalidate_tlb(vaddr_t begin, vaddr_t end, bool stage1)
{
	if (stage1) {
		arch_mm_invalidate_stage1_range(begin, end);
	} else {
		arch_mm_invalidate_stage2_range(begin, end);
	}
}

/**
 * Updates the given table such that the given virtual address range is mapped
 * to the corresponding physical address range in the architecture-agnostic mode
 * provided.
 */
bool mm_ptable_identity_map(struct mm_ptable *t, vaddr_t begin, vaddr_t end,
			    int mode)
{
	uint64_t attrs = arch_mm_mode_to_attrs(mode);
	int flags = (mode & MM_MODE_NOSYNC) ? 0 : MAP_FLAG_SYNC;
	int level = arch_mm_max_level(mode);
	pte_t *table = ptr_from_va(va_from_pa(t->table));
	paddr_t paddr = arch_mm_clear_pa(pa_from_va(begin));

	begin = arch_mm_clear_va(begin);
	end = arch_mm_clear_va(va_add(end, PAGE_SIZE - 1));

	/*
	 * Do it in two steps to prevent leaving the table in a halfway updated
	 * state. In such a two-step implementation, the table may be left with
	 * extra internal tables, but no different mapping on failure.
	 */
	if (!mm_map_level(begin, end, paddr, attrs, table, level, flags)) {
		return false;
	}

	mm_map_level(begin, end, paddr, attrs, table, level,
		     flags | MAP_FLAG_COMMIT);

	/* Invalidate the tlb. */
	if (!(mode & MM_MODE_NOINVALIDATE)) {
		mm_invalidate_tlb(begin, end, (mode & MM_MODE_STAGE1) != 0);
	}

	return true;
}

/**
 * Updates the given table such that the given virtual address range is not
 * mapped to any physical address.
 */
bool mm_ptable_unmap(struct mm_ptable *t, vaddr_t begin, vaddr_t end, int mode)
{
	int flags = (mode & MM_MODE_NOSYNC) ? 0 : MAP_FLAG_SYNC;
	int level = arch_mm_max_level(mode);
	pte_t *table = ptr_from_va(va_from_pa(t->table));

	begin = arch_mm_clear_va(begin);
	end = arch_mm_clear_va(va_add(end, PAGE_SIZE - 1));

	/* Also do updates in two steps, similarly to mm_ptable_identity_map. */
	if (!mm_map_level(begin, end, pa_from_va(begin), 0, table, level,
			  flags)) {
		return false;
	}

	mm_map_level(begin, end, pa_from_va(begin), 0, table, level,
		     flags | MAP_FLAG_COMMIT);

	/* Invalidate the tlb. */
	if (!(mode & MM_MODE_NOINVALIDATE)) {
		mm_invalidate_tlb(begin, end, (mode & MM_MODE_STAGE1) != 0);
	}

	return true;
}

/**
 * Updates the given table such that a single virtual address page is mapped to
 * the corresponding physical address page in the provided architecture-agnostic
 * mode.
 */
bool mm_ptable_identity_map_page(struct mm_ptable *t, vaddr_t va, int mode)
{
	size_t i;
	uint64_t attrs = arch_mm_mode_to_attrs(mode);
	pte_t *table = ptr_from_va(va_from_pa(t->table));
	bool sync = !(mode & MM_MODE_NOSYNC);
	paddr_t pa = arch_mm_clear_pa(pa_from_va(va));

	va = arch_mm_clear_va(va);

	for (i = arch_mm_max_level(mode); i > 0; i--) {
		table = mm_populate_table_pte(table + mm_index(va, i), i, sync);
		if (!table) {
			return false;
		}
	}

	i = mm_index(va, 0);
	table[i] = arch_mm_pa_to_page_pte(pa, attrs);
	return true;
}

/**
 * Writes the given table to the debug log, calling itself recursively to
 * write sub-tables.
 */
static void mm_dump_table_recursive(pte_t *table, int level, int max_level)
{
	uint64_t i;
	for (i = 0; i < PAGE_SIZE / sizeof(pte_t); i++) {
		if (!arch_mm_pte_is_present(table[i])) {
			continue;
		}

		dlog("%*s%x: %x\n", 4 * (max_level - level), "", i, table[i]);
		if (!level) {
			continue;
		}

		if (arch_mm_pte_is_table(table[i])) {
			mm_dump_table_recursive(arch_mm_pte_to_table(table[i]),
						level - 1, max_level);
		}
	}
}

/**
 * Write the given table to the debug log.
 */
void mm_ptable_dump(struct mm_ptable *t, int mode)
{
	pte_t *table = ptr_from_va(va_from_pa(t->table));
	int max_level = arch_mm_max_level(mode);
	mm_dump_table_recursive(table, max_level, max_level);
}

/**
 * Defragments the given page table by converting page table references to
 * blocks whenever possible.
 */
void mm_ptable_defrag(struct mm_ptable *t, int mode)
{
	/* TODO: Implement. */
	(void)t;
	(void)mode;
}

/**
 * Unmaps the hypervisor pages from the given page table.
 */
bool mm_ptable_unmap_hypervisor(struct mm_ptable *t, int mode)
{
	/* TODO: If we add pages dynamically, they must be included here too. */
	return mm_ptable_unmap(t, va_init((uintvaddr_t)text_begin),
			       va_init((uintvaddr_t)text_end), mode) &&
	       mm_ptable_unmap(t, va_init((uintvaddr_t)rodata_begin),
			       va_init((uintvaddr_t)rodata_end), mode) &&
	       mm_ptable_unmap(t, va_init((uintvaddr_t)data_begin),
			       va_init((uintvaddr_t)data_end), mode);
}

/**
 * Determines if the given virtual address is mapped in the given page table
 * by recursively traversing all levels of the page table.
 */
static bool mm_is_mapped_recursive(const pte_t *table, vaddr_t addr, int level)
{
	pte_t pte;
	uintvaddr_t va_level_end = va_addr(mm_level_end(addr, level));

	/* It isn't mapped if it doesn't fit in the table. */
	if (va_addr(addr) >= va_level_end) {
		return false;
	}

	pte = table[mm_index(addr, level)];

	if (level == 0) {
		return arch_mm_pte_is_present(pte);
	}

	if (arch_mm_is_block_allowed(level) && arch_mm_pte_is_block(pte)) {
		return true;
	}

	if (arch_mm_pte_is_table(pte)) {
		return mm_is_mapped_recursive(arch_mm_pte_to_table(pte), addr,
					      level - 1);
	}

	return false;
}

/**
 * Determines if the given virtual address is mapped in the given page table.
 */
bool mm_ptable_is_mapped(struct mm_ptable *t, vaddr_t addr, int mode)
{
	pte_t *table = ptr_from_va(va_from_pa(t->table));
	int level = arch_mm_max_level(mode);

	addr = arch_mm_clear_va(addr);

	return mm_is_mapped_recursive(table, addr, level);
}

/**
 * Initialises the given page table.
 */
bool mm_ptable_init(struct mm_ptable *t, uint32_t id, int mode)
{
	size_t i;
	pte_t *table;

	if (mode & MM_MODE_NOSYNC) {
		table = halloc_aligned_nosync(PAGE_SIZE, PAGE_SIZE);
	} else {
		table = halloc_aligned(PAGE_SIZE, PAGE_SIZE);
	}

	if (!table) {
		return false;
	}

	for (i = 0; i < PAGE_SIZE / sizeof(pte_t); i++) {
		table[i] = arch_mm_absent_pte();
	}

	/* TODO: halloc could return a virtual or physical address if mm not
	 * enabled? */
	t->table = pa_init((uintpaddr_t)table);
	t->id = id;

	return true;
}

/**
 * Updates the hypervisor page table such that the given virtual address range
 * is mapped to the corresponding physical address range in the
 * architecture-agnostic mode provided.
 */
bool mm_identity_map(vaddr_t begin, vaddr_t end, int mode)
{
	return mm_ptable_identity_map(&ptable, begin, end,
				      mode | MM_MODE_STAGE1);
}

/**
 * Updates the hypervisor table such that the given virtual address range is not
 * mapped to any physical address.
 */
bool mm_unmap(vaddr_t begin, vaddr_t end, int mode)
{
	return mm_ptable_unmap(&ptable, begin, end, mode | MM_MODE_STAGE1);
}

/**
 * Initialises memory management for the hypervisor itself.
 */
bool mm_init(void)
{
	dlog("text: 0x%x - 0x%x\n", text_begin, text_end);
	dlog("rodata: 0x%x - 0x%x\n", rodata_begin, rodata_end);
	dlog("data: 0x%x - 0x%x\n", data_begin, data_end);

	if (!mm_ptable_init(&ptable, 0, MM_MODE_NOSYNC | MM_MODE_STAGE1)) {
		dlog("Unable to allocate memory for page table.\n");
		return false;
	}

	/* Map page for uart. */
	/* TODO: We may not want to map this. */
	mm_ptable_identity_map_page(&ptable, va_init(PL011_BASE),
				    MM_MODE_R | MM_MODE_W | MM_MODE_D |
					    MM_MODE_NOSYNC | MM_MODE_STAGE1);

	/* Map each section. */
	mm_identity_map(va_init((uintvaddr_t)text_begin),
			va_init((uintvaddr_t)text_end),
			MM_MODE_X | MM_MODE_NOSYNC);

	mm_identity_map(va_init((uintvaddr_t)rodata_begin),
			va_init((uintvaddr_t)rodata_end),
			MM_MODE_R | MM_MODE_NOSYNC);

	mm_identity_map(va_init((uintvaddr_t)data_begin),
			va_init((uintvaddr_t)data_end),
			MM_MODE_R | MM_MODE_W | MM_MODE_NOSYNC);

	return arch_mm_init(ptable.table, true);
}

bool mm_cpu_init(void)
{
	return arch_mm_init(ptable.table, false);
}

/**
 * Defragments the hypervisor page table.
 */
void mm_defrag(void)
{
	mm_ptable_defrag(&ptable, MM_MODE_STAGE1);
}