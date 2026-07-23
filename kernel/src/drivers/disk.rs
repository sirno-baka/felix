// DISK DRIVER — ATA PIO MODE (Master + Slave)
use core::arch::asm;
use core::fmt;
use crate::println;

const DATA_REGISTER: u16 = 0x1f0;
const SECTOR_COUNT_REGISTER: u16 = 0x1f2;
const LBA_LOW_REGISTER: u16 = 0x1f3;
const LBA_MID_REGISTER: u16 = 0x1f4;
const LBA_HIGH_REGISTER: u16 = 0x1f5;
const DRIVE_REGISTER: u16 = 0x1f6;
const STATUS_COMMAND_REGISTER: u16 = 0x1f7;

const READ_COMMAND: u8 = 0x20;
const WRITE_COMMAND: u8 = 0x30;

const STATUS_BSY: u8 = 0b10000000;
const STATUS_RDY: u8 = 0b01000000;
const STATUS_DRQ: u8 = 0b00001000;
const STATUS_ERR: u8 = 0x1 ;

#[derive(Copy, Clone)]
pub struct Disk {
    pub enabled: bool,
    is_master: bool,
}

const ALT_STATUS_REGISTER: u16 = 0x3F6;

#[inline(always)]
fn io_delay() {
    unsafe {
        for _ in 0..4 {
            let _: u8;
            core::arch::asm!(
            "in al, dx",
            out("al") _,
            in("dx") 0x3F6u16,
            options(nostack, preserves_flags)
            );
        }
    }
}


impl Disk {
    pub const fn new(is_master: bool) -> Self {
        Disk {
            enabled: false,
            is_master,
        }
    }
    #[inline]
    fn status(&self) -> u8 {
        let status: u8;
        unsafe {
            core::arch::asm!(
            "in al, dx",
            out("al") status,
            in("dx") STATUS_COMMAND_REGISTER,
            options(nostack, preserves_flags)
            );
        }
        status
    }

    /// Более надёжное ожидание DRQ с таймаутом
    fn wait_drq(&self) -> bool {
        for _ in 0..100_000 {          // достаточно большой таймаут
            let s = self.status();

            if (s & STATUS_BSY) != 0 {
                // можно чуть подождать
                for _ in 0..10 { unsafe { core::arch::asm!("nop"); } }
                continue;
            }

            if (s & STATUS_ERR) != 0 {
                println!("[DISK] ERR bit set, status = {:02X}", s);
                return false;
            }

            if (s & STATUS_DRQ) != 0 {
                return true;
            }

            // небольшая пауза, чтобы дать контроллеру обновить статус
            for _ in 0..50 {
                unsafe { core::arch::asm!("nop"); }
            }
        }

        println!("[DISK] wait_drq TIMEOUT, last status = {:02X}", self.status());
        false
    }

    fn wait_ready(&self) {
        while (self.status() & STATUS_BSY) != 0 {}
    }

    // Возвращает байт для записи в DRIVE_REGISTER (0xE0 или 0xF0 + LBA high)
    fn drive_select_byte(&self, lba: u64) -> u8 {
        let base = if self.is_master { 0xE0 } else { 0xF0 };
        base | ((lba >> 24) & 0x0F) as u8
    }

    // Выбрать нужный диск перед операцией
    fn select_drive(&self, lba: u64) {
        unsafe {
            core::arch::asm!("out dx, al",
            in("dx") DRIVE_REGISTER,
            in("al") self.drive_select_byte(lba));
        }
        io_delay();          // ← обязательно
    }


    pub fn read<T>(&self, target: *mut T, lba: u64, sectors: u16) {
        if !self.enabled {
            println!("[ERROR] Cannot read!");
            return;
        }

        // 1. Ждём готовности
        self.wait_ready();

        // 2. Выбираем диск
        self.select_drive(lba);
        io_delay();
        io_delay();

        // 3. Ещё раз ждём после select
        self.wait_ready();

        unsafe {
            // nIEN = 1
            core::arch::asm!("out dx, al", in("dx") 0x3F6u16, in("al") 0b00000010u8, options(nostack, preserves_flags));
            io_delay();

            // Параметры с паузами
            core::arch::asm!("out dx, al", in("dx") SECTOR_COUNT_REGISTER, in("al") sectors as u8, options(nostack, preserves_flags));
            io_delay();

            core::arch::asm!("out dx, al", in("dx") LBA_LOW_REGISTER,  in("al") (lba) as u8, options(nostack, preserves_flags));
            io_delay();

            core::arch::asm!("out dx, al", in("dx") LBA_MID_REGISTER,  in("al") (lba >> 8) as u8, options(nostack, preserves_flags));
            io_delay();

            core::arch::asm!("out dx, al", in("dx") LBA_HIGH_REGISTER, in("al") (lba >> 16) as u8, options(nostack, preserves_flags));
            io_delay();

            // Команда READ SECTORS
            core::arch::asm!("out dx, al", in("dx") STATUS_COMMAND_REGISTER, in("al") READ_COMMAND, options(nostack, preserves_flags));
        }

        // Даём контроллеру время после команды
        io_delay();
        io_delay();
        io_delay();

        println!("[DISK] read: after cmd, status = {:02X}", self.status());

        let mut ptr = target as *mut u16;

        for sector in 0..sectors {
            if !self.wait_drq() {
                println!("[DISK] read: no DRQ at sector {}, status={:02X}", sector, self.status());
                return;
            }

            for _ in 0..256 {
                let word: u16;
                unsafe {
                    core::arch::asm!(
                    "in ax, dx",
                    out("ax") word,
                    in("dx") DATA_REGISTER,
                    options(nostack, preserves_flags)
                    );
                    core::ptr::write_unaligned(ptr, word);
                    ptr = ptr.add(1);
                }
            }
        }

        // Финальная проверка
        self.wait_ready();
        println!("[DISK] read finished, final status = {:02X}", self.status());
    }

