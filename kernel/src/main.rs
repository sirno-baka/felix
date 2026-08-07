#![no_std]
#![no_main]
#![feature(naked_functions)]
#![feature(pointer_byte_offsets)]
#![feature(unsize)]
#![feature(coerce_unsized)]
#![feature(inline_const)]
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
mod pci;
mod time;
mod io;
mod disk;

use alloc::boxed::Box;
use alloc::sync::Arc;
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
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::drivers::pic::wait;
// use crate::drivers::ramfs::RamFs;
use crate::filesystem::VFS;
use crate::filesystem::vfs::Vfs;
use crate::io::{inb, outb};
use crate::pci::ide::IDE;
use crate::sync::mutex::Mutex;
use crate::utils::queue::Queue;
static mut TEST_WRITE: [u32; 128] = [0; 128];
static mut TEST_READ:  [u32; 128] = [0; 128];
const KERNEL_START: u32 = 0xC000_0000;
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
        let mut mask: u8;
        asm!("in al, 0x21", out("al") mask);     // читаем текущую маску master PIC
        asm!("out 0x21, al", in("al") mask | 1); // устанавливаем бит 0 (IRQ0 = timer)
        // 1. GDT + TSS
        gdt::GlobalDescriptorTable::init();
        GDT.set_kernel_stack(STACK_START);
        GDT.load();
        GDT.load_tss();

        // 2. Paging
        {
            let mut pm = PAGING.lock();
            pm.init(STACK_START as u32);

            // setup_kernel_page_dir больше не нужен — init() уже всё сделал
            let end_page = pm.next_free_page;
            let pd_phys = pm.dir_phys();

            unsafe {
                crate::memory::paging::KERNEL_END_PAGE = end_page;
                crate::memory::paging::KERNEL_PD_PHYS = pd_phys;
            }
        }

        // 3. IDT — загружаем ОЧЕНЬ РАНО
        IDT.init();
        IDT.add_exceptions();
        IDT.add(
            interrupts::timer::TIMER_INT as usize,
            interrupts::timer::timer as u32,
        );
        IDT.add_user_interrupt(
            syscalls::handler::SYSCALL_INT as usize,
            syscalls::handler::syscall as u32,
        );
        IDT.add(
            drivers::keyboard::KEYBOARD_INT as usize,
            drivers::keyboard::keyboard as u32,
        );
        IDT.load();                     // ← ПЕРЕМЕСТИТЬ СЮДА

        // 4. PIC
        PICS.init();

        // 5. Keyboard buffer
        *KEYBOARD_BUFFER.lock() = Some(Queue::new());

        // IDE init
        IDE.lock().initialize().expect("Cannot read from disks");

        let first = IDE.lock().get_device(0).unwrap();
        let mut ext2 = Ext2::new(first.clone(), None);
        //Ext2::format_gb(first, 0, 64, 4096);
        ext2.mount(None);
        VFS.get().set_root(Box::new(ext2));

        // 6. Диск + VFS
        // DISK.check();
        // let config = DISK.find_ext2_partition_config();
        // if DISK.enabled {
        //     let mut ext2 = Ext2::new(&mut DISK, Some(config));
        //     ext2.mount(None);
        //     VFS.get().set_root(Box::new(ext2));
        // }
        // let ram_fs = Box::new(RamFs::new());
        // VFS.get().set_root(ram_fs);
        // if DISK.enabled {
        //     let test_lba = 100; // Безопасный LBA, не трогаем MBR
        //
        //     unsafe {
        //         // заполняем
        //         for i in 0..128 {
        //             TEST_WRITE[i] = 0xDEADBEEF + i as u32;
        //         }
        //         println!("read_buf addr = {:p}", TEST_READ.as_ptr());
        //         println!("[TEST] write LBA {}...", test_lba);
        //         DISK.write(TEST_WRITE.as_ptr(), test_lba, 1);
        //
        //         println!("[TEST] read  LBA {}...", test_lba);
        //         DISK.read(TEST_READ.as_mut_ptr(), test_lba, 1);
        //
        //         // сравнение
        //         let mut ok = true;
        //         for i in 0..128 {
        //             if TEST_WRITE[i] != TEST_READ[i] {
        //                 println!("[TEST] err index {}: wrote {:08X}, read {:08X}",
        //                          i, TEST_WRITE[i], TEST_READ[i]);
        //                 ok = false;
        //                 break;
        //             }
        //         }
        //
        //         if ok {
        //             println!("[TEST] OKK");
        //         } else {
        //             println!("[TEST] FAIL");
        //         }
        //     }

        // let mut fs = Ext2::format_gb(&mut DISK, 0, 1, 4096);
        // VFS.get().set_root(Box::new(fs));


        println!("[VFS] Virtual filesystem initialized");
        print_info();

        // 7. Task Manager (после IDT!)
        TASK_MANAGER.init();
        for i in 0..5000 {
            wait();
        }
        crate::syscalls::handler::sys_execve("/shell\0".as_ptr() as *const u8);


        // === ВКЛЮЧАЕМ ТАЙМЕР И ПРЕРЫВАНИЯ ТОЛЬКО В КОНЦЕ ===
        asm!("in al, 0x21", out("al") mask);
        asm!("out 0x21, al", in("al") mask & !1u8); // снимаем бит 0
        asm!("sti");
        // run!("/hello");
        // run!("/shell");
        loop {
            unsafe {
                asm!("hlt");        // даём CPU спать до следующего таймера
            }
        }
    }
}

unsafe fn exampletask1() {

    let mut shell = shell::shell::Shell::new();
    loop {
        shell.run();
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