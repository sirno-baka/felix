#![no_std]
#![no_main]
#![feature(naked_functions)]
#![feature(pointer_byte_offsets)]
#![feature(unsize)]
#![feature(coerce_unsized)]
extern crate alloc;

mod drivers;
mod filesystem;
mod interrupts;
mod memory;
mod multitasking;
mod print;
mod syscalls;
mod shell;
mod sync;
mod wrappers;

mod utils;
mod spin;

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
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::drivers::pic::wait;
use crate::filesystem::VFS;
use crate::filesystem::vfs::Vfs;
use crate::utils::queue::Queue;

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
        IDT.add_exceptions();
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


        //only keyboard

        unsafe {
            // asm!("out 0x21, al", in("al") 0xfd_u8);
        }

        *KEYBOARD_BUFFER.lock() = Some(Queue::new());

        DISK_SLAVE.check();

        if DISK_SLAVE.enabled {
            let mut ext2 = Ext2::new(&mut DISK_SLAVE);
            ext2.mount();
            VFS.get().set_root(Box::new(ext2));
        }


        println!("[VFS] Virtual filesystem initialized");

        print_info();
        TASK_MANAGER.init();

        // TASK_MANAGER.add_task(exampletask3 as u32);
        // TASK_MANAGER.add_task(exampletask2 as u32);
        TASK_MANAGER.add_task(exampletask1 as u32);

        // asm!("xchg bx, bx");
        asm!("sti");
        // let mut shell = shell::shell::Shell::new();
        // unsafe { shell.run(); }
        // shell.init();

        // VFS.get().list_directory_entries("/").map(|t| {t.iter().map(|x| { println!("{}", x.name)}).collect::<Vec<_>>()});
        loop {
        //     shell.process_input()
        }
    }
}

unsafe fn exampletask1() {
    let mut shell = shell::shell::Shell::new();
    loop {
        shell.run();
    }
}
fn exampletask2() {
    let mut counter = 0;
    loop {
        counter += 1;
        if counter % 1 == 0 {
            println!("[Task ONE] {}", counter);
            VFS.get().read_file("test");
        }
        for _ in 0..10_000_0 {
            wait();
        }

    }
}

fn exampletask3() {
    let mut counter = 0;
    loop {
        counter += 1;
        if counter % 1 == 0 {
            println!("[Task TWO] {}", counter);
            VFS.get().read_file("test");
        }
        for _ in 0..10_000_0 {
            wait();
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