    pub fn write<T>(&self, source: *const T, lba: u64, sectors: u16) {
        if !self.enabled {
            println!("[ERROR] Cannot write!");
            return;
        }

        // 1. Ждём, пока диск точно готов
        self.wait_ready();

        // 2. Выбираем диск + LBA[27:24]
        self.select_drive(lba);
        io_delay();
        io_delay();                 // чуть больше задержки

        // 3. Ещё раз проверяем, что после select он всё ещё ready
        self.wait_ready();

        unsafe {
            // nIEN = 1 (отключаем IRQ)
            core::arch::asm!("out dx, al", in("dx") 0x3F6u16, in("al") 0b00000010u8, options(nostack, preserves_flags));
            io_delay();

            // Параметры (с паузами между каждым out)
            core::arch::asm!("out dx, al", in("dx") SECTOR_COUNT_REGISTER, in("al") sectors as u8, options(nostack, preserves_flags));
            io_delay();

            core::arch::asm!("out dx, al", in("dx") LBA_LOW_REGISTER,  in("al") (lba) as u8, options(nostack, preserves_flags));
            io_delay();

            core::arch::asm!("out dx, al", in("dx") LBA_MID_REGISTER,  in("al") (lba >> 8) as u8, options(nostack, preserves_flags));
            io_delay();

            core::arch::asm!("out dx, al", in("dx") LBA_HIGH_REGISTER, in("al") (lba >> 16) as u8, options(nostack, preserves_flags));
            io_delay();

            // Команда WRITE SECTORS
            core::arch::asm!("out dx, al", in("dx") STATUS_COMMAND_REGISTER, in("al") WRITE_COMMAND, options(nostack, preserves_flags));
        }

        // Очень важно: после команды дать контроллеру время
        io_delay();
        io_delay();
        io_delay();

        println!("[DISK] after cmd, status = {:02X}", self.status());

        let mut ptr = source as *const u16;

        for sector in 0..sectors {
            if !self.wait_drq() {
                println!("[DISK] write: no DRQ at sector {}, status={:02X}", sector, self.status());
                return;
            }

            for _ in 0..256 {
                let word = unsafe { core::ptr::read_unaligned(ptr) };
                unsafe {
                    core::arch::asm!(
                    "out dx, ax",
                    in("dx") DATA_REGISTER,
                    in("ax") word,
                    options(nostack, preserves_flags)
                    );
                    ptr = ptr.add(1);
                }
            }
        }

        // Ждём окончания записи на пластину
        self.wait_ready();
        println!("[DISK] write finished, final status = {:02X}", self.status());
    }


    pub fn is_err(&self) -> bool {
        let status: u8;
        unsafe {
            asm!("in al, dx", out("al") status, in("dx") STATUS_COMMAND_REGISTER);
        }
        (status & STATUS_ERR) != 0
    }

    pub fn is_ready(&self) -> bool {
        let status: u8;
        unsafe {
            asm!("in al, dx", out("al") status, in("dx") STATUS_COMMAND_REGISTER);
        }
        (status & STATUS_RDY) != 0
    }

    pub fn is_drq(&self) -> bool {
        let status: u8;
        unsafe {
            asm!("in al, dx", out("al") status, in("dx") STATUS_COMMAND_REGISTER);
        }
        (status & STATUS_DRQ) != 0
    }

    pub fn check(&mut self) {
        self.select_drive(0);
        io_delay();

        let status = self.status();

        if status != 0x00 && status != 0xFF {
            self.enabled = true;
            println!("[!] ATA {} drive found! Status: {:02X}",
                     if self.is_master { "MASTER" } else { "SLAVE" }, status);
        } else {
            self.enabled = false;
            println!("[ERROR] ATA {} drive not working! Status: {:02X}",
                     if self.is_master { "MASTER" } else { "SLAVE" }, status);
        }
    }

    pub fn reset(&self) {
        unsafe {
            asm!("out dx, al", in("dx") 0x3f6, in("al") 0b00000110u8); // reset
            asm!("out dx, al", in("dx") 0x3f6, in("al") 0b00000010u8); // normal
        }
    }
}


// ====================== ГЛОБАЛЬНЫЕ ДИСКИ ======================
pub static mut DISK: Disk = Disk::new(true);
pub static mut DISK_SLAVE: Disk = Disk::new(false);
