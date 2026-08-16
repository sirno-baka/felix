// exceptions.rs
//
// User-mode faults (CS.RPL == 3) kill the current task and schedule away.
// Kernel-mode faults halt the system (real bug).
//
// Naked stubs always either:
//   - never return (kernel panic path), or
//   - switch to a fresh CPUState from schedule() which has no error-code
//     on the stack, so iretd layout matches the timer/exit path.

use core::arch::asm;
use crate::filesystem::file::{FileDescriptor, PipeEnd};
use crate::multitasking::task::{CPUState, TASK_MANAGER};
use crate::pipe;
use crate::println;
use crate::net::SOCKET_TABLE;

/// True if the interrupted code was in ring 3.
#[inline]
fn is_user_cs(cs: u32) -> bool {
    (cs & 3) == 3
}

fn close_descriptor(desc: FileDescriptor) {
    match desc {
        FileDescriptor::Socket { socket_id } => {
            SOCKET_TABLE.lock().free(socket_id);
        }
        FileDescriptor::Pipe { pipe_id, end } => match end {
            PipeEnd::Read => pipe::pipe_close_reader(pipe_id),
            PipeEnd::Write => pipe::pipe_close_writer(pipe_id),
        },
        _ => {}
    }
}

/// Mark current userspace task as zombie and switch to another task.
/// Mirrors `sys_exit` so pipes get EOF and the parent can `wait()`.
/// `exit_code` should be 128 + signal number (Unix convention).
fn kill_current_task(esp: u32, reason: &str, exit_code: i32) -> u32 {
    unsafe {
        let slot = TASK_MANAGER.get_current_slot() as usize;
        if slot == 0 {
            // Idle must never die this way
            println!("\n=== KERNEL BUG: fault in idle ({}) ===", reason);
            loop {
                asm!("hlt");
            }
        }

        if let Some(ref mut t) = TASK_MANAGER.tasks[slot] {
            let fds: alloc::vec::Vec<_> = t.fd_table.take_all().collect();
            for desc in fds {
                close_descriptor(desc);
            }
            t.running = false;
            t.zombie = true;
            t.exit_code = exit_code;
            t.pending_signals = 0;
        }

        if crate::signal::get_foreground() == slot as i8 {
            crate::signal::clear_foreground();
        }

        println!("[exc] task {} killed: {} (exit={})", slot, reason, exit_code);

        TASK_MANAGER.schedule(esp as *mut CPUState) as u32
    }
}

