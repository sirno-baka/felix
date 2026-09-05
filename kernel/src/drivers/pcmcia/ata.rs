//! Generic ATA PIO task-file support.
//!
//! This module is intentionally independent from PCMCIA/CardBus.  The same
//! primitive can later be reused by the legacy IDE controller by providing
//! another task-file I/O base.

use crate::disk::interface::BlockDevice;
use crate::io::{inb, inw, outb, outw};
use crate::pci::ide::ata::ATA;
use crate::time::sleep;

const STATUS_ERR: u8 = 0x01;
const STATUS_DF: u8 = 0x20;
const STATUS_DRQ: u8 = 0x08;
const STATUS_BSY: u8 = 0x80;

const CMD_IDENTIFY: u8 = 0xEC;
const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const STATUS_DRDY: u8 = 0x40;
const SECTOR_SIZE: u32 = 512;

#[derive(Copy, Clone, Debug)]
pub struct IdentifyData {
    pub model: [u8; 40],
    pub sectors: u64,
    pub lba48: bool,
}

#[derive(Copy, Clone)]
pub struct AtaPio {
    base: u16,
}

impl AtaPio {
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    #[inline] fn data(&self) -> u16 { self.base }
    #[inline] fn error_features(&self) -> u16 { self.base + 1 }
    #[inline] fn sector_count(&self) -> u16 { self.base + 2 }
    #[inline] fn lba0(&self) -> u16 { self.base + 3 }
    #[inline] fn lba1(&self) -> u16 { self.base + 4 }
    #[inline] fn lba2(&self) -> u16 { self.base + 5 }
    #[inline] fn device(&self) -> u16 { self.base + 6 }
    #[inline] fn status_command(&self) -> u16 { self.base + 7 }

    fn wait_not_busy(&self) -> Option<u8> {
        for _ in 0..2000 {
            let status = inb(self.status_command());
            if status == 0x00 || status == 0xFF { return None; }
            if (status & STATUS_BSY) == 0 { return Some(status); }
            sleep(1);
        }
        None
    }

    pub fn identify(&self) -> Option<IdentifyData> {
        crate::println!("[ATA] IDENTIFY (0xEC) base=0x{:03x}", self.base);
        outb(self.device(), 0xA0);
        sleep(1);
        outb(self.sector_count(), 0);
        outb(self.lba0(), 0);
        outb(self.lba1(), 0);
        outb(self.lba2(), 0);
        outb(self.status_command(), CMD_IDENTIFY);

        let mut status = match self.wait_not_busy() {
            Some(value) => value,
            None => { crate::println!("[ATA] no status after IDENTIFY"); return None; }
        };

        for _ in 0..2000 {
            status = inb(self.status_command());
            if (status & (STATUS_ERR | STATUS_DF)) != 0 {
                let error = inb(self.error_features());
                crate::println!("[ATA] IDENTIFY failed status={:02x} error={:02x}", status, error);
                return None;
            }
            if (status & STATUS_BSY) == 0 && (status & STATUS_DRQ) != 0 { break; }
            sleep(1);
        }

        status = inb(self.status_command());
        if (status & STATUS_DRQ) == 0 {
            crate::println!("[ATA] IDENTIFY timeout status={:02x}", status);
            return None;
        }

        let mut words = [0u16; 256];
        for word in words.iter_mut() { *word = inw(self.data()); }

        let mut model = [b' '; 40];
        for (i, word) in words[27..47].iter().enumerate() {
            model[i * 2] = (word >> 8) as u8;
            model[i * 2 + 1] = *word as u8;
        }

        let lba28 = (words[60] as u32) | ((words[61] as u32) << 16);
        let lba48 = (words[100] as u64)
            | ((words[101] as u64) << 16)
            | ((words[102] as u64) << 32)
            | ((words[103] as u64) << 48);
        let has_lba48 = (words[83] & (1 << 10)) != 0;
        let sectors = if has_lba48 { lba48 } else { lba28 as u64 };

        crate::print!("[ATA] model: ");
        for byte in model.iter() {
            let ch = if *byte >= 0x20 && *byte <= 0x7e { *byte as char } else { ' ' };
            crate::print!("{}", ch);
        }
        crate::println!("");
        crate::println!(
            "[ATA] IDENTIFY ok status={:02x} LBA48={} sectors={} capacity={} MiB",
            status, has_lba48, sectors, sectors / 2048
        );
        Some(IdentifyData { model, sectors, lba48: has_lba48 })
    }

    fn wait_drq(&self) -> Result<(), u8> {
        for _ in 0..2000 {
            let status = inb(self.status_command());
            if (status & STATUS_BSY) != 0 { sleep(1); continue; }
            if (status & (STATUS_ERR | STATUS_DF)) != 0 { return Err(inb(self.error_features())); }
            if (status & STATUS_DRQ) != 0 { return Ok(()); }
            sleep(1);
        }
        Err(0xFF)
    }

    fn select_lba28(&self, lba: u32) {
        outb(self.device(), 0xE0 | (((lba >> 24) as u8) & 0x0F));
        sleep(1);
    }


}

impl BlockDevice for AtaPio {
    fn read_sectors(&self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        if numsects == 0 || buf == 0 || lba > 0x0FFF_FFFF { return Err(1); }
        self.select_lba28(lba);
        outb(self.sector_count(), numsects);
        outb(self.lba0(), lba as u8);
        outb(self.lba1(), (lba >> 8) as u8);
        outb(self.lba2(), (lba >> 16) as u8);
        outb(self.status_command(), CMD_READ_SECTORS);

        let dst = buf as *mut u16;
        for sector in 0..numsects as usize {
            self.wait_drq()?;
            let ptr = unsafe { dst.add(sector * 256) };
            for i in 0..256usize {
                let word = inw(self.data());
                unsafe { core::ptr::write_volatile(ptr.add(i), word); }
            }
        }
        let _ = inb(self.status_command());
        Ok(())
    }

    fn write_sectors(&mut self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        if numsects == 0 || buf == 0 || lba > 0x0FFF_FFFF { return Err(1); }
        self.select_lba28(lba);
        outb(self.sector_count(), numsects);
        outb(self.lba0(), lba as u8);
        outb(self.lba1(), (lba >> 8) as u8);
        outb(self.lba2(), (lba >> 16) as u8);
        outb(self.status_command(), CMD_WRITE_SECTORS);

        let src = buf as *const u16;
        for sector in 0..numsects as usize {
            self.wait_drq()?;
            let ptr = unsafe { src.add(sector * 256) };
            for i in 0..256usize {
                let word = unsafe { core::ptr::read_volatile(ptr.add(i)) };
                outw(self.data(), word);
            }
            let _ = inb(self.status_command());
        }

        for _ in 0..2000 {
            let status = inb(self.status_command());
            if (status & STATUS_BSY) == 0 {
                if (status & (STATUS_ERR | STATUS_DF)) != 0 { return Err(inb(self.error_features())); }
                return Ok(());
            }
            sleep(1);
        }
        Err(0xFF)
    }

    fn sector_size(&self) -> u32 { SECTOR_SIZE }
}
