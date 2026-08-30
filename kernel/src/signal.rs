//! Minimal signal subsystem for Felix.
//!
//! Design goals:
//! - bitmask of pending signals per task (extensible to more signals)
//! - default actions now: terminate (like Unix for SIGINT/SIGTERM/...)
//! - later: per-task handlers, blocked mask, SIG_IGN, restart flags
//!
//! Signal numbers intentionally mirror Linux where practical.

use crate::filesystem::file::{FileDescriptor, PipeEnd};
use crate::multitasking::task::{CPUState, TASK_MANAGER};
use crate::net::SocketState;
use crate::println;

// ====================== Signal numbers ======================

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGTERM: u32 = 15;

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

/// Queue `sig` for task `slot`. Returns false if slot invalid / idle / zombie.
pub fn send_signal(slot: i8, sig: u32) -> bool {
    if slot <= 0 || sig == 0 || sig > 31 {
        return false;
    }
    unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            if t.zombie || !t.running {
                return false;
            }
            t.pending_signals |= sigbit(sig);
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
    unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            if t.zombie {
                return false;
            }
            t.pending_signals = 0;
            t.running = false;
            t.zombie = true;
            t.exit_code = 128 + (if sig == 0 { SIGKILL } else { sig }) as i32;
        } else {
            return false;
        }
    }
    close_task_fds(slot);
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
    let mut taken: [Option<FileDescriptor>; 64] = [None; 64];
    unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
            for i in 0..64 {
                taken[i] = t.fd_table.close(i);
            }
        }
    }
    for desc in taken.into_iter().flatten() {
        match desc {
            FileDescriptor::Pipe { pipe_id, end } => match end {
                PipeEnd::Read => crate::pipe::pipe_close_reader(pipe_id),
                PipeEnd::Write => crate::pipe::pipe_close_writer(pipe_id),
            },
            FileDescriptor::Socket { socket_id } => {
                let mut table = crate::net::SOCKET_TABLE.lock();
                if let Some(sock) = table.get_mut(socket_id) {
                    sock.state = SocketState::Closed;
                }
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
                    if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                        t.pending_signals &= !sigbit(sig);
                        t.running = false;
                        t.zombie = true;
                        t.exit_code = 128 + sig as i32;
                    }
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
                t.zombie = true;
                t.exit_code = 128 + sig as i32;
            }
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

const MAX_TASKS_GUARD: usize = 8;

fn lowest_sig(mask: u32) -> Option<u32> {
    if mask == 0 {
        return None;
    }
    Some(mask.trailing_zeros() + 1)
}
