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
use crate::disk::interface::BlockDevice;
use crate::disk::{copy_sectors, PartitionConfig};
use crate::disk::ramdisk::RamDisk;
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::drivers::pic::wait;
use crate::filesystem::{Filesystem, VFS};
use crate::filesystem::vfs::Vfs;
use crate::io::{inb, outb};
use crate::pci::floppy::disk::Floppy;
use crate::pci::ide::IDE;
use crate::sync::mutex::Mutex;
use crate::utils::queue::Queue;
use crate::wrappers::{cli, sti};

static mut TEST_WRITE: [u32; 128] = [0; 128];
static mut TEST_READ:  [u32; 128] = [0; 128];

// ===================== HIGHER-HALF CONSTANTS =====================
// Physical load address (bootloader still puts kernel here)
pub const KERNEL_PHYS: u32 = 0x0100_0000;
// Virtual higher-half base
pub const KERNEL_OFFSET: u32 = 0xC000_0000;
pub const KERNEL_START: u32 = KERNEL_PHYS + KERNEL_OFFSET; // 0xC100_0000
pub const KERNEL_SIZE: u32  = 0x0010_0000;
pub const STACK_SIZE: u32   = 0x0010_0000;  // 4 МБ стека
pub const STACK_START: u32  = KERNEL_START + KERNEL_SIZE + STACK_SIZE; // ≈ 0xC160_0000

#[macro_export]
macro_rules! run {
    ($app:expr) => {
        unsafe {
            let path = concat!($app, "\0");
            crate::syscalls::handler::sys_execve(path.as_ptr() as *const u8);
        }
    };
}

pub extern "C" fn irq6() {
    unsafe {
        outb(0x20, 0x20);
    }
}

/// Early higher-half transition.
/// This runs while we are still executing from the physical address
/// (bootloader jumped to 0x01000000). We set up a temporary page directory
/// that identity-maps the low 32 MiB AND maps the higher-half window
/// 0xC0000000+ → physical 0x00000000+, enable paging, then jump to the
/// high-virtual version of the rest of the kernel.
#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    unsafe {
        // ---------------------------------------------------------------
        // 1. Build a temporary page directory + page tables in low memory
        //    We place them at a known physical location after the kernel
        //    image (or use a static and convert virt→phys).
        //    For simplicity we use a fixed physical address 0x00200000.
        // ---------------------------------------------------------------
        const TEMP_PD_PHYS: u32 = 0x0020_0000;
        const TEMP_PT0_PHYS: u32 = 0x0020_1000; // covers 0–4 MiB identity + higher
        // More PTs can be added if needed.

        // Zero PD
        let pd = TEMP_PD_PHYS as *mut u32;
        for i in 0..1024 {
            *pd.add(i) = 0;
        }

        // Identity map first 32 MiB with 4 MiB large pages (PSE)
        // Also map the same physical pages at 0xC0000000 + base
        for i in 0..8u32 {
            let phys = i * 0x400000;
            let flags = 0x83u32; // Present + Writable + Large page
            // Identity
            *pd.add(i as usize) = phys | flags;
            // Higher-half (PDE index for 0xC0000000 is 768)
            *pd.add(768 + i as usize) = phys | flags;
        }

        // Also map the recursive entry? optional for early

        // ---------------------------------------------------------------
        // 2. Enable PSE (4 MiB pages) and load CR3 / enable PG
        // ---------------------------------------------------------------
        let mut cr4: u32;
        asm!("mov {}, cr4", out(reg) cr4);
        cr4 |= 1 << 4; // PSE
        asm!("mov cr4, {}", in(reg) cr4);

        asm!("mov cr3, {}", in(reg) TEMP_PD_PHYS);

        let mut cr0: u32;
        asm!("mov {}, cr0", out(reg) cr0);
        cr0 |= 1 << 31; // PG
        asm!("mov cr0, {}", in(reg) cr0);

        // ---------------------------------------------------------------
        // 3. Far jump to the higher-half entry point
        //    The symbol higher_half_entry is linked at high VMA.
        // ---------------------------------------------------------------
        asm!(
        "lea {0}, {1}",          // load high address of label
        "jmp {0}",
        out(reg) _,
        sym higher_half_entry,
        );
        loop {

        }
    }
}

