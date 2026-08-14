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
use crate::wrappers::cli;

static mut TEST_WRITE: [u32; 128] = [0; 128];
static mut TEST_READ:  [u32; 128] = [0; 128];

// ===================== HIGHER-HALF CONSTANTS =====================
// Physical load address (bootloader still puts kernel here)
pub const KERNEL_PHYS: u32 = 0x0100_0000;
// Virtual higher-half base
pub const KERNEL_OFFSET: u32 = 0xC000_0000;
pub const KERNEL_START: u32 = KERNEL_PHYS + KERNEL_OFFSET; // 0xC100_0000
pub const KERNEL_SIZE: u32  = 0x0010_0000;
pub const STACK_SIZE: u32   = 0x0040_0000;  // 4 МБ стека
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

        crate::drivers::framebuffer::init();
        cli!();
        if let Some(ref fb) = *crate::drivers::framebuffer::FRAMEBUFFER.lock() {
            // Пример: заливаем экран тёмно-синим
            fb.fill(0xff00ff);

            // // Рисуем зелёный прямоугольник
            fb.fill_rect(100, 100, 200, 150, 0x00FF00);
            fb.fill_rect(300, 200, 30, 50, 0x11aa00);
            //
            // // Красный пиксель
            // fb.put_pixel(400, 300, 0xFF0000);
        }
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