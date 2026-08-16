//! Minimal userspace runtime for Felix.
//!
//! Provides the real ELF entry point (`_start`) and calls the user's `main`.
//! Also owns the panic handler so applications don't need to define one.

use core::panic::PanicInfo;

/// Real entry point of every userspace program.
/// The linker places this in the `.start` section (must be the first code section).
#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    // Later: parse argc/argv/envp from the user stack here.
    // For now we just call main with no arguments.

    extern "C" {
        fn main() -> i32;
    }

    let _code = unsafe { main() };

    // Always terminate the task when main returns.
    unsafe { crate::syscall::exit() }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Best-effort print; may fail if the heap / console is broken.
    crate::println!("panic: {}", info);
    unsafe { crate::syscall::exit() }
}
