//HELLO
//Simple program to test libfelix

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use libfelix::syscall;
use libfelix::syscall::write;

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    unsafe {
        loop {
            write(0, b"1".as_ptr(), 1);
        }
    };
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
