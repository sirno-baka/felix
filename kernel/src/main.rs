#![no_std]
#![no_main]
#![feature(pointer_byte_offsets)]
#![allow(static_mut_refs)]
#![feature(unsize)]
#![feature(coerce_unsized)]
#![feature(inline_const)]
#![allow(warnings)]
extern crate alloc;

mod device;
mod disk;
mod drivers;
mod elf;
mod fb_panic;
mod filesystem;
mod gdt;
mod interrupts;
mod io;
mod memory;
mod multitasking;
mod net;
mod pci;
mod pipe;
mod pit;
mod print;
mod random;
mod shell;
mod signal;
mod smp;
mod spin;
mod sync;
mod syscalls;
mod time;
mod tss;
mod tty;
mod utils;
mod wrappers;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};
use core::str::FromStr;
use drivers::pic::PICS;
use filesystem::ext2::Ext2;
use gdt::{GDT, GlobalDescriptorTable};
use interrupts::idt::IDT;
use memory::paging::PAGING;
use print::PRINTER;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::socket::udp;
use smoltcp::time::Instant;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

use crate::device::char::{NullDevice, ZeroDevice};
use crate::disk::interface::BlockDevice;
use crate::disk::ramdisk::RamDisk;
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::drivers::net::i8255x::SCB_STATUS;
use crate::drivers::pcmcia;
use crate::drivers::pcmcia::PcmciaDevice;
use crate::filesystem::devfs::DevFS;
use crate::filesystem::fat32::{FatDisk, FatFs, find_fat_partition_config};
use crate::filesystem::init::init_usb;
use crate::filesystem::{Filesystem, VFS};

static SYSTEM_LOG_CURSOR: crate::sync::mutex::Mutex<u64> = crate::sync::mutex::Mutex::new(0);
static SYSTEM_LOG_READY: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static SYSTEM_LOG_LAST_FLUSH_MS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
use crate::io::outb;
use crate::pci::ide::{IDE, IDEDevice};
use crate::pci::print_devices;
use crate::pit::init;
use crate::sync::mutex::Mutex;
use crate::utils::queue::Queue;
use crate::wrappers::_cli;
use multitasking::task::TASK_MANAGER;

static mut TEST_WRITE: [u32; 128] = [0; 128];
static mut TEST_READ: [u32; 128] = [0; 128];

// ===================== HIGHER-HALF CONSTANTS =====================
// Physical load address (bootloader still puts kernel here)
pub const KERNEL_PHYS: u32 = 0x0100_0000;
// Virtual higher-half base
pub const KERNEL_OFFSET: u32 = 0xC000_0000;
pub const KERNEL_START: u32 = KERNEL_PHYS + KERNEL_OFFSET; // 0xC100_0000
pub const KERNEL_SIZE: u32 = 0x0010_0000;
pub const STACK_SIZE: u32 = 0x0040_0000; // 4 МБ стека: 0xC1200000..0xC1600000
pub const STACK_START: u32 = 0xC160_0000;
// Kernel heap: virt 0xC1800000..0xC2800000 → phys 0x01800000..0x02800000 (allocator.rs).
// Frame allocator starts at FRAME_ALLOC_START = 0x02800000 (must be past heap end).
pub const HEAP_END_VIRT: u32 = 0xC280_0000;

#[macro_export]
macro_rules! run {
    ($app:expr) => {
        unsafe {
            let path = concat!($app, "\0");
            // parent_slot 0 = idle; this macro is kernel-side only
            let _ = crate::syscalls::handler::sys_spawn(
                0,
                path.as_ptr() as *const u8,
                0,
                -1,
                -1,
                -1,
                &[],
                &[],
                -1,
                false,
            );
        }
    };
}

