//! Intel 82365-compatible ExCA/PC16 socket controller access.

use core::ptr::{read_volatile, write_volatile};

use super::{CF_IO_BASE, CF_IO_END, CF_MEM_PHYS};

const PC16_OFFSET: u32 = 0x800;

pub mod reg {
    pub const IDREV: u32 = 0x00;
    pub const IFSTAT: u32 = 0x01;
    pub const PWCTRL: u32 = 0x02;
    pub const IGCTRL: u32 = 0x03;
    pub const CSCHG: u32 = 0x04;
    pub const CSCINT: u32 = 0x05;
    pub const AWINEN: u32 = 0x06;
    pub const IOCTRL: u32 = 0x07;
    pub const IOWIN0_START: u32 = 0x08;
    pub const IOWIN0_END: u32 = 0x0A;
    pub const IOWIN0_OFFSET: u32 = 0x0C;
    pub const MEMWIN0_START: u32 = 0x10;
    pub const MEMWIN0_END: u32 = 0x12;
    pub const MEMWIN0_OFFSET: u32 = 0x14;
    pub const CB_MEM_PAGE0: u32 = 0x40;
    pub const CDGENC: u32 = 0x16;
    pub const MEMWIN1_START: u32 = 0x18;
    pub const MEMWIN1_END: u32 = 0x1A;
    pub const MEMWIN1_OFFSET: u32 = 0x1C;
    pub const GLCTRL: u32 = 0x1E;
    pub const ATCTRL: u32 = 0x1F;
    pub const MEMWIN2_START: u32 = 0x20;
    pub const MEMWIN2_END: u32 = 0x22;
    pub const MEMWIN2_OFFSET: u32 = 0x24;
    pub const MEMWIN3_START: u32 = 0x28;
    pub const MEMWIN3_END: u32 = 0x2A;
    pub const MEMWIN3_OFFSET: u32 = 0x2C;
    pub const MISCC1: u32 = 0x30;
    pub const MEMWIN4_START: u32 = 0x31;
    pub const MEMWIN4_END: u32 = 0x33;
    pub const MEMWIN4_OFFSET: u32 = 0x35;
    pub const IO_OFFSET0: u32 = 0x37;
    pub const IO_OFFSET1: u32 = 0x39;
    pub const GPIO: u32 = 0x3B;
    pub const SMPGA0: u32 = 0x40;
}

pub mod ifstat {
    pub const BVD1: u8 = 0x01;
    pub const BVD2: u8 = 0x02;
    pub const CD1: u8 = 0x04;
    pub const CD2: u8 = 0x08;
    pub const WP: u8 = 0x10;
    pub const READY: u8 = 0x20;
    pub const POWERON: u8 = 0x40;
    pub const GPI: u8 = 0x80;
}

pub mod power {
    pub const OFF: u8 = 0x00;
    pub const OUTPUT_ENABLE: u8 = 0x80;
    pub const NORESET: u8 = 0x40;
    pub const AUTO: u8 = 0x20;
    pub const VCC_5V: u8 = 0x10;
    pub const VCC_3V3: u8 = 0x18;
}

pub mod igctrl {
    pub const PC_RESET: u8 = 0x40;
    pub const PC_IOCARD: u8 = 0x20;
    pub const IRQ_MASK: u8 = 0x0F;
}

pub mod addrwin {
    pub const MEM0: u8 = 0x01;
    pub const IO0: u8 = 0x40;
    pub const IO1: u8 = 0x80;
}

pub mod ioctrl {
    pub const IO0_16BIT: u8 = 0x01;
    pub const IO0_IOCS16: u8 = 0x02;
    pub const IO0_0WS: u8 = 0x04;
    pub const IO0_WAIT: u8 = 0x08;
    pub const IO1_16BIT: u8 = 0x10;
    pub const IO1_IOCS16: u8 = 0x20;
    pub const IO1_0WS: u8 = 0x40;
    pub const IO1_WAIT: u8 = 0x80;
}

#[derive(Copy, Clone, Debug)]
pub struct SocketStatus {
    pub raw: u8,
    pub bvd1: bool,
    pub bvd2: bool,
    pub cd1: bool,
    pub cd2: bool,
    pub write_protected: bool,
    pub ready: bool,
    pub power_on: bool,
    pub gpi: bool,
}

impl SocketStatus {
    pub fn from_raw(raw: u8) -> Self {
        Self {
            raw,
            bvd1: (raw & ifstat::BVD1) != 0,
            bvd2: (raw & ifstat::BVD2) != 0,
            cd1: (raw & ifstat::CD1) != 0,
            cd2: (raw & ifstat::CD2) != 0,
            write_protected: (raw & ifstat::WP) != 0,
            ready: (raw & ifstat::READY) != 0,
            power_on: (raw & ifstat::POWERON) != 0,
            gpi: (raw & ifstat::GPI) != 0,
        }
    }

