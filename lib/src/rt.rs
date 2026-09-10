//! Minimal userspace runtime for Felix.
//!
//! Provides the real ELF entry point (`_start`) and calls the user's `main`.
//! Also owns the panic handler so applications don't need to define one.

use core::arch::naked_asm;
use core::ffi::CStr;
use core::panic::PanicInfo;

/// argc placed on the user stack by the kernel before jump to `_start`.
static mut ARGC: i32 = 0;
/// argv pointer (into the initial user stack).
static mut ARGV: *const *const u8 = core::ptr::null();
/// envp pointer (NULL-terminated KEY=VALUE strings after argv).
static mut ENVP: *const *const u8 = core::ptr::null();

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

/// Return an exported environment variable by name.
pub fn env(name: &str) -> Option<&'static str> {
    envs().find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Iterate over exported `KEY=VALUE` entries.
pub fn envs() -> EnvIter {
    EnvIter { index: 0 }
}

pub struct EnvIter {
    index: usize,
}

impl Iterator for EnvIter {
    type Item = &'static str;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if ENVP.is_null() {
                return None;
            }
            let ptr = *ENVP.add(self.index);
            if ptr.is_null() {
                return None;
            }
            self.index += 1;
            CStr::from_ptr(ptr as *const i8).to_str().ok()
        }
    }
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
///   [esp+4*(argc+1)] = envp[0] / NULL
///   ... string data higher up ...
/// ```
///
/// This must be naked: a normal Rust function may push registers or create a
/// stack frame before inline asm runs, which would make `[esp]` no longer argc.
#[unsafe(naked)]
#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    unsafe {
        naked_asm!(
            "mov eax, dword ptr [esp]", // argc
            "lea ecx, [esp + 4]",       // argv
            "mov dword ptr [{argc}], eax",
            "mov dword ptr [{argv}], ecx",
            // Preserve the initial-stack pointers above, then give Rust a clean
            // ABI-aligned stack independent from argc/env string placement.
            "and esp, -16",
            "call {start}",
            // rust_start is divergent. Keep a safe fallback if that ever changes.
            "2:",
            "hlt",
            "jmp 2b",
            argc = sym ARGC,
            argv = sym ARGV,
            start = sym rust_start,
        );
    }
}

extern "C" fn rust_start() -> ! {
    unsafe {
        ARGC = ARGC.max(0);
        ENVP = ARGV.add(ARGC as usize + 1);
    }

    extern "C" {
        fn main() -> i32;
    }

    let code = unsafe { main() };
    unsafe { crate::syscall::exit_status(code) }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::println!("panic: {}", info);
    unsafe { crate::syscall::exit() }
}