/// Early higher-half transition.
/// This runs while we are still executing from the physical address
/// (bootloader jumped to 0x01000000). We set up a temporary page directory
/// that identity-maps the low 32 MiB AND maps the higher-half window
/// 0xC0000000+ → physical 0x00000000+, enable paging, then jump to the
/// high-virtual version of the rest of the kernel.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".start")]
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

        // Early TEMP_PD: map 64 MiB so higher_half_entry + kernel heap are reachable.
        // Full RAM-sized window is installed later in PageManager::init after
        // detect_ram() / configure_from_ram() (cannot call those here: still at
        // physical VMA before the higher-half jump).
        for i in 0..16u32 {
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
        loop {}
    }
}
/// Continues kernel initialisation after we are running in higher-half.
#[unsafe(no_mangle)]
pub extern "C" fn higher_half_entry() -> ! {
    unsafe {
        // Now ESP must be the high virtual stack
        asm!("mov esp, {}", in(reg) STACK_START);
        // Configure x87 and, when CPUID allows it, FXSR/SSE context support on
        // the BSP before any Rust/kernel code has a chance to execute x87.
        crate::multitasking::task::init_cpu_fp();
        crate::memory::resources::init_cpu_pat_wc();
        // let mut mask: u8;
        // asm!("in al, 0x21", out("al") mask);
        // asm!("out 0x21, al", in("al") mask | 1);

        // 1. GDT + TSS (addresses are now high)
        gdt::GlobalDescriptorTable::init();
        GDT.set_kernel_stack(STACK_START);
        GDT.load();
        GDT.load_tss();

        // 2. Detect physical RAM (BootInfo / CMOS) → size large-page window,
        //    then install the definitive page directory.
        {
            let ram = crate::memory::paging::detect_ram();
            crate::memory::paging::configure_from_ram(ram);
            println!(
                "[mem] installed {} MiB, managed lowmem {} MiB → {} large pages",
                crate::memory::paging::installed_ram_mib(),
                crate::memory::paging::detected_ram_bytes() / (1024 * 1024),
                crate::memory::paging::large_page_count()
            );
            let mut pm = PAGING.lock();
            pm.init(HEAP_END_VIRT);

            crate::memory::paging::KERNEL_END_PAGE = pm.next_free_page;
            crate::memory::paging::KERNEL_PD_PHYS = pm.dir_phys();
        }

        if let Err(e) = crate::memory::resources::init() {
            println!("[mem] resource manager init error: {}", e);
        }

        // 3. IDT — загружаем ОЧЕНЬ РАНО
        IDT.init();
        IDT.add_exceptions();
        IDT.add(
            interrupts::timer::TIMER_INT as usize,
            interrupts::timer::timer as u32,
        );
        IDT.add(
            smp::AP_TIMER_VECTOR as usize,
            smp::ap_timer_interrupt as u32,
        );
        IDT.add(
            smp::WORK_IPI_VECTOR as usize,
            smp::work_ipi_interrupt as u32,
        );
        IDT.add(smp::TLB_IPI_VECTOR as usize, smp::tlb_ipi_interrupt as u32);
        IDT.add_user_interrupt(
            syscalls::handler::SYSCALL_INT as usize,
            syscalls::handler::syscall as u32,
        );
        IDT.add(
            drivers::keyboard::KEYBOARD_INT as usize,
            drivers::keyboard::keyboard as u32,
        );
        IDT.add(
            drivers::mouse::MOUSE_INT as usize,
            drivers::mouse::mouse_irq as u32,
        );
        IDT.load(); // ← ПЕРЕМЕСТИТЬ СЮДА
        // После полной инициализации paging

        // Bring up application processors after the final page tables and IDT
        // exist. APs use private stacks and remain in kernel idle for now.
        smp::init();

        // VESA framebuffer (mode set by bootloader, info at 0x5000).
        // After graphics mode VGA text is gone — software console + mini-WM.
        crate::drivers::framebuffer::init();

        match crate::drivers::intel_gen9::init_native_framebuffer() {
            Ok(true) => println!("[GEN9] native Intel framebuffer enabled"),
            Ok(false) => match crate::drivers::intel_snb::init_native_framebuffer() {
                Ok(true) => println!("[SNB] native Intel framebuffer enabled"),
                Ok(false) => match crate::drivers::ati_m6::init_native_lcd() {
                    Ok(()) => println!("[M6] native LCD mode enabled"),
                    Err(e) => println!("[M6] error: {}", e),
                },
                Err(e) => println!("[SNB] takeover skipped: {}", e),
            },
            Err(e) => println!("[GEN9] takeover skipped: {}", e),
        }

        crate::drivers::wm::init();
        // 4. PIC
        PICS.init();

        // Use nesting cli so KMutex unlock cannot sti mid-boot.
        crate::wrappers::_cli();
        // 5. Keyboard buffer + PS/2 mouse (after PIC, still IF=0)
        *KEYBOARD_BUFFER.lock() = Some(Queue::new());
        crate::drivers::mouse::init();

        // Real ATA disks → auto-detect ext2 / FAT, DevFS, network.
        IDE.lock().initialize().expect("Cannot probe IDE");

        if !filesystem::init::init_rootfs() {
            println!("[!] init_rootfs failed — no mountable disk");
            halt();
        }
        drivers::audio::init();
        init_usb();
        drivers::net::init_net();
        // ---------------------------------------------------------------
        // Launch userspace init while TIMER is still masked and IF=0.
        // PID 0 remains the kernel idle task; the first userspace PID is 1.
        // init owns service lifecycle and is responsible for spawning shell.
        // ---------------------------------------------------------------
        crate::wrappers::_cli();
        TASK_MANAGER.init();

        let data = match VFS.get().read_file("/init") {
            Some(d) => d,
            None => {
                println!("[!] /init not found on root fs");
                halt();
            }
        };
        let init_argv = [b"/init\0".to_vec()];
        let init_pid = crate::syscalls::handler::sys_spawn(
            0,
            data.as_ptr(),
            data.len(),
            -1,
            -1,
            -1,
            &init_argv,
            &[],
            -1,
            false,
        );
        if init_pid == usize::MAX {
            println!("[!] Failed to exec /init");
            loop {
                asm!("hlt");
            }
        }
        println!("[!] init spawned as pid={}", init_pid);

        // Enable only the IRQs with real handlers here:
        //   master IRQ0 = PIT, IRQ1 = keyboard, IRQ2 = slave cascade
        //   slave  IRQ12 = PS/2 mouse (slave line 4)
        // Keep shared PCI IRQ9 masked: OHCI + ToPIC hotplug are polling-only
        // until the kernel has a shared legacy-INTx dispatcher. IRQ11 also
        // stays masked because there is no active owner/handler for it here.
        PICS.set_masks(0xF8, 0xEF);
        println!(
            "[IRQ] PIC masks={:#04x}/{:#04x}",
            PICS.master_mask(),
            PICS.slave_mask(),
        );

        println!(
            "[!] Higher-half kernel running at 0x{:08x}",
            higher_half_entry as u32
        );
        println!("[!] Enabling interrupts — entering idle");
        init(200);

        // Start a fresh persistent kernel log for this boot. KLOG itself is
        // always populated by println!/debugln; disk persistence happens only
        // from this idle context, never from IRQ/print paths.
        let _ = VFS.get().mkdir("/var");
        if VFS.get().resolve_path("/var/system.log").is_some() {
            let _ = VFS.get().write_file("/var/system.log", &[]);
        } else {
            let _ = VFS.get().create_file("/var/system.log", &[]);
        }
        *SYSTEM_LOG_CURSOR.lock() = 0;
        flush_system_log();
        SYSTEM_LOG_LAST_FLUSH_MS.store(
            crate::time::uptime_ms() as usize,
            core::sync::atomic::Ordering::Release,
        );
        SYSTEM_LOG_READY.store(true, core::sync::atomic::Ordering::Release);

        // Enable interrupts. Boot used nested wrappers::_cli() above, while
        // this point intentionally releases *all* boot-time interrupt guards.
        // Keep the software nesting counter synchronized with the real IF.
        crate::wrappers::_rst();
        asm!("sti");

        loop {
            asm!("hlt");
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Prefer framebuffer if VESA is active (real HW with no serial).
    let used_fb = crate::fb_panic::try_panic_fb(info);

    // Still try VGA text + E9 (QEMU / text mode).
    println!("\n\n=== KERNEL PANIC ===");
    println!("Panic message: {}", info);
    if let Some(location) = info.location() {
        println!(
            "Location: {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }
    println!("System halted");

    loop {
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

fn first_ata_disk() -> Option<IDEDevice> {
    let ide = IDE.lock();
    for i in 0..4u8 {
        if let Some(dev) = ide.get_device(i) {
            if dev.r#type == 0 {
                return Some(dev);
            }
        }
    }
    None
}

fn flush_system_log() {
    let cursor = *SYSTEM_LOG_CURSOR.lock();
    let (bytes, end_cursor) = crate::print::klog_snapshot_from(cursor);
    if bytes.is_empty() {
        *SYSTEM_LOG_CURSOR.lock() = end_cursor;
        return;
    }

    let Some(inode) = VFS.get().resolve_path("/var/system.log") else {
        return;
    };
    let (written, _) = VFS.get().append_write(inode, &bytes);
    if written == bytes.len() {
        *SYSTEM_LOG_CURSOR.lock() = end_cursor;
    }
}

pub(crate) fn maybe_flush_system_log() {
    use core::sync::atomic::Ordering;

    if !SYSTEM_LOG_READY.load(Ordering::Acquire) {
        return;
    }
    let now = crate::time::uptime_ms() as usize;
    let last = SYSTEM_LOG_LAST_FLUSH_MS.load(Ordering::Acquire);
    if now.wrapping_sub(last) < 1000 {
        return;
    }
    if SYSTEM_LOG_LAST_FLUSH_MS
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    flush_system_log();
}

fn halt() -> ! {
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

fn print_info() {
    let mut p = PRINTER.lock();
    p.set_colors(0xf, 0);
    p.reset_colors();
}
