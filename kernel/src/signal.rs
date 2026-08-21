//! Minimal signal subsystem for Felix.
//!
//! Design goals:
//! - bitmask of pending signals per task (extensible to more signals)
//! - default actions now: terminate (like Unix for SIGINT/SIGTERM/...)
//! - later: per-task handlers, blocked mask, SIG_IGN, restart flags
//!
//! Signal numbers intentionally mirror Linux where practical.

use crate::multitasking::task::{CPUState, TASK_MANAGER};
use crate::println;

// ====================== Signal numbers ======================

pub const SIGHUP:  u32 = 1;
pub const SIGINT:  u32 = 2;
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

// ====================== Foreground task ======================

/// Slot of the process that receives terminal-generated signals (Ctrl+C).
/// `-1` = none (e.g. shell is at the prompt).
static mut FOREGROUND_TASK: i8 = -1;

pub fn set_foreground(slot: i8) {
    unsafe { FOREGROUND_TASK = slot; }
}

pub fn get_foreground() -> i8 {
    unsafe { FOREGROUND_TASK }
}

pub fn clear_foreground() {
    unsafe { FOREGROUND_TASK = -1; }
}

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

/// Send `sig` to the current foreground task.
/// Returns true if a task received it (caller should NOT put the key in stdin).
pub fn signal_foreground(sig: u32) -> bool {
    let fg = get_foreground();
    if fg < 0 {
        return false;
    }
    send_signal(fg, sig)
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

            // Fatal signals → terminate
            let fatal = pending & DEFAULT_TERMINATE;
            if fatal != 0 {
                // Prefer a well-known signum for exit status (lowest set bit)
                let sig = lowest_sig(fatal).unwrap_or(SIGINT);

                if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                    t.pending_signals &= !fatal;
                    t.running = false;
                    t.zombie = true;
                    t.exit_code = 128 + sig as i32;
                }

                if get_foreground() == slot {
                    clear_foreground();
                }

                println!("[signal] task {} killed by signal {}", slot, sig);

                let new_esp = TASK_MANAGER.schedule(esp as *mut CPUState) as u32;
                // Continue loop: maybe the next task also has pending signals
                let _ = new_esp;
                // Use the scheduled stack for further delivery / return
                return deliver_pending_after_switch(new_esp);
            }

            // Non-fatal pending bits: for now just clear (no handlers yet).
            // Later: invoke userspace handler here.
            if let Some(ref mut t) = TASK_MANAGER.tasks[slot as usize] {
                t.pending_signals = 0;
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
            if get_foreground() == slot {
                clear_foreground();
            }
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
