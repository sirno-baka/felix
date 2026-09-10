//! Minimal signal subsystem for Felix.
//!
//! Design goals:
//! - bitmask of pending signals per task (extensible to more signals)
//! - default actions now: terminate (like Unix for SIGINT/SIGTERM/...)
//! - later: per-task handlers, blocked mask, SIG_IGN, restart flags
//!
//! Signal numbers intentionally mirror Linux where practical.

use crate::filesystem::file::{FileDescriptor, PipeEnd};
use crate::multitasking::task::{CPUState, TASK_MANAGER, MAX_TASKS};
use crate::net::SocketState;
use crate::println;

// ====================== Signal numbers ======================

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGTERM: u32 = 15;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;

pub const SIG_DFL: u32 = 0;
pub const SIG_IGN: u32 = 1;

/// Bit for signal number `sig` (1..=31).
#[inline]
pub const fn sigbit(sig: u32) -> u32 {
    if sig == 0 || sig > 31 {
        0
    } else {
        1u32 << (sig - 1)
    }
}

/// Signals whose default action is terminate (can grow over time).
const DEFAULT_TERMINATE: u32 =
    sigbit(SIGHUP) | sigbit(SIGINT) | sigbit(SIGQUIT) | sigbit(SIGKILL) | sigbit(SIGTERM);

// ====================== Send ======================

/// Queue `sig` for a live task. A stopped task may accumulate pending signals;
/// they are delivered after SIGCONT makes it runnable again. Stop/continue/kill
/// themselves are handled synchronously by sys_kill.
pub fn send_signal(slot: i8, sig: u32) -> bool {
    if slot <= 0 || sig == 0 || sig > 31 {
        return false;
    }
    unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            if t.zombie {
                return false;
            }
            t.pending_signals |= sigbit(sig);
            true
        } else {
            false
        }
    }
}

/// Stop a task without turning it into a zombie. Its CPU state and FDs remain
/// intact so the shell can later resume it with SIGCONT/`bg`/`fg`.
pub fn stop_task(slot: i8) -> bool {
    stop_task_with_signal(slot, SIGSTOP)
}

/// Stop and latch the transition for waitpid(WUNTRACED). The event remains
/// pending until the parent consumes it, even if SIGCONT arrives quickly.
pub fn stop_task_with_signal(slot: i8, sig: u32) -> bool {
    if slot <= 0 {
        return false;
    }
    unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            if t.zombie || t.stopped {
                return false;
            }
            t.running = false;
            t.stopped = true;
            t.stop_signal = sig;
            t.wait_stopped_pending = true;
            // POSIX: generating a stop signal discards a pending SIGCONT.
            t.pending_signals &= !(sigbit(sig) | sigbit(SIGCONT));
            println!("[signal] task {} stopped by {}", slot, sig);
            true
        } else {
            false
        }
    }
}

/// Resume a previously stopped task.
pub fn continue_task(slot: i8) -> bool {
    if slot <= 0 {
        return false;
    }
    unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            if t.zombie {
                return false;
            }
            let was_stopped = t.stopped;
            t.stopped = false;
            t.running = true;
            if was_stopped {
                t.wait_continued_pending = true;
            }
            // POSIX: SIGCONT discards pending job-control stop signals whether
            // or not SIGCONT itself is caught by a userspace handler.
            t.pending_signals &= !(sigbit(SIGCONT)
                | sigbit(SIGSTOP)
                | sigbit(SIGTSTP)
                | sigbit(SIGTTIN)
                | sigbit(SIGTTOU));
            println!("[signal] task {} continued", slot);
            true
        } else {
            false
        }
    }
}

/// Kill `slot` now. No handler, no ignore, scheduler will not pick it again.
pub fn force_kill(slot: i8, sig: u32) -> bool {
    if slot <= 0 {
        return false;
    }
    let dead_pid = unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            if t.zombie {
                return false;
            }
            let pid = t.pid;
            t.pending_signals = 0;
            t.running = false;
            t.stopped = false;
            t.zombie = true;
            t.term_signal = if sig == 0 { SIGKILL } else { sig };
            t.exit_code = 128 + t.term_signal as i32;
            pid
        } else {
            return false;
        }
    };
    unsafe { TASK_MANAGER.reparent_children_of(dead_pid); }
    close_task_fds(slot);
    crate::syscalls::wasm::clear_task_state(slot as usize);
    crate::drivers::wm::destroy_windows_of(slot);
    unsafe {
        TASK_MANAGER.reap_orphans();
    }
    println!("[signal] task {} force-killed ({})", slot, sig);
    true
}