    pub fn card_present(&self) -> bool { self.cd1 && self.cd2 }
}

#[derive(Copy, Clone)]
pub struct Pc16 {
    base: u32,
}

impl Pc16 {
    pub const fn new(bar0_virt: u32) -> Self { Self { base: bar0_virt + PC16_OFFSET } }

    #[inline]
    unsafe fn read8(&self, offset: u32) -> u8 { read_volatile((self.base + offset) as *const u8) }
    #[inline]
    unsafe fn write8(&self, offset: u32, value: u8) { write_volatile((self.base + offset) as *mut u8, value); }
    #[inline]
    unsafe fn read16(&self, offset: u32) -> u16 {
        let lo = read_volatile((self.base + offset) as *const u8);
        let hi = read_volatile((self.base + offset + 1) as *const u8);
        (lo as u16) | ((hi as u16) << 8)
    }
    #[inline]
    unsafe fn write16(&self, offset: u32, value: u16) {
        write_volatile((self.base + offset) as *mut u8, value as u8);
        write_volatile((self.base + offset + 1) as *mut u8, (value >> 8) as u8);
    }

    pub unsafe fn idrev(&self) -> u8 { self.read8(reg::IDREV) }
    pub unsafe fn ifstat(&self) -> u8 { self.read8(reg::IFSTAT) }
    pub unsafe fn status(&self) -> SocketStatus { SocketStatus::from_raw(self.ifstat()) }
    pub unsafe fn pwctrl(&self) -> u8 { self.read8(reg::PWCTRL) }
    pub unsafe fn igctrl(&self) -> u8 { self.read8(reg::IGCTRL) }
    pub unsafe fn cschg(&self) -> u8 { self.read8(reg::CSCHG) }
    pub unsafe fn cscint(&self) -> u8 { self.read8(reg::CSCINT) }
    pub unsafe fn awinen(&self) -> u8 { self.read8(reg::AWINEN) }
    pub unsafe fn ioctrl(&self) -> u8 { self.read8(reg::IOCTRL) }
    pub unsafe fn write_reg8(&self, offset: u32, value: u8) { self.write8(offset, value) }
    pub unsafe fn write_reg16(&self, offset: u32, value: u16) { self.write16(offset, value) }

    pub unsafe fn power_off(&self) { self.write8(reg::PWCTRL, power::OFF); }
    pub unsafe fn power_5v(&self) { self.write8(reg::PWCTRL, power::OUTPUT_ENABLE | power::NORESET | power::VCC_5V); }
    pub unsafe fn power_3v3(&self) { self.write8(reg::PWCTRL, power::OUTPUT_ENABLE | power::NORESET | power::VCC_3V3); }

    pub unsafe fn card_reset_assert(&self) { self.write8(reg::IGCTRL, self.read8(reg::IGCTRL) | igctrl::PC_RESET); }
    pub unsafe fn card_reset_deassert(&self) { self.write8(reg::IGCTRL, self.read8(reg::IGCTRL) & !igctrl::PC_RESET); }
    pub unsafe fn set_io_card_mode(&self, enabled: bool) {
        let mut v = self.read8(reg::IGCTRL);
        if enabled { v |= igctrl::PC_IOCARD; } else { v &= !igctrl::PC_IOCARD; }
        self.write8(reg::IGCTRL, v);
    }
    pub unsafe fn set_irq(&self, irq: u8) {
        let mut v = self.read8(reg::IGCTRL) & !igctrl::IRQ_MASK;
        v |= irq & igctrl::IRQ_MASK;
        self.write8(reg::IGCTRL, v);
    }

    pub unsafe fn configure_cf_io(&self) {
        let mut awinen = self.read8(reg::AWINEN) & !(addrwin::IO0 | addrwin::IO1);
        self.write8(reg::AWINEN, awinen);
        self.write16(reg::IOWIN0_START, CF_IO_BASE);
        self.write16(reg::IOWIN0_END, CF_IO_END);
        // 0xC000 + 0x41E0 = 0x01E0 on the card (16-bit wrap)
        self.write16(reg::IOWIN0_OFFSET, 0x41E0);
        self.write16(reg::IO_OFFSET0, 0x41E0);
        let mut ioctl = self.read8(reg::IOCTRL);
        ioctl &= !0xFF;
        ioctl |= ioctrl::IO0_16BIT | ioctrl::IO0_IOCS16 | ioctrl::IO0_WAIT
            | ioctrl::IO1_16BIT | ioctrl::IO1_IOCS16 | ioctrl::IO1_WAIT;
        self.write8(reg::IOCTRL, ioctl);
        // Ricoh 16-bit ATA timing mode (RF5C_MODE_ATA).
        self.write8(reg::ATCTRL, 0x01);
        self.write8(reg::AWINEN, awinen | addrwin::MEM0 | addrwin::IO0);
    }

