//HELLO
//Simple program to test libfelix

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use libfelix::{println, syscall};
use libfelix::syscall::{open, write};

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    unsafe {
        let data = "helloo000o".as_ptr();
        write(0, data, 11);
    };
    loop {

    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
