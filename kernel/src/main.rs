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
mod shell;
mod syscalls;
pub mod print;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::panic::PanicInfo;
use spin::Mutex;
use drivers::disk::DISK;
use drivers::pic::PICS;
use interrupts::idt::IDT;
use memory::paging::PAGING;
use shell::shell::SHELL;
use print::PRINTER;
use filesystem::ext2::Ext2;

use multitasking::task::TASK_MANAGER;
use crate::drivers::disk::DISK_SLAVE;
use crate::filesystem::vfs::Vfs;

//1MiB. TODO: Get those from linker
const KERNEL_START: u32 = 0x0010_0000;
const KERNEL_SIZE: u32 = 0x0010_0000;
const STACK_SIZE: u32 = 0x0010_0000;

const STACK_START: u32 = KERNEL_START + KERNEL_SIZE + STACK_SIZE;

const VERSION: &str = env!("CARGO_PKG_VERSION");

//KERNEL ENTRY POINT
#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    unsafe {
        //setup stack
        asm!("mov esp, {}", in(reg) STACK_START);

        //setup paging
        PAGING.identity();
        PAGING.enable();

        //bochs magic breakpoint
        asm!("xchg bx, bx");

        //setup idt
        IDT.init(); //init idt  
        // IDT.add_exceptions(); //add CPU exceptions to idt
        IDT.add(
            interrupts::timer::TIMER_INT as usize,
            interrupts::timer::timer as u32,
        ); //add timer interrupt to idt     
        IDT.add(
            syscalls::handler::SYSCALL_INT as usize,
            syscalls::handler::syscall as u32,
        ); //add system call handler interrupt     
        IDT.add(
            drivers::keyboard::KEYBOARD_INT as usize,
            drivers::keyboard::keyboard as u32,
        ); //add keyboard interrupt to idt   
        IDT.load(); //load idt

        //init programmable interrupt controllers
        PICS.init();
        let mut vfs = Vfs::new();
        //enable ata disk if present
        // DISK.check();
        // if DISK.enabled {
        //     DISK.check();
        //     if DISK.enabled {
        //         let mut ext2 = Ext2::new(&mut DISK);
        //         ext2.mount();
        //         vfs.set_root(Box::new(ext2));
        //     }
        //
        //      // clone нужен, потому что lock держит mutable
        // }
        DISK_SLAVE.check();
        let filename = "/test";

        if DISK_SLAVE.enabled {
        // Монтируем EXT2
            if DISK_SLAVE.enabled {
                let mut ext2 = Ext2::new(&mut DISK_SLAVE);
                ext2.mount();
                vfs.set_root(Box::new(ext2));
            }
        }
        *crate::filesystem::VFS.lock() = Some(vfs);
        if let Some(vfs) = crate::filesystem::VFS.lock().as_ref() {  // ← as_ref
            let data:Vec<u8> = vec![1,2,3,4,5];
            let success = vfs.write_file(filename, data.as_slice());
            if success {
                println!("Written to");
            }
        }

        if let Some(vfs) = crate::filesystem::VFS.lock().as_ref() {  // ← as_ref
            let success = vfs.read_file(filename);
            if success.is_some() {
                println!("Written to {:?}", success.unwrap());
            }
        }

        if let Some(vfs) = crate::filesystem::VFS.lock().as_ref() {  // ← as_ref
            vfs.list_directory("/");
        }
        println!("[VFS] Virtual filesystem initialized successfully");



        // //init filesystem
        // if DISK.enabled {
        //     let fat = FAT.acquire_mut();
        //     fat.load_header();
        //     fat.load_table();
        //     fat.load_entries();
        //     FAT.free();
        // }

        //print name, version and copyright
        print_info();

        // DISK_SLAVE.check();
        // if DISK_SLAVE.enabled {
        //     let mut ext2 = Ext2::new(&mut DISK_SLAVE);
        //     ext2.mount();
        //
        //     *crate::filesystem::EXT2_SLAVE.lock() = Some(ext2);
        //     println!("[EXT2] Slave filesystem mounted via Mutex");
        // }

        //init felix shell
        SHELL.init();

        //init multitasking
        TASK_MANAGER.init();

        //bochs magic breakpoint
        asm!("xchg bx, bx");

        //enable hardware interrupts
        asm!("sti");

        loop {}
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

    println!("\nSystem halted (press Ctrl+C or reset button)");
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

fn print_info() {
    unsafe {
        PRINTER.set_colors(0xf, 0);
    }


    unsafe {
        PRINTER.reset_colors();
    }
}
