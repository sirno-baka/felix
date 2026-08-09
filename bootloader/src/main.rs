#![no_std]
#![no_main]

#[macro_use]
mod print;

mod disk;
mod gdt;
mod splash;
mod tss;

use core::arch::asm;
use core::panic::PanicInfo;
use disk::DISK;
use gdt::GlobalDescriptorTable;
use crate::gdt::GDT;

//const VERSION: &str = env!("CARGO_PKG_VERSION");
const KERNEL_LBA: u64 = 65; //kernel location logical block address

const KERNEL_SIZE: u16 = 2048; //kernel size in sectors

const KERNEL_BUFFER: u16 = 0xbe00; //buffer location for copy
const KERNEL_TARGET: u32 = 0x0100_0000; //where to put kernel in memory

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("PANIC! Info: {}", info);

    loop {}
}

//bootloader entry point
#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    //uncomment to enable splashscreen
    //clear!();
    //splash::splash();
    //wait_for_key();
    //clear!();
    gdt::GlobalDescriptorTable::init();     // ← инициализируем один раз
    //unreal mode is needed because diskreader needs to copy from buffer to protected mode memory

    unsafe {
        GDT.load();
        // GDT.load_tss();
        // GDT.set_kernel_stack(STACK_START);
    }
    //load kernel
    print!("[!] Loading kernel");
    enable_a20();
    println!("[!] Switching to 16bit unreal mode...");
    unreal_mode();
    println!("[!] Checking memory...");


    unsafe {
        DISK.init(KERNEL_LBA as u32, KERNEL_BUFFER);
        DISK.read_sectors(KERNEL_SIZE, KERNEL_TARGET);
    }

    println!("[!] Kernel loaded to memory.");
    fn calculate_checksum(addr: u32, len: usize) -> u32 {
        let mut sum = 0u32;
        let ptr = addr as *const u8;
        for i in 0..len {
            sum = sum.wrapping_add(unsafe { *ptr.add(i) } as u32);
        }
        sum
    }

    // В _start() после загрузки:
    let checksum = calculate_checksum(KERNEL_TARGET, (KERNEL_SIZE as usize) * 512);
    println!("[!] Kernel checksum: 0x{:08x}", checksum);
    //load dgt
    println!("[!] Loading Global Descriptor Table...");



    //switch to protected mode
    println!("[!] Switching to 32bit protected mode and jumping to kernel...");
    protected_mode();

    //loop in case kernel returns
    loop {}
}
/// Печатает содержимое памяти в hex (работает в unreal mode / protected mode)
fn hexdump(addr: u32, len: usize) {
    unsafe {
        let ptr = addr as *const u8;

        for i in 0..len {
            if i % 16 == 0 {
                if i != 0 {
                    println!();
                }
                print!("{:08x}: ", addr + i as u32);
            }
            let byte = *ptr.add(i);
            print!("{:02x} ", byte);
        }
        println!();
    }
}
#[no_mangle]
pub extern "C" fn fail() -> ! {
    println!("[!] Read fail!");

    loop {}
}

//switch to 32bit protected mode and jump to kernel
fn protected_mode() {
    unsafe {
        //enable protected mode in cr0 register
        asm!("mov eax, cr0", "or al, 1", "mov cr0, eax");
        // println!("Protected mode enabled");
        //push kernel address
        asm!(
            "push {0:e}",
            in(reg) KERNEL_TARGET,
        );

        //jump to protected mode
        asm!("ljmp $0x8, $2f", "2:", options(att_syntax));
        // println!("Protected mode entrer");
        //protected mode start
        asm!(
            ".code32",

            //setup segment registers
            "mov {0:e}, 0x10",
            "mov ds, {0:e}",
            "mov es, {0:e}",
            "mov fs, {0:e}",
            "mov gs, {0:e}",
            "mov ss, {0:e}",

            //jump to kernel
            "pop {1:e}",
            "call {1:e}",

            out(reg) _,
            in(reg) KERNEL_TARGET,
        );
    }
}

fn unreal_mode() {
    //backup segment registers
    let ds: u16;
    let ss: u16;
    unsafe {
        asm!("mov {0:x}, ds", out(reg) ds);
        asm!("mov {0:x}, ss", out(reg) ss);
    }

    //load gdt
    unsafe {
        GDT.load();                             // ← старый вызов теперь работает
    }

    unsafe {
        //backup cr0 register
        let mut cr0: u32;
        asm!("mov {0:e}, cr0", out(reg) cr0);

        //set cr0 protected bit
        let cr0_protected = cr0 | 1;
        asm!("mov cr0, {0:e}", in(reg) cr0_protected);

        //setup segment registers
        asm!("mov {0:x}, 0x10", "mov ds, {0:x}", "mov ss, {0:x}", out(reg) _);

        //restore cr0 register
        asm!("mov cr0, {0:e}", in(reg) cr0);

        //restore segment registers
        asm!("mov ds, {0:x}", in(reg) ds);
        asm!("mov ss, {0:x}", in(reg) ss);
    }
}
//
// fn unreal_mode() {
//     unsafe {
//         // Загружаем GDT (можно делать в real mode)
//         GDT.load();
//
//         // Сохраняем CR0
//         let mut cr0: u32;
//         asm!("mov {0:e}, cr0", out(reg) cr0);
//
//         // Входим в protected mode
//         let cr0_protected = cr0 | 1;
//         asm!("mov cr0, {0:e}", in(reg) cr0_protected);
//
//         // Загружаем плоский data-селектор (0x10) во ВСЕ сегментные регистры данных
//         asm!(
//         "mov {0:x}, 0x10",
//         "mov ds, {0:x}",
//         "mov es, {0:x}",
//         "mov fs, {0:x}",
//         "mov gs, {0:x}",
//         "mov ss, {0:x}",
//         out(reg) _,
//         );
//
//         // Выходим обратно в real mode, но кэшированные дескрипторы остаются 4GB
//         asm!("mov cr0, {0:e}", in(reg) cr0);
//     }
//     // НЕ ВОССТАНАВЛИВАЕМ старые DS/SS — это и было ошибкой!
// }

fn enable_a20() {
    unsafe {
        // 1. Fast A20 (порт 0x92) - самый надежный способ
        let mut val: u8;
        asm!("in al, 0x92", out("al") val);
        if (val & 2) == 0 {
            val |= 2;       // Включить A20
            val &= !1;      // Не сбросить CPU (бит 0)
            asm!("out 0x92, al", in("al") val);
        }

        // 2. Fallback на BIOS
        let mut ax: u16;
        asm!(
        "mov ax, 0x2401",
        "int 0x15",
        "mov {0:x}, ax",
        lateout(reg) ax,
        options(nostack, preserves_flags),
        );
    }
}
#[allow(dead_code)]
fn wait_for_key() {
    unsafe {
        asm!("int 0x16", in("ah") 0x00 as u8);
    }
}
