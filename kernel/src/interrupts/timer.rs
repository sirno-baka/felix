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
            // Save registers (must match CPUState layout)
            "push ebp",
            "push edi",
            "push esi",
            "push edx",
            "push ecx",
            "push ebx",
            "push eax",
            // Pass current stack pointer to handler
            "push esp",
            "call timer_handler",
            "add esp, 4",        // cleanup pushed argument
            // Switch to new task's kernel stack
            "mov esp, eax",
            // Restore registers
            "pop eax",
            "pop ebx",
            "pop ecx",
            "pop edx",
            "pop esi",
            "pop edi",
            "pop ebp",
            "sti",
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