const POPUG: [u8; 1327] = [0, 1, 1, 0, 0, 175, 0, 24, 0, 0, 0, 0, 28, 0, 28, 0, 8, 32, 0, 0, 0, 1, 9, 9, 11, 13, 14, 0, 22, 22, 0, 25, 26, 13, 17, 22, 19, 13, 13, 1, 35, 37, 13, 35, 36, 0, 47, 50, 15, 35, 53, 0, 51, 53, 0, 58, 60, 43, 37, 37, 4, 73, 76, 0, 76, 80, 2, 81, 85, 5, 87, 92, 29, 87, 90, 4, 95, 100, 17, 69, 119, 0, 96, 100, 5, 112, 118, 51, 79, 105, 60, 82, 103, 76, 33, 29, 67, 91, 114, 122, 85, 82, 99, 101, 101, 29, 43, 233, 31, 39, 233, 29, 60, 234, 31, 50, 234, 33, 33, 232, 33, 57, 235, 3, 124, 130, 32, 90, 152, 47, 99, 151, 62, 109, 154, 49, 113, 167, 51, 121, 190, 23, 93, 238, 27, 71, 235, 20, 106, 235, 28, 105, 240, 27, 126, 240, 41, 122, 200, 83, 107, 130, 70, 121, 172, 64, 127, 203, 7, 129, 134, 2, 141, 147, 5, 144, 151, 3, 156, 163, 10, 154, 163, 14, 168, 174, 3, 172, 179, 6, 175, 184, 9, 171, 180, 24, 128, 243, 0, 188, 196, 22, 162, 196, 17, 191, 197, 11, 167, 236, 4, 176, 230, 28, 188, 243, 46, 135, 206, 39, 133, 222, 63, 135, 202, 60, 141, 221, 55, 136, 215, 49, 152, 207, 42, 141, 235, 39, 136, 228, 38, 150, 237, 59, 142, 224, 37, 179, 213, 44, 172, 245, 3, 196, 205, 9, 196, 206, 1, 207, 216, 14, 204, 212, 9, 201, 212, 0, 210, 220, 13, 210, 220, 17, 200, 207, 16, 201, 209, 3, 200, 230, 1, 219, 229, 4, 222, 233, 13, 214, 225, 11, 217, 228, 10, 221, 233, 3, 214, 225, 16, 207, 226, 16, 216, 227, 5, 214, 249, 0, 226, 227, 2, 226, 235, 12, 225, 237, 15, 234, 238, 5, 229, 241, 2, 233, 244, 2, 237, 249, 11, 229, 242, 11, 233, 245, 11, 237, 250, 1, 241, 252, 13, 240, 253, 16, 231, 235, 19, 238, 236, 16, 239, 251, 26, 236, 248, 19, 240, 252, 30, 241, 252, 45, 209, 244, 38, 199, 243, 35, 234, 244, 47, 241, 252, 51, 232, 237, 50, 233, 243, 57, 239, 249, 92, 143, 145, 67, 182, 188, 76, 138, 199, 72, 142, 208, 65, 142, 219, 79, 145, 202, 83, 157, 231, 86, 166, 242, 110, 166, 221, 112, 170, 211, 125, 183, 241, 74, 237, 246, 73, 245, 249, 92, 234, 236, 94, 236, 244, 87, 237, 248, 92, 244, 249, 85, 242, 250, 104, 204, 221, 108, 238, 244, 106, 240, 247, 106, 241, 249, 98, 242, 251, 115, 233, 241, 127, 234, 240, 116, 240, 247, 114, 241, 248, 129, 108, 106, 139, 99, 96, 144, 155, 155, 169, 182, 182, 141, 237, 242, 149, 239, 244, 166, 214, 227, 201, 199, 199, 193, 220, 221, 202, 223, 224, 195, 223, 226, 204, 224, 225, 199, 229, 230, 199, 235, 243, 223, 231, 229, 217, 231, 232, 214, 236, 240, 223, 246, 247, 213, 246, 246, 225, 242, 243, 225, 247, 248, 228, 252, 253, 238, 255, 255, 246, 255, 255, 254, 254, 254, 246, 246, 246, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 53, 35, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 56, 103, 91, 52, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 12, 1, 16, 107, 88, 84, 92, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 21, 103, 54, 35, 102, 101, 90, 90, 35, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 12, 98, 89, 57, 98, 107, 89, 90, 58, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 55, 103, 91, 103, 107, 91, 90, 58, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 80, 98, 101, 107, 90, 90, 35, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 14, 51, 83, 105, 92, 101, 84, 90, 51, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 18, 123, 136, 139, 114, 107, 103, 103, 102, 99, 89, 91, 79, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 151, 152, 122, 145, 154, 141, 144, 148, 144, 113, 103, 108, 108, 108, 108, 106, 99, 91, 21, 0, 0, 0, 0, 0, 0, 0, 0, 25, 152, 167, 156, 170, 168, 157, 146, 143, 147, 134, 108, 108, 108, 108, 108, 108, 108, 99, 86, 7, 0, 0, 0, 0, 0, 0, 13, 149, 165, 149, 150, 173, 166, 160, 161, 145, 138, 118, 108, 113, 108, 108, 108, 108, 108, 106, 104, 22, 0, 1, 0, 0, 1, 0, 28, 170, 167, 27, 156, 172, 166, 160, 158, 153, 120, 103, 106, 108, 108, 113, 108, 113, 108, 108, 105, 54, 0, 0, 0, 0, 0, 5, 26, 155, 162, 174, 172, 171, 169, 160, 161, 145, 117, 103, 108, 108, 108, 106, 108, 106, 108, 106, 106, 78, 1, 0, 0, 2, 47, 130, 126, 46, 127, 170, 172, 169, 164, 161, 145, 137, 121, 114, 106, 106, 108, 106, 105, 106, 106, 106, 108, 93, 3, 0, 0, 24, 132, 128, 70, 69, 46, 131, 165, 163, 155, 135, 134, 138, 143, 143, 133, 111, 106, 105, 106, 105, 105, 106, 108, 89, 4, 0, 0, 48, 48, 48, 124, 70, 75, 66, 124, 140, 119, 115, 77, 65, 99, 120, 142, 138, 111, 105, 105, 105, 105, 105, 106, 108, 11, 0, 0, 23, 129, 69, 125, 124, 40, 46, 66, 95, 116, 34, 29, 31, 64, 97, 92, 112, 111, 105, 105, 105, 105, 105, 106, 106, 11, 0, 0, 1, 37, 73, 72, 73, 46, 36, 61, 110, 45, 30, 31, 31, 63, 97, 88, 103, 106, 105, 104, 99, 104, 105, 106, 103, 11, 0, 0, 0, 0, 20, 67, 73, 72, 72, 94, 99, 44, 29, 31, 42, 87, 98, 101, 106, 106, 104, 92, 99, 99, 105, 106, 108, 15, 0, 0, 0, 0, 0, 10, 39, 124, 71, 90, 100, 59, 33, 32, 43, 88, 83, 104, 111, 104, 99, 109, 99, 99, 105, 106, 108, 15, 0, 0, 0, 0, 1, 0, 17, 76, 76, 90, 100, 96, 43, 43, 87, 88, 89, 106, 104, 99, 92, 92, 99, 99, 105, 108, 105, 9, 0, 0, 0, 0, 0, 0, 56, 91, 93, 84, 91, 107, 107, 107, 98, 80, 101, 106, 99, 92, 92, 95, 99, 99, 105, 108, 93, 3, 0, 0, 0, 0, 0, 4, 84, 90, 90, 91, 91, 99, 102, 107, 107, 98, 98, 104, 95, 95, 99, 95, 92, 99, 105, 106, 82, 1, 0, 0, 0, 0, 0, 16, 84, 84, 91, 91, 91, 91, 91, 99, 98, 92, 91, 90, 90, 90, 91, 91, 92, 92, 104, 106, 78, 1, 0, 0, 0, 1, 0, 50, 95, 84, 95, 95, 90, 91, 90, 84, 95, 90, 90, 90, 90, 90, 90, 91, 91, 104, 104, 106, 60, 0, 0, 0, 0, 0, 0, 55, 84, 90, 91, 90, 91, 91, 90, 91, 91, 91, 91, 91, 91, 91, 90, 90, 94, 91, 89, 103, 60, 0, 0, 0, 0, 0, 4, 79, 90, 92, 92, 84, 81, 84, 84, 95, 84, 84, 86, 84, 81, 86, 85, 86, 86, 86, 85, 84, 51, 0, 0];

