//HELLO
//Simple program to test libfelix

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;



#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    unsafe { asm!("nop") };
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
