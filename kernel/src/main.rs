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
mod gdt;
mod tss;
mod utils;
mod spin;
mod elf;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::panic::PanicInfo;
use drivers::pic::PICS;
use gdt::{GDT, GlobalDescriptorTable};
use interrupts::idt::IDT;
use memory::paging::PAGING;
use print::PRINTER;
use filesystem::ext2::Ext2;

use multitasking::task::TASK_MANAGER;
use crate::drivers::disk::{DISK, DISK_SLAVE};
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::drivers::pic::wait;
use crate::filesystem::VFS;
use crate::filesystem::vfs::Vfs;
use crate::utils::queue::Queue;

const KERNEL_START: u32 = 0x0010_0000;
const KERNEL_SIZE: u32 = 0x0010_0000;
const STACK_SIZE: u32 = 0x0010_0000;

const STACK_START: u32 = KERNEL_START + KERNEL_SIZE + STACK_SIZE;

#[macro_export]
macro_rules! run {
    ($app:expr) => {
        unsafe {
            let path = concat!($app, "\0");
            crate::syscalls::handler::sys_execve(path.as_ptr() as *const u8);
        }
    };
}

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    unsafe {
        asm!("mov esp, {}", in(reg) STACK_START);

        // GDT setup
        gdt::GlobalDescriptorTable::init();
        GDT.set_kernel_stack(STACK_START);
        GDT.load();
        GDT.load_tss();           // ←←← ОБЯЗАТЕЛЬНО

        PAGING.lock().init(STACK_START as u32);
        IDT.init();
        IDT.add_exceptions();
        IDT.add(
            interrupts::timer::TIMER_INT as usize,
            interrupts::timer::timer as u32,
        ); //add timer interrupt to idt
        IDT.add_user_interrupt(
            syscalls::handler::SYSCALL_INT as usize,
            syscalls::handler::syscall as u32,
        );
        IDT.add(
            drivers::keyboard::KEYBOARD_INT as usize,
            drivers::keyboard::keyboard as u32,
        );
        IDT.load();

        PICS.init();
        // drivers::pit::set_period_ms(1000);
        *KEYBOARD_BUFFER.lock() = Some(Queue::new());

        DISK.check();
        let config = DISK.find_ext2_partition_config();

        if DISK.enabled {
            let mut ext2 = Ext2::new(&mut DISK, Some(config));
            ext2.mount(None);
            VFS.get().set_root(Box::new(ext2));
        }

        println!("[VFS] Virtual filesystem initialized");
        print_info();

        TASK_MANAGER.init();

        // TASK_MANAGER.add_task(exampletask3 as u32);
        // TASK_MANAGER.add_task(exampletask2 as u32);
        // let slot = unsafe { TASK_MANAGER.get_free_slot() };
        // const APP_TARGET: u32 = 0x40000000;
        // //0x40000000
        // //  0x400000
        // const APP_SIZE: u32 = 4 * 1024 * 1024; // 4 MiB на задачу
        // let target = APP_TARGET + (slot as u32 * APP_SIZE);
        // let user_stack_top = target + APP_SIZE - 0x2000; // 8 KiB стек

        // TASK_MANAGER.add_task(exampletask1 as u32, user_stack_top);

        // asm!("xchg bx, bx");
        asm!("sti");
        // let mut shell = shell::shell::Shell::new();
        // unsafe { shell.run(); }
        // shell.init();
        run!("/hello");
        // VFS.get().list_directory_entries("/").map(|t| {t.iter().map(|x| { println!("{}", x.name)}).collect::<Vec<_>>()});
        loop {
            // print!("2");
            // shell.process_input()
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
            // VFS.get().read_file("test");
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