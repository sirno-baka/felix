#![no_std]
#![no_main]
extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use core::str::FromStr;
use libfelix::syscall::write;


#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    // Статическая строка (для сравнения)
    let static_hello: &[u8] = b"HELLO";
    let mut v = Vec::with_capacity(32);
    for c in static_hello.iter(){
        v.push(*c)
    }

    unsafe {
        write(0, v.as_ptr(), v.len());
    }

    let mut s = String::with_capacity(32);
    for c in static_hello.iter(){
        s.push((*c) as char)
    }
    unsafe {
        write(0, s.as_ptr(), s.len());
    }

    let mut v = Vec::with_capacity(32);
    for c in static_hello.iter(){
        v.push(*c)
    }

    unsafe {
        write(0, v.as_ptr(), v.len());
    }

    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! { loop {} }