/// Drop every fd of `slot` so pipes/sockets get EOF immediately.
fn close_task_fds(slot: i8) {
    if slot <= 0 {
        return;
    }
    let taken = unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            t.fd_table.take_all()
        } else {
            alloc::vec::Vec::new()
        }
    };
    for closed in taken {
        if !closed.last_open_ref {
            continue;
        }
        match closed.desc {
            FileDescriptor::Pipe { pipe_id, end } => match end {
                PipeEnd::Read => crate::pipe::pipe_close_reader(pipe_id),
                PipeEnd::Write => crate::pipe::pipe_close_writer(pipe_id),
            },
            FileDescriptor::Socket { socket_id } => {
                crate::net::SOCKET_TABLE.lock().free(socket_id);
            }
            FileDescriptor::Pty { pty_id, side } => {
                crate::tty::close_ref(pty_id, side);
            }
            _ => {}
        }
    }
}

// ====================== Delivery ======================

/// Apply default actions for any pending signals on the *current* task.
///
/// If the task must terminate, marks it zombie and switches away, returning
/// the new task's CPU-state pointer (same contract as `sys_exit`).
///
/// Call this after `schedule` (timer) and before returning to userspace
/// from a normal syscall path.
pub fn deliver_pending(esp: u32) -> u32 {
    unsafe {
        // Loop in case the newly scheduled task also has fatal signals.
        for _ in 0..MAX_TASKS_GUARD {
            let slot = TASK_MANAGER.get_current_slot();
            if slot <= 0 {
                return esp_or_current(esp);
            }

            let pending = match TASK_MANAGER.tasks[slot as usize].as_ref() {
                Some(t) if t.pending_signals != 0 && t.running && !t.zombie => t.pending_signals,
                _ => return esp_or_current(esp),
            };

            // Process pending signals one by one (lowest first)
            let mut remaining = pending;
            while remaining != 0 {
                let sig = lowest_sig(remaining).unwrap_or(1);
                remaining &= !sigbit(sig);

                let handler = match TASK_MANAGER.tasks[slot as usize].as_ref() {
                    Some(t) => t.signal_handlers[(sig - 1) as usize],
                    None => 0,
                };

                if handler == 1 {
                    // SIG_IGN — drop
                    if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                        t.pending_signals &= !sigbit(sig);
                    }
                    continue;
                }

                if handler != 0 {
                    // Custom handler (cdecl): push sig, then return addr
                    if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                        t.pending_signals &= !sigbit(sig);
                        let state = &mut *(esp as *mut CPUState);
                        let mut usp = state.esp;
                        usp = usp.wrapping_sub(4);
                        *(usp as *mut u32) = sig;
                        usp = usp.wrapping_sub(4);
                        *(usp as *mut u32) = state.eip;
                        state.esp = usp;
                        state.eip = handler;
                    }
                    return esp_or_current(esp);
                }

                // Default action
                if (DEFAULT_TERMINATE & sigbit(sig)) != 0 {
                    let mut dead_pid = -1;
                    if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                        dead_pid = t.pid;
                        t.pending_signals &= !sigbit(sig);
                        t.running = false;
                        t.stopped = false;
                        t.zombie = true;
                        t.term_signal = sig;
                        t.exit_code = 128 + sig as i32;
                    }
                    TASK_MANAGER.reparent_children_of(dead_pid);
                    close_task_fds(slot);
                    crate::syscalls::wasm::clear_task_state(slot as usize);
                    crate::drivers::wm::destroy_windows_of(slot);
                    println!("[signal] task {} killed by signal {}", slot, sig);
                    let new_esp = TASK_MANAGER.schedule(esp as *mut CPUState) as u32;
                    return deliver_pending_after_switch(new_esp);
                }

                // Unknown default: just clear
                if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                    t.pending_signals &= !sigbit(sig);
                }
            }
            return esp_or_current(esp);
        }
        esp_or_current(esp)
    }
}

/// After we already switched due to a fatal signal, only check the new
/// current task once more (avoid deep recursion).
fn deliver_pending_after_switch(esp: u32) -> u32 {
    unsafe {
        let slot = TASK_MANAGER.get_current_slot();
        if slot <= 0 {
            return esp;
        }
        let pending = match TASK_MANAGER.tasks[slot as usize].as_ref() {
            Some(t) if t.pending_signals != 0 && t.running && !t.zombie => t.pending_signals,
            _ => return esp,
        };
        let fatal = pending & DEFAULT_TERMINATE;
        if fatal != 0 {
            let sig = lowest_sig(fatal).unwrap_or(SIGINT);
            if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                t.pending_signals &= !fatal;
                t.running = false;
                t.stopped = false;
                t.zombie = true;
                t.term_signal = sig;
                t.exit_code = 128 + sig as i32;
            }
            close_task_fds(slot);
            crate::drivers::wm::destroy_windows_of(slot);
            return TASK_MANAGER.schedule(esp as *mut CPUState) as u32;
        }
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            t.pending_signals = 0;
        }
        esp
    }
}

fn esp_or_current(esp: u32) -> u32 {
    esp
}

const MAX_TASKS_GUARD: usize = MAX_TASKS as usize;

fn lowest_sig(mask: u32) -> Option<u32> {
    if mask == 0 {
        return None;
    }
    Some(mask.trailing_zeros() + 1)
}
