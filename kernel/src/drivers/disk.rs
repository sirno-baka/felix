// DISK DRIVER — ATA PIO MODE (Master + Slave)
use core::arch::asm;
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

#[derive(Copy, Clone)]
pub struct Disk {
    pub enabled: bool,
    is_master: bool,
}

impl Disk {
    pub const fn new(is_master: bool) -> Self {
        Disk {
            enabled: false,
            is_master,
        }
    }

    // Возвращает байт для записи в DRIVE_REGISTER (0xE0 или 0xF0 + LBA high)
    fn drive_select_byte(&self, lba: u64) -> u8 {
        let base = if self.is_master { 0xE0 } else { 0xF0 };
        base | ((lba >> 24) & 0x0F) as u8
    }

    // Выбрать нужный диск перед операцией
    fn select_drive(&self, lba: u64) {
        unsafe {
            asm!("out dx, al",
            in("dx") DRIVE_REGISTER,
            in("al") self.drive_select_byte(lba));
        }
        // Небольшая задержка для slave-диска (рекомендуется)
        if !self.is_master {
            for _ in 0..4 {
                unsafe { asm!("nop"); }
            }
        }
    }

    // ====================== READ ======================
    pub fn read<T>(&self, target: *mut T, lba: u64, sectors: u16) {
        if !self.enabled {
            println!("[ERROR] Cannot read! Disk {} not enabled", if self.is_master { "MASTER" } else { "SLAVE" });
            return;
        }

        while self.is_busy() {}

        self.select_drive(lba);

        unsafe {
            // disable ATA interrupt
            asm!("out dx, al", in("dx") 0x3f6, in("al") 0b00000010u8);

            asm!("out dx, al", in("dx") SECTOR_COUNT_REGISTER, in("al") sectors as u8);
            asm!("out dx, al", in("dx") LBA_LOW_REGISTER,  in("al") lba as u8);
            asm!("out dx, al", in("dx") LBA_MID_REGISTER,  in("al") (lba >> 8) as u8);
            asm!("out dx, al", in("dx") LBA_HIGH_REGISTER, in("al") (lba >> 16) as u8);
            // drive select уже выполнен
            asm!("out dx, al", in("dx") STATUS_COMMAND_REGISTER, in("al") READ_COMMAND);
        }

        let mut sectors_left = sectors;
        let mut target_pointer = target as *mut u32;

        while sectors_left > 0 {
            for _i in 0..128 {
                while self.is_busy() {}
                while !self.is_ready() {}

                let buffer: u32;
                unsafe {
                    asm!("in eax, dx", out("eax") buffer, in("dx") DATA_REGISTER);
                    core::ptr::write_unaligned(target_pointer, buffer);
                    target_pointer = target_pointer.byte_add(4);
                }
            }
            sectors_left -= 1;
        }

        self.reset();
    }

    // ====================== WRITE ======================
    pub fn write<T>(&self, source: *const T, lba: u64, sectors: u16) {
        if !self.enabled {
            println!("[ERROR] Cannot write! Disk {} not enabled", if self.is_master { "MASTER" } else { "SLAVE" });
            return;
        }

        while self.is_busy() {}

        self.select_drive(lba);

        unsafe {
            asm!("out dx, al", in("dx") 0x3f6, in("al") 0b00000010u8);

            asm!("out dx, al", in("dx") SECTOR_COUNT_REGISTER, in("al") sectors as u8);
            asm!("out dx, al", in("dx") LBA_LOW_REGISTER,  in("al") lba as u8);
            asm!("out dx, al", in("dx") LBA_MID_REGISTER,  in("al") (lba >> 8) as u8);
            asm!("out dx, al", in("dx") LBA_HIGH_REGISTER, in("al") (lba >> 16) as u8);
            asm!("out dx, al", in("dx") STATUS_COMMAND_REGISTER, in("al") WRITE_COMMAND);
        }

        let mut sectors_left = sectors;
        let mut source_pointer = source as *const u32;

        while sectors_left > 0 {
            while self.is_busy() || !self.is_drq() {}

            for _i in 0..128 {
                let data = unsafe { core::ptr::read_unaligned(source_pointer) };
                unsafe {
                    asm!("out dx, eax", in("dx") DATA_REGISTER, in("eax") data);
                    source_pointer = source_pointer.byte_add(4);
                }
            }
            sectors_left -= 1;
        }

        self.reset();
    }

    // ====================== STATUS ======================
    pub fn is_busy(&self) -> bool {
        let status: u8;
        unsafe {
            asm!("in al, dx", out("al") status, in("dx") STATUS_COMMAND_REGISTER);
        }
        (status & STATUS_BSY) != 0
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

    // ====================== CHECK ======================
    pub fn check(&mut self) {
        self.select_drive(0); // выбираем диск (LBA не важен для check)

        let status: u8;
        unsafe {
            asm!("in al, dx", out("al") status, in("dx") STATUS_COMMAND_REGISTER);
        }

        if status != 0 && status != 0xff {
            self.enabled = true;
            println!("[!] ATA {} drive found! Status: {:X}",
                     if self.is_master { "MASTER" } else { "SLAVE" }, status);
        } else {
            self.enabled = false;
            println!("[ERROR] ATA {} drive not working! Status: {:X}",
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
