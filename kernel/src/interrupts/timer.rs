// TIMER INTERRUPT HANDLER
// Triggers the scheduler and performs context switching

use crate::drivers::pic::PICS;
use crate::multitasking::task::{CPUState, TASK_MANAGER};
use crate::println;
use crate::time::{SYSTEM_FRACTION, jiffies};
use core::arch::asm;
use core::arch::naked_asm;

pub const TIMER_INT: u8 = 32;

// Как часто поллить сеть (в тиках таймера)
// При SYSTEM_FRACTION ≈ 1.0 (1 мс) → каждые 10 мс
const NET_POLL_EVERY: usize = 10;

static mut NET_POLL_COUNTER: usize = 0;

/// Naked interrupt handler for timer (IRQ0)
#[unsafe(naked)]
pub extern "C" fn timer() {
    unsafe {
        naked_asm!(
            "cli",
            "call jiffies_inc",
            "push ebp",
            "push edi",
            "push esi",
            "push edx",
            "push ecx",
            "push ebx",
            "push eax",
            "push esp",
            "call timer_handler",
            "add esp, 4",
            "mov esp, eax",
            "pop eax",
            "pop ebx",
            "pop ecx",
            "pop edx",
            "pop esi",
            "pop edi",
            "pop ebp",
            // На стеке: EIP, CS, EFLAGS, [ESP, SS]
            // ВАЖНО: не трогать EAX — там return value syscall
            // (если IRQ0 пришёл между sti и iretd в syscall path).
            // Раньше mov ax, 0x23 превращал ret=0 в ret=35.
            "mov cx, [esp + 4]", // CS
            "and cx, 3",
            "cmp cx, 3",
            "jne 2f",
            "mov cx, 0x23",
            "mov ds, cx",
            "mov es, cx",
            "2:",
            "iretd"
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn timer_handler(esp: u32) -> u32 {
    unsafe {
        // === 1. Сетевой полл (неблокирующий) ===
        NET_POLL_COUNTER += 1;
        if NET_POLL_COUNTER >= NET_POLL_EVERY {
            NET_POLL_COUNTER = 0;
            poll_network();
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
unsafe fn poll_network() {
    // Считаем текущее время в миллисекундах
    let timestamp_ms = (jiffies() as f64 * SYSTEM_FRACTION) as i64;

    // Пытаемся взять стек без блокировки
    if let Some(mut guard) = crate::net::stack::NET_STACK.try_lock() {
        if let Some(ref mut stack) = *guard {
            stack.poll(timestamp_ms);
        }
    }
    // Если лок занят (syscall как раз работает с сетью) — просто пропускаем этот тик.
    // Это нормально и безопасно.
}
