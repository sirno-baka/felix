//HELLO
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use libfelix::syscall::{write};

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    for i in 0..90000000 {

    }
    // === Самый надёжный способ сейчас ===
    let text = b"123!\n";

    let mut v = alloc::vec::Vec::with_capacity(4);
    v.extend_from_slice(text);

    unsafe { write(0, v.as_ptr(), v.len()); }


    // === Или через String (через into_bytes — форсирует реальный heap) ===
    let s = String::from("Hello from String via into_bytes!");
    let bytes = s.into_bytes();
    unsafe { write(0, bytes.as_ptr(), bytes.len()); }
    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}