/// Continues kernel initialisation after we are running in higher-half.
#[no_mangle]
pub extern "C" fn higher_half_entry() -> ! {
    unsafe {
        // Now ESP must be the high virtual stack
        asm!("mov esp, {}", in(reg) STACK_START);
        // let mut mask: u8;
        // asm!("in al, 0x21", out("al") mask);
        // asm!("out 0x21, al", in("al") mask | 1);

        // 1. GDT + TSS (addresses are now high)
        gdt::GlobalDescriptorTable::init();
        GDT.set_kernel_stack(STACK_START);
        GDT.load();
        GDT.load_tss();

        // 2. Full paging (replaces the temporary PD with the proper one)
        {
            let mut pm = PAGING.lock();
            pm.init(STACK_START);

            crate::memory::paging::KERNEL_END_PAGE = pm.next_free_page;
            crate::memory::paging::KERNEL_PD_PHYS = pm.dir_phys();
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
        IDT.add(
            6,
            irq6 as u32,
        );
        IDT.load();                     // ← ПЕРЕМЕСТИТЬ СЮДА
        // После полной инициализации paging

        // crate::drivers::framebuffer::init();
        // cli!();
        // if let Some(ref fb) = *crate::drivers::framebuffer::FRAMEBUFFER.lock() {
        //     // Пример: заливаем экран тёмно-синим
        //     fb.fill(0xff00ff);
        //
        //     // // Рисуем зелёный прямоугольник
        //     fb.fill_rect(100, 100, 200, 150, 0x00FF00);
        //     fb.fill_rect(300, 200, 30, 50, 0x11aa00);
        //     //
        //     // // Красный пиксель
        //     // fb.put_pixel(400, 300, 0xFF0000);
        // }
        // sti!();
        // 4. PIC
        PICS.init();

        // 5. Keyboard buffer
        *KEYBOARD_BUFFER.lock() = Some(Queue::new());

        // IDE init
        IDE.lock().initialize().expect("Cannot read from disks");

        // let first = IDE.lock().get_device(0).unwrap();
        // let mut ext2d = Ext2::new(first.clone(), None);
        // //Ext2::format_gb(first, 0, 64, 4096);
        // ext2d.mount(None);
        // let disk = Arc::new(spin::Mutex::new(floppy));
        // let mut superblock_buf = [0u8; 1024];
        // match disk.lock().read_sectors(2, 2113, superblock_buf.as_mut_ptr() as u32) {
        //     Ok(_) => {println!("{:02x?}", &superblock_buf);}
        //     Err(e) => { print!("err: {:02x?}", e);}
        // };
        // Буфер достаточного размера
        let mut rd = RamDisk::new();
        let disk = Arc::new(spin::Mutex::new(rd));
        let mut ext2 = Ext2::new(disk.clone(), None);
        ext2.mount(None);

        VFS.get().set_root(Box::new(ext2));

        println!("[VFS] Virtual filesystem initialized");
        print_info();
        // В main.rs после инициализации
        pci::print_devices();

        // Найти Ethernet
        if let Some(eth) = pci::find_device(0x8086, 0x1229) {  // типичный 8255x
            crate::println!("Found Intel Ethernet!");
            eth.enable_bus_mastering();

            if let Some(bar) = eth.get_bar(0) {
                crate::println!("BAR0 = {:#x}, size = {}", bar.address().unwrap_or(0), bar.size());
            }
        }
        // 7. Task Manager (после IDT!)
        TASK_MANAGER.init();
        for i in 0..5000 {
            wait();
        }
        // // === ВКЛЮЧАЕМ ТАЙМЕР И ПРЕРЫВАНИЯ ТОЛЬКО В КОНЦЕ ===
        // asm!("in al, 0x21", out("al") mask);
        // asm!("out 0x21, al", in("al") mask & !1u8); // снимаем бит 0

        // let entries = VFS.get().list_directory_entries("/");
        let data= VFS.get().read_file("/shell").unwrap();
        crate::syscalls::handler::sys_execve(data.as_ptr(), data.len());


        asm!("sti");


        // For brevity in this patch the remaining init is left as a TODO —
        // paste the original body after the paging block from the old _start.
        // The only change needed is that all addresses (STACK_START etc.) are
        // already the high ones.

        println!("[!] Higher-half kernel running at 0x{:08x}", higher_half_entry as u32);

        // Temporary halt so you can verify the jump worked
        loop {
            asm!("hlt");
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