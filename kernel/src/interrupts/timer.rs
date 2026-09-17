// TIMER INTERRUPT HANDLER
// Triggers the scheduler and performs context switching

use crate::drivers::pic::PICS;
use crate::multitasking::task::{CPUState, TASK_MANAGER};
use crate::println;
use crate::time::uptime_ms;
use core::arch::asm;
use core::arch::naked_asm;

pub const TIMER_INT: u8 = 32;

// Poll the network by elapsed time, not by IRQ count, so changing the PIT
// frequency does not change the networking cadence.
const NET_POLL_EVERY_MS: u64 = 10;
static mut LAST_NET_POLL_MS: u64 = 0;

/// Naked interrupt handler for timer (IRQ0)
#[unsafe(naked)]
pub extern "C" fn timer() {
    unsafe {
        naked_asm!(
            "cli",
            "push ebp",
            "push edi",
            "push esi",
            "push edx",
            "push ecx",
            "push ebx",
            "push eax",
            // All interrupted GPRs are now safe on the stack, so AX can be
            // used to establish the kernel data selectors for Rust code.
            "cld",
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            // jiffies_inc is a normal Rust function and may clobber caller-saved
            // registers; call it only after the interrupted context is saved.
            "call jiffies_inc",
            "push esp",
            "call timer_handler",
            "add esp, 4",
            "mov esp, eax",
            // CPUState layout at the selected task's ESP:
            // eax,ebx,ecx,edx,esi,edi,ebp,eip,cs,eflags,esp,ss.
            // Pick DS/ES BEFORE restoring EAX/ECX; the previous code changed
            // CX after pop and silently corrupted userspace on every tick.
            "mov ax, [esp + 32]",
            "and ax, 3",
            "cmp ax, 3",
            "jne 1f",
            "mov ax, 0x23",
            "jmp 2f",
            "1:",
            "mov ax, 0x10",
            "2:",
            "mov ds, ax",
            "mov es, ax",
            "pop eax",
            "pop ebx",
            "pop ecx",
            "pop edx",
            "pop esi",
            "pop edi",
            "pop ebp",
            "iretd"
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn timer_handler(esp: u32) -> u32 {
    unsafe {
        // main.rs writes complete legacy PIC masks shortly before STI. Restore
        // registered PCI INTx lines once, after the system has actually entered
        // normal interrupt-driven operation.
        crate::drivers::shared_irq::late_restore_once();

        // Audio bottom half. This is deliberately outside the PCI shared IRQ:
        // the hard IRQ only reads/acks status and records completion. Mixing and
        // DMA refill happen here. poll() uses try_lock, so it never blocks the
        // timer when a syscall currently owns the audio core.
        crate::drivers::audio::poll();

        // === 1. Сетевой полл (неблокирующий) ===
        let now_ms = uptime_ms();
        if now_ms.saturating_sub(LAST_NET_POLL_MS) >= NET_POLL_EVERY_MS {
            LAST_NET_POLL_MS = now_ms;
            poll_network(now_ms as i64);
        }

        // === 2. Планировщик ===
        let mut new_esp = TASK_MANAGER.schedule(esp as *mut CPUState) as u32;

        // === 3. Pending signals on the task about to run ===
        new_esp = crate::signal::deliver_pending(new_esp);

        // === 4. EOI ===
        PICS.end_interrupt(TIMER_INT);
        new_esp
    }
}

/// Безопасный полл из IRQ-контекста
unsafe fn poll_network(timestamp_ms: i64) {
    // Пытаемся взять стек без блокировки
    if let Some(mut guard) = crate::net::stack::NET_STACK.try_lock() {
        if let Some(ref mut stack) = *guard {
            stack.poll(timestamp_ms);
        }
    }
    // Если лок занят (syscall как раз работает с сетью) — просто пропускаем этот тик.
    // Это нормально и безопасно.
}