fn kernel_halt(name: &str, eip: u32, cs: u32, eflags: u32) -> ! {
    println!("\n=== KERNEL EXCEPTION: {} ===", name);
    println!("  EIP={:#x} CS={:#x} EFLAGS={:#b}", eip, cs, eflags);
    println!("System halted");
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

/// Shared path: user → kill+schedule, kernel → halt.
fn handle_fault(name: &str, sig_exit: i32, esp: u32, eip: u32, cs: u32, eflags: u32) -> u32 {
    if is_user_cs(cs) {
        kill_current_task(esp, name, sig_exit)
    } else {
        kernel_halt(name, eip, cs, eflags)
    }
}

// ==================== Naked stubs (timer-compatible layout) ====================
//
// Layout after pushes matches CPUState head so `mov esp, eax; pop…; iretd`
// works when handler returns a scheduled task's cpu_state_ptr.

macro_rules! exception_stub {
    ($naked_name:ident, $handler_name:ident) => {
        #[naked]
        pub extern "C" fn $naked_name() {
            unsafe {
                asm!(
                    "cli",
                    "push ebp",
                    "push edi",
                    "push esi",
                    "push edx",
                    "push ecx",
                    "push ebx",
                    "push eax",
                    "push esp",
                    "call {handler}",
                    "add esp, 4",
                    "mov esp, eax",
                    "pop eax",
                    "pop ebx",
                    "pop ecx",
                    "pop edx",
                    "pop esi",
                    "pop edi",
                    "pop ebp",
                    // If returning to user (CS.RPL==3), fix DS/ES like timer does
                    "mov cx, [esp + 4]",
                    "and cx, 3",
                    "cmp cx, 3",
                    "jne 2f",
                    "mov cx, 0x23",
                    "mov ds, cx",
                    "mov es, cx",
                    "2:",
                    "iretd",
                    handler = sym $handler_name,
                    options(noreturn)
                );
            }
        }
    };
}

// Stubs that the CPU pushes an error code for: discard it before the
// register save so the stack matches the no-error-code layout.
// (We never resume the faulting frame — only switch or halt.)
macro_rules! exception_stub_with_error_code {
    ($naked_name:ident, $handler_name:ident) => {
        #[naked]
        pub extern "C" fn $naked_name() {
            unsafe {
                asm!(
                    "cli",
                    // error code is already on stack; drop it so layout matches
                    "add esp, 4",
                    "push ebp",
                    "push edi",
                    "push esi",
                    "push edx",
                    "push ecx",
                    "push ebx",
                    "push eax",
                    "push esp",
                    "call {handler}",
                    "add esp, 4",
                    "mov esp, eax",
                    "pop eax",
                    "pop ebx",
                    "pop ecx",
                    "pop edx",
                    "pop esi",
                    "pop edi",
                    "pop ebp",
                    "mov cx, [esp + 4]",
                    "and cx, 3",
                    "cmp cx, 3",
                    "jne 2f",
                    "mov cx, 0x23",
                    "mov ds, cx",
                    "mov es, cx",
                    "2:",
                    "iretd",
                    handler = sym $handler_name,
                    options(noreturn)
                );
            }
        }
    };
}

// ---------- #DE Division Error (no error code) ----------
exception_stub!(div_error, div_error_handler);

#[no_mangle]
pub extern "C" fn div_error_handler(esp: u32) -> u32 {
    let state = unsafe { &*(esp as *const CPUState) };
    // Unix: SIGFPE → 128+8 = 136
    handle_fault("div_error", 136, esp, state.eip, state.cs, state.eflags)
}

// ---------- #UD Invalid Opcode (no error code) ----------
exception_stub!(invalid_opcode, invalid_opcode_handler);

#[no_mangle]
pub extern "C" fn invalid_opcode_handler(esp: u32) -> u32 {
    let state = unsafe { &*(esp as *const CPUState) };
    // SIGILL → 128+4 = 132
    handle_fault("invalid_opcode", 132, esp, state.eip, state.cs, state.eflags)
}

// ---------- #GP General Protection (error code) ----------
exception_stub_with_error_code!(general_protection_fault, general_protection_fault_handler);

#[no_mangle]
pub extern "C" fn general_protection_fault_handler(esp: u32) -> u32 {
    let state = unsafe { &*(esp as *const CPUState) };
    // SIGSEGV → 128+11 = 139
    handle_fault("general_protection_fault", 139, esp, state.eip, state.cs, state.eflags)
}

// ---------- #DF Double Fault (error code) ----------
exception_stub_with_error_code!(double_fault, double_fault_handler);

#[no_mangle]
pub extern "C" fn double_fault_handler(esp: u32) -> u32 {
    // Double fault is almost always fatal even from user context
    let state = unsafe { &*(esp as *const CPUState) };
    if is_user_cs(state.cs) {
        kill_current_task(esp, "double_fault", 139)
    } else {
        kernel_halt("double_fault", state.eip, state.cs, state.eflags)
    }
}

// ---------- generic (no error code) ----------
exception_stub!(generic_handler, generic_handler_handler);

#[no_mangle]
pub extern "C" fn generic_handler_handler(esp: u32) -> u32 {
    let state = unsafe { &*(esp as *const CPUState) };
    handle_fault("generic", 139, esp, state.eip, state.cs, state.eflags)
}

// ---------- #PF Page Fault (error code) ----------
exception_stub_with_error_code!(page_fault, page_fault_handler);

#[no_mangle]
pub extern "C" fn page_fault_handler(esp: u32) -> u32 {
    // After dropping error code + pushing regs, `esp` points at a CPUState-shaped frame.
    let state = unsafe { &*(esp as *const CPUState) };

    let cr2: u32;
    unsafe {
        asm!("mov {}, cr2", out(reg) cr2);
    }

    // error code was discarded in the stub; re-read from CR2 context only
    println!(
        "PAGE FAULT! CR2={:#x} EIP={:#x} CS={:#x}",
        cr2, state.eip, state.cs
    );

    if is_user_cs(state.cs) {
        // SIGSEGV → 128+11 = 139
        kill_current_task(esp, "page_fault", 139)
    } else {
        kernel_halt("page_fault", state.eip, state.cs, state.eflags)
    }
}
