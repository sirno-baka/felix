//! Convenient signal API for userspace.

use crate::syscall::{self, SigAction, SIG_DFL, SIG_IGN};

pub use crate::syscall::{SIGHUP, SIGINT, SIGKILL, SIGQUIT, SIGTERM};

/// Handler type: called with the signal number.
pub type Handler = extern "C" fn(sig: u32);

/// Install a signal handler. Replaces previous action.
/// Returns previous handler address (0 = default, 1 = ignore).
pub fn on(sig: u32, handler: Handler) -> u32 {
    let act = SigAction {
        sa_handler: handler as u32,
        sa_mask: 0,
        sa_flags: 0,
    };
    let mut old = SigAction::default();
    unsafe {
        let _ = syscall::sigaction(sig, &act, &mut old);
    }
    old.sa_handler
}

/// Restore default action for `sig`.
pub fn default(sig: u32) {
    let act = SigAction {
        sa_handler: SIG_DFL,
        sa_mask: 0,
        sa_flags: 0,
    };
    unsafe {
        let _ = syscall::sigaction(sig, &act, core::ptr::null_mut());
    }
}

/// Ignore `sig`.
pub fn ignore(sig: u32) {
    let act = SigAction {
        sa_handler: SIG_IGN,
        sa_mask: 0,
        sa_flags: 0,
    };
    unsafe {
        let _ = syscall::sigaction(sig, &act, core::ptr::null_mut());
    }
}

/// Exit the process (via SYS_EXIT).
pub fn exit() -> ! {
    unsafe { syscall::exit() }
}

/// Convenience: on SIGINT/SIGTERM call `exit`.
pub fn exit_on_terminate() {
    on(SIGINT, exit_handler);
    on(SIGTERM, exit_handler);
}

extern "C" fn exit_handler(_sig: u32) {
    exit();
}
