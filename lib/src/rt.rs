//! Minimal userspace runtime for Felix.
//!
//! Provides the real ELF entry point (`_start`) and calls the user's `main`.
//! Also owns the panic handler so applications don't need to define one.

use core::arch::asm;
use core::ffi::CStr;
use core::panic::PanicInfo;

/// argc placed on the user stack by the kernel before jump to `_start`.
static mut ARGC: i32 = 0;
/// argv pointer (into the initial user stack).
static mut ARGV: *const *const u8 = core::ptr::null();

/// Number of command-line arguments (including argv[0]).
pub fn argc() -> usize {
    unsafe { ARGC.max(0) as usize }
}

/// Get argument `i` as a UTF-8 string, if present and valid.
pub fn arg(i: usize) -> Option<&'static str> {
    unsafe {
        if i >= ARGC as usize || ARGV.is_null() {
            return None;
        }
        let ptr = *ARGV.add(i);
        if ptr.is_null() {
            return None;
        }
        CStr::from_ptr(ptr as *const i8).to_str().ok()
    }
}

/// Iterate over all arguments.
pub fn args() -> ArgsIter {
    ArgsIter { index: 0 }
}

pub struct ArgsIter {
    index: usize,
}

impl Iterator for ArgsIter {
    type Item = &'static str;

    fn next(&mut self) -> Option<Self::Item> {
        let a = arg(self.index)?;
        self.index += 1;
        Some(a)
    }
}

/// Real entry point of every userspace program.
/// Kernel leaves the stack as:
/// ```text
///   [esp]     = argc
///   [esp+4]   = argv[0]
///   ...
///   [esp+4*argc] = NULL
///   [esp+4*(argc+1)] = NULL  (empty envp)
///   ... string data higher up ...
/// ```
#[no_mangle]
#[link_section = ".start"]
pub unsafe extern "C" fn _start() -> ! {
    // Naked-ish entry: read argc/argv from the initial stack frame
    // before any Rust prologue would mess things up. We use a thin
    // asm block that stores them into statics, then call main.
    asm!(
        "mov eax, dword ptr [esp]",
        "lea ecx, [esp + 4]",
        "mov dword ptr [{argc}], eax",
        "mov dword ptr [{argv}], ecx",
        argc = sym ARGC,
        argv = sym ARGV,
        options(nostack, preserves_flags),
    );

    extern "C" {
        fn main() -> i32;
    }

    let _code = main();
    crate::syscall::exit()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::println!("panic: {}", info);
    unsafe { crate::syscall::exit() }
}