    pub unsafe fn configure_cf_attribute_window(&self) {
        let mut awinen = self.read8(reg::AWINEN) & !addrwin::MEM0;
        self.write8(reg::AWINEN, awinen);
        self.write8(reg::MISCC1, 0x01);
        self.write8(reg::CB_MEM_PAGE0, (CF_MEM_PHYS >> 24) as u8);
        let page = ((CF_MEM_PHYS >> 12) & 0x0fff) as u16;
        self.write16(reg::MEMWIN0_START, page);
        self.write16(reg::MEMWIN0_END, page | 0x8000);
        self.write16(reg::MEMWIN0_OFFSET, 0x4000);
        awinen |= addrwin::MEM0;
        self.write8(reg::AWINEN, awinen);
    }

    pub unsafe fn dump(&self) {
        crate::println!("[PCMCIA] PC16 register dump:");
        for (off, name) in [
            (reg::IDREV, "IDREV"), (reg::IFSTAT, "IFSTAT"),
            (reg::PWCTRL, "PWCTRL"), (reg::IGCTRL, "IGCTRL"),
            (reg::CSCHG, "CSCHG"), (reg::CSCINT, "CSCINT"),
            (reg::AWINEN, "AWINEN"), (reg::IOCTRL, "IOCTRL"),
        ] {
            crate::println!("[PCMCIA]   +{:02x} {:<6} = {:02x}", off, name, self.read8(off));
        }
        crate::println!("[PCMCIA]   +08 IOWIN0  = {:04x}", self.read16(reg::IOWIN0_START));
        crate::println!("[PCMCIA]   +0a IOWIN0E = {:04x}", self.read16(reg::IOWIN0_END));
        crate::println!("[PCMCIA]   +0c IOOFF0  = {:04x}", self.read16(reg::IOWIN0_OFFSET));
        crate::println!("[PCMCIA]   +10 MEM0S   = {:04x}", self.read16(reg::MEMWIN0_START));
        crate::println!("[PCMCIA]   +12 MEM0E   = {:04x}", self.read16(reg::MEMWIN0_END));
        crate::println!("[PCMCIA]   +14 MEM0OFF = {:04x}", self.read16(reg::MEMWIN0_OFFSET));
        crate::println!("[PCMCIA]   +16 CDGENC  = {:02x}", self.read8(reg::CDGENC));
        crate::println!("[PCMCIA]   +18 MEM1S   = {:04x}", self.read16(reg::MEMWIN1_START));
        crate::println!("[PCMCIA]   +1a MEM1E   = {:04x}", self.read16(reg::MEMWIN1_END));
        crate::println!("[PCMCIA]   +1c MEM1OFF = {:04x}", self.read16(reg::MEMWIN1_OFFSET));
        crate::println!("[PCMCIA]   +1e GLCTRL  = {:02x}", self.read8(reg::GLCTRL));
        crate::println!("[PCMCIA]   +1f ATCTRL  = {:02x}", self.read8(reg::ATCTRL));
        crate::println!("[PCMCIA]   +20 MEM2S   = {:04x}", self.read16(reg::MEMWIN2_START));
        crate::println!("[PCMCIA]   +22 MEM2E   = {:04x}", self.read16(reg::MEMWIN2_END));
        crate::println!("[PCMCIA]   +24 MEM2OFF = {:04x}", self.read16(reg::MEMWIN2_OFFSET));
        crate::println!("[PCMCIA]   +28 MEM3S   = {:04x}", self.read16(reg::MEMWIN3_START));
        crate::println!("[PCMCIA]   +2a MEM3E   = {:04x}", self.read16(reg::MEMWIN3_END));
        crate::println!("[PCMCIA]   +2c MEM3OFF = {:04x}", self.read16(reg::MEMWIN3_OFFSET));
        crate::println!("[PCMCIA]   +30 MISCC1  = {:02x}", self.read8(reg::MISCC1));
        crate::println!("[PCMCIA]   +31 MEM4S   = {:04x}", self.read16(reg::MEMWIN4_START));
        crate::println!("[PCMCIA]   +33 MEM4E   = {:04x}", self.read16(reg::MEMWIN4_END));
        crate::println!("[PCMCIA]   +35 MEM4OFF = {:04x}", self.read16(reg::MEMWIN4_OFFSET));
        crate::println!("[PCMCIA]   +37 IOOFF0  = {:04x}", self.read16(reg::IO_OFFSET0));
        crate::println!("[PCMCIA]   +39 IOOFF1  = {:04x}", self.read16(reg::IO_OFFSET1));
        crate::println!("[PCMCIA]   +3b GPIO    = {:02x}", self.read8(reg::GPIO));
        crate::println!("[PCMCIA]   +40 SMPGA0  = {:02x}", self.read8(reg::SMPGA0));
    }
}
