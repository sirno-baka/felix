#![no_std]
#![no_main]
#![feature(naked_functions)]
#![feature(pointer_byte_offsets)]

extern crate alloc;

mod drivers;
mod filesystem;
mod interrupts;
mod memory;
mod multitasking;
mod print;
mod syscalls;
mod shell;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::panic::PanicInfo;
use drivers::pic::PICS;
use interrupts::idt::IDT;
use memory::paging::PAGING;
use print::PRINTER;
use filesystem::ext2::Ext2;

use multitasking::task::TASK_MANAGER;
use crate::drivers::disk::DISK_SLAVE;
use crate::filesystem::vfs::Vfs;

const KERNEL_START: u32 = 0x0010_0000;
const KERNEL_SIZE: u32 = 0x0010_0000;
const STACK_SIZE: u32 = 0x0010_0000;

const STACK_START: u32 = KERNEL_START + KERNEL_SIZE + STACK_SIZE;

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    unsafe {
        asm!("mov esp, {}", in(reg) STACK_START);

        PAGING.identity();
        PAGING.enable();

        IDT.init();
        IDT.add(
            interrupts::timer::TIMER_INT as usize,
            interrupts::timer::timer as u32,
        ); //add timer interrupt to idt
        IDT.add(
            syscalls::handler::SYSCALL_INT as usize,
            syscalls::handler::syscall as u32,
        );
        IDT.add(
            drivers::keyboard::KEYBOARD_INT as usize,
            drivers::keyboard::keyboard as u32,
        );
        IDT.load();

        PICS.init();

        unsafe {
            asm!("out 0x21, al", in("al") 0xfd_u8);
        }

        let mut vfs = Vfs::new();
        DISK_SLAVE.check();

        if DISK_SLAVE.enabled {
            let mut ext2 = Ext2::new(&mut DISK_SLAVE);
            ext2.mount();
            vfs.set_root(Box::new(ext2));
        }

        *crate::filesystem::VFS.lock() = Some(vfs);
        if let Some(vfs) = crate::filesystem::VFS.lock().as_mut() {
            let data: Vec<u8> = vec![1, 2, 3, 4, 5];
            let success = vfs.write_file("/test", data.as_slice());
            if success {
                println!("Written to");
            }
        }

        if let Some(vfs) = crate::filesystem::VFS.lock().as_mut() {
            vfs.list_directory("/");
        }
        println!("[VFS] Virtual filesystem initialized");

        print_info();
        TASK_MANAGER.init();

        asm!("xchg bx, bx");
        asm!("sti");
        let mut shell = shell::shell::Shell::new();
        loop {
            shell.run()
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n\n=== KERNEL PANIC ===");
    println!("Panic message: {}", info);
    if let Some(location) = info.location() {
        println!("Location: {}:{}:{}",
                 location.file(), location.line(), location.column());
    }
    println!("System halted");
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

fn print_info() {
    let mut p = PRINTER.lock();
    p.set_colors(0xf, 0);
    p.reset_colors();
}