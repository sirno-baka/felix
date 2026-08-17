#![no_std]
#![no_main]

#[macro_use]
mod print;

mod disk;
mod gdt;
mod splash;
mod tss;
mod vesa;

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

const RAMFS_LBA: u64 = 2114; //kernel location logical block address
const RAMFS_TARGET: u32 = 0x0060_0000; //where to put kernel in memory
const RAMFS_SIZE: u16 = 766; //kernel size in sectors

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

    // Load kernel + ramfs while still in VGA text mode so messages are visible.
    unsafe {
        DISK.init(KERNEL_LBA as u32, KERNEL_BUFFER);
        DISK.reset();
        DISK.delay();
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

    unsafe {
        DISK.init(RAMFS_LBA as u32, KERNEL_BUFFER);
        DISK.read_sectors(RAMFS_SIZE, RAMFS_TARGET);
    }
    println!("[!] RamFS loaded to memory.");
    // В _start() после загрузки:
    let checksum = calculate_checksum(RAMFS_TARGET, (RAMFS_SIZE as usize) * 512);
    println!("[!] RamFS checksum: 0x{:08x}", checksum);

    // ===================== VESA =====================
    // Switch graphics mode AFTER all text boot messages.
    // Framebuffer info is written to 0x5000 for the kernel.
    println!("[!] Setting VESA graphics mode...");
    unsafe {
        if vesa::init_vesa() {
            // Text mode is gone — further println! may be invisible on screen
            // (still go to serial/E9 if enabled).
        }
    }
    println!("[!] Loading Global Descriptor Table...");



    unsafe {
        GDT.load();          // ← обязательно!
    }
    // ================================================
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
        // === 1. Снова загружаем GDT (критично после VESA) ===
        GDT.load();

        // === 2. Включаем Protected Mode ===
        asm!(
        "mov eax, cr0",
        "or eax, 1",
        "mov cr0, eax",
        options(nostack, preserves_flags)
        );

        // === 3. Far jump с явным 32-битным смещением ===
        // Используем абсолютный адрес метки через lea + ljmp
        asm!(
        // Получаем адрес метки в eax
        "lea eax, [2f]",
        "push 0x08",          // CS = 0x08
        "push eax",           // offset
        "retf",               // far return = ljmp
        "2:",
        options(nostack)
        );

        // === 4. Теперь мы точно в 32-битном коде с правильным CS ===
        asm!(
        ".code32",
        "mov ax, 0x10",
        "mov ds, ax",
        "mov es, ax",
        "mov fs, ax",
        "mov gs, ax",
        "mov ss, ax",

        // Ставим нормальный стек (на всякий случай)
        "mov esp, 0x90000",

        // Прыгаем в ядро
        "mov eax, 0x01000000",
        "call eax",

        // Если вернулось — зависаем
        "3:",
        "hlt",
        "jmp 3b",
        options(nostack)
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
