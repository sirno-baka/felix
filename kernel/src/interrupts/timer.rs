// TIMER INTERRUPT HANDLER
// Triggers the scheduler and performs context switching

use crate::drivers::pic::PICS;
use crate::multitasking::task::{CPUState, TASK_MANAGER};
use core::arch::asm;
use crate::println;

pub const TIMER_INT: u8 = 32;

/// Naked interrupt handler for timer (IRQ0)
#[naked]
pub extern "C" fn timer() {
    unsafe {
        asm!(
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
        // Если CS.RPL == 3 — грузим DS/ES user data
        "mov ax, [esp + 4]",   // CS
        "and ax, 3",
        "cmp ax, 3",
        "jne 2f",
        "mov ax, 0x23",        // user data (как SS)
        "mov ds, ax",
        "mov es, ax",
        "2:",

        "iretd",
        options(noreturn)
        );
    }
}

/// Called from assembly. Returns new stack pointer (new task's CPUState)
#[no_mangle]
pub extern "C" fn timer_handler(esp: u32) -> u32 {
    unsafe {
        let new_esp = TASK_MANAGER.schedule(esp as *mut CPUState) as u32;
        // println!("TH");
        PICS.end_interrupt(TIMER_INT);

        new_esp
    }
}
