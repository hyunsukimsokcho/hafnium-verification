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

#include "hf/arch/cpu.h"
#include "hf/arch/vm/interrupts_gicv3.h"

#include "hf/dlog.h"
#include "hf/std.h"

#include "vmapi/hf/call.h"
#include "vmapi/hf/spci.h"

#include "hftest.h"
#include "primary_with_secondary.h"

/*
 * Secondary VM that sends messages in response to interrupts, and interrupts
 * itself when it receives a message.
 */

static void irq(void)
{
	uint32_t interrupt_id = hf_interrupt_get();
	char buffer[] = "Got IRQ xx.";
	int size = sizeof(buffer);
	dlog("secondary IRQ %d from current\n", interrupt_id);
	buffer[8] = '0' + interrupt_id / 10;
	buffer[9] = '0' + interrupt_id % 10;
	memcpy_s(SERVICE_SEND_BUFFER()->payload, SPCI_MSG_PAYLOAD_MAX, buffer,
		 size);
	spci_message_init(SERVICE_SEND_BUFFER(), size, HF_PRIMARY_VM_ID,
			  hf_vm_get_id());
	spci_msg_send(0);
	dlog("secondary IRQ %d ended\n", interrupt_id);
}

/**
 * Try to receive a message from the mailbox, blocking if necessary, and
 * retrying if interrupted.
 */
int32_t mailbox_receive_retry()
{
	int32_t received;

	do {
		received = spci_msg_recv(SPCI_MSG_RECV_BLOCK);
	} while (received == SPCI_INTERRUPTED);

	return received;
}

TEST_SERVICE(interruptible)
{
	spci_vm_id_t this_vm_id = hf_vm_get_id();
	struct spci_message *recv_buf = SERVICE_RECV_BUFFER();

	exception_setup(irq);
	hf_interrupt_enable(SELF_INTERRUPT_ID, true);
	hf_interrupt_enable(EXTERNAL_INTERRUPT_ID_A, true);
	hf_interrupt_enable(EXTERNAL_INTERRUPT_ID_B, true);
	arch_irq_enable();

	for (;;) {
		const char ping_message[] = "Ping";
		const char enable_message[] = "Enable interrupt C";

		mailbox_receive_retry();
		if (recv_buf->source_vm_id == HF_PRIMARY_VM_ID &&
		    recv_buf->length == sizeof(ping_message) &&
		    memcmp(recv_buf->payload, ping_message,
			   sizeof(ping_message)) == 0) {
			/* Interrupt ourselves */
			hf_interrupt_inject(this_vm_id, 0, SELF_INTERRUPT_ID);
		} else if (recv_buf->source_vm_id == HF_PRIMARY_VM_ID &&
			   recv_buf->length == sizeof(enable_message) &&
			   memcmp(recv_buf->payload, enable_message,
				  sizeof(enable_message)) == 0) {
			/* Enable interrupt ID C. */
			hf_interrupt_enable(EXTERNAL_INTERRUPT_ID_C, true);
		} else {
			dlog("Got unexpected message from VM %d, size %d.\n",
			     recv_buf->source_vm_id, recv_buf->length);
			FAIL("Unexpected message");
		}
		hf_mailbox_clear();
	}
}
