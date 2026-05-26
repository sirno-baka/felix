//HELLO
//Simple program to test libfelix

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use libfelix::{println, syscall};
use libfelix::syscall::{open, read, write};

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    unsafe {
        println!("Hello, world!");

    };
    loop {

    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
