/*
 * Copyright 2018 The Hafnium Authors.
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

#pragma once

/* Define the standard types for the platform. */
#if defined(__linux__) && defined(__KERNEL__)

#include <linux/types.h>

typedef phys_addr_t hf_ipaddr_t;

#else

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef uintptr_t hf_ipaddr_t;

#endif

/** The ID of the primary VM which is responsible for scheduling. */
#define HF_PRIMARY_VM_ID 0

/* Invalid values for fields to indicate absence or errors. */
#define HF_INVALID_VM_ID 0xffffffff

/* Sleep value for an indefinite period of time. */
#define HF_SLEEP_INDEFINITE 0xffffffffffffff

/** The amount of data that can be sent to a mailbox. */
#define HF_MAILBOX_SIZE 4096

/** The number of virtual interrupt IDs which are supported. */
#define HF_NUM_INTIDS 64

/** Interrupt ID returned when there is no interrupt pending. */
#define HF_INVALID_INTID 0xffffffff

/** Interrupt ID indicating the mailbox is readable. */
#define HF_MAILBOX_READABLE_INTID 1

/** Interrupt ID indicating a mailbox is writable. */
#define HF_MAILBOX_WRITABLE_INTID 2

/** The virtual interrupt ID used for the virtual timer. */
#define HF_VIRTUAL_TIMER_INTID 3
