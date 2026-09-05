//! Ricoh R5C475II PCI -> PC Card / CardBus controller.
//!
//! The R5C475II exposes the legacy 16-bit PC Card socket registers at BAR0
//! + 0x800.  The layout is compatible with the classic 82365-style ExCA
//! register block.  This file intentionally stops before CIS parsing and
//! before configuring a card into ATA/CF mode.
//!
//! References used while implementing this first layer:
//! - Ricoh R5C476II application note (the R5C475II is register-compatible
//!   with the 16-bit socket interface): ExCA/socket registers.
//! - coreboot's Ricoh RL5C476 support, which places the 16-bit control
//!   structure at BAR0 + 0x800 and uses pwctrl/igctrl/awinen.

use core::ptr::{read_volatile, write_volatile};

use crate::memory::paging::KERNEL_OFFSET;
use crate::pci::{self, bar::Bar, device::PciDevice};

/// Ricoh PCI vendor ID.
pub const RICOH_VENDOR_ID: u16 = 0x1180;

/// Ricoh R5C475II PCI device ID.
pub const R5C475II_DEVICE_ID: u16 = 0x0475;

/// The legacy 16-bit PC Card control block starts here in the controller BAR.
pub const PC16_BASE_OFFSET: u32 = 0x0800;

/// Minimum register space needed for the socket control block.
pub const PC16_REG_SPACE: u32 = 0x0100;

/// ExCA-compatible 16-bit socket register offsets.
///
/// The first registers are common to 82365-compatible PCMCIA controllers.
pub mod reg {
    pub const IDREV: u32 = 0x00;
    pub const IFSTAT: u32 = 0x01;
    pub const PWCTRL: u32 = 0x02;
    pub const IGCTRL: u32 = 0x03;
    pub const CSCHG: u32 = 0x04;
    pub const CSCINT: u32 = 0x05;
    pub const AWINEN: u32 = 0x06;
    pub const IOCTRL: u32 = 0x07;

    pub const IOSTL0: u32 = 0x08;
    pub const IOSTH0: u32 = 0x09;
    pub const IOSPL0: u32 = 0x0A;
    pub const IOSPH0: u32 = 0x0B;

    pub const IOSTL1: u32 = 0x0C;
    pub const IOSTH1: u32 = 0x0D;
    pub const IOSPL1: u32 = 0x0E;
    pub const IOSPH1: u32 = 0x0F;

    pub const SMSTL0: u32 = 0x10;
    pub const SMSTH0: u32 = 0x11;
    pub const SMSPL0: u32 = 0x12;
    pub const SMSPH0: u32 = 0x13;
    pub const MOFFL0: u32 = 0x14;
    pub const MOFFH0: u32 = 0x15;

    pub const CDGENC: u32 = 0x16;
    pub const SMSTL1: u32 = 0x18;
    pub const SMSTH1: u32 = 0x19;
    pub const SMSPL1: u32 = 0x1A;
    pub const SMSPH1: u32 = 0x1B;
    pub const MOFFL1: u32 = 0x1C;
    pub const MOFFH1: u32 = 0x1D;

    pub const GLCTRL: u32 = 0x1E;
    pub const ATCTRL: u32 = 0x1F;

    pub const MISCC1: u32 = 0x37;
    pub const IOFFL0: u32 = 0x38;
    pub const IOFFH0: u32 = 0x39;
    pub const IOFFL1: u32 = 0x3A;
    pub const IOFFH1: u32 = 0x3B;
    pub const GPIO: u32 = 0x3C;
    pub const SMPGA0: u32 = 0x48;
}

/// Common IFSTAT bits from the 82365-compatible socket interface.
pub mod ifstat {
    /// Card-detect pins are both active when a card is inserted.
    pub const CD1: u8 = 1 << 0;
    pub const CD2: u8 = 1 << 1;
    /// Write-protect input.
    pub const WP: u8 = 1 << 4;
    /// Card READY/BUSY indication.
    pub const READY: u8 = 1 << 5;
    /// Battery voltage detect 1.
    pub const BVD1: u8 = 1 << 6;
    /// Battery voltage detect 2.
    pub const BVD2: u8 = 1 << 7;
}

/// A decoded snapshot of the socket state.
#[derive(Debug, Clone, Copy)]
pub struct SocketStatus {
    pub raw: u8,
    pub card_detect_1: bool,
    pub card_detect_2: bool,
    pub ready: bool,
    pub write_protected: bool,
    pub bvd1: bool,
    pub bvd2: bool,
}

impl SocketStatus {
    pub fn from_raw(raw: u8) -> Self {
        Self {
            raw,
            card_detect_1: (raw & ifstat::CD1) != 0,
            card_detect_2: (raw & ifstat::CD2) != 0,
            ready: (raw & ifstat::READY) != 0,
            write_protected: (raw & ifstat::WP) != 0,
            bvd1: (raw & ifstat::BVD1) != 0,
            bvd2: (raw & ifstat::BVD2) != 0,
        }
    }

    /// On the classic PC Card interface both CD inputs must indicate the
    /// inserted state.  Keep this as a raw interpretation for now; the
    /// controller-specific polarity is handled by the hardware interface.
    pub fn card_present(&self) -> bool {
        self.card_detect_1 && self.card_detect_2
    }
}

/// Memory-mapped access to the R5C475II's 16-bit socket control block.
pub struct Socket {
    base: *mut u8,
}

impl Socket {
    /// # Safety
    /// `base` must be a valid, mapped, uncached MMIO address for BAR0+0x800.
    unsafe fn new(base: u32) -> Self {
        Self {
            base: base as *mut u8,
        }
    }

    #[inline]
    unsafe fn read8(&self, offset: u32) -> u8 {
        read_volatile(self.base.add(offset as usize))
    }

    #[inline]
    unsafe fn write8(&self, offset: u32, value: u8) {
        write_volatile(self.base.add(offset as usize), value);
    }

    #[inline]
    pub unsafe fn id_revision(&self) -> u8 {
        self.read8(reg::IDREV)
    }

    #[inline]
    pub unsafe fn status_raw(&self) -> u8 {
        self.read8(reg::IFSTAT)
    }

    #[inline]
    pub unsafe fn status(&self) -> SocketStatus {
        SocketStatus::from_raw(self.status_raw())
    }

    #[inline]
    pub unsafe fn power_control(&self) -> u8 {
        self.read8(reg::PWCTRL)
    }

    #[inline]
    pub unsafe fn interrupt_control(&self) -> u8 {
        self.read8(reg::IGCTRL)
    }

    #[inline]
    pub unsafe fn address_window_enable(&self) -> u8 {
        self.read8(reg::AWINEN)
    }

    /// Put the socket into a known safe baseline.
    ///
    /// This is deliberately conservative: no card power is enabled and no
    /// memory/I/O window is exposed.  It is therefore safe to call during the
    /// first probe before we know what card is inserted.
    pub unsafe fn disable_socket(&self) {
        self.write8(reg::PWCTRL, 0x00);
        self.write8(reg::IGCTRL, 0x00);
        self.write8(reg::AWINEN, 0x00);
    }

    /// Set the legacy 82365-compatible socket interrupt/control byte.
    ///
    /// Kept as a raw operation intentionally.  The exact IRQ/IO-card policy
    /// will be selected after CIS/card-type detection.
    pub unsafe fn set_interrupt_control(&self, value: u8) {
        self.write8(reg::IGCTRL, value);
    }

    /// Set the socket power-control byte.
    ///
    /// This is not called automatically.  In the next layer we will derive
    /// the correct 3.3V/5V policy from card identification before writing it.
    pub unsafe fn set_power_control(&self, value: u8) {
        self.write8(reg::PWCTRL, value);
    }

    /// Enable/disable the PC Card address windows.
    pub unsafe fn set_address_window_enable(&self, value: u8) {
        self.write8(reg::AWINEN, value);
    }
}

/// Convert a physical PCI MMIO address into the kernel higher-half mapping.
///
/// Felix currently maps the physical PCI/MMIO low address space through the
/// same 0xC0000000 higher-half offset used by the kernel.
#[inline]
fn mmio_virt(phys: u32) -> Option<u32> {
    phys.checked_add(KERNEL_OFFSET)
}

fn find_controller() -> Option<PciDevice> {
    pci::find_device(RICOH_VENDOR_ID, R5C475II_DEVICE_ID)
}

/// Probe and initialize the R5C475II socket controller.
///
/// Returns nothing for now because this is a kernel bring-up driver.  All
/// useful information is printed so the first real-machine test can tell us
/// whether the PCI function and socket registers are mapped correctly.
pub fn probe() {
    crate::println!("[PCMCIA] probing Ricoh R5C475II...");

    let Some(dev) = find_controller() else {
        crate::println!("[PCMCIA] R5C475II [{:04x}:{:04x}] not found", RICOH_VENDOR_ID, R5C475II_DEVICE_ID);
        return;
    };

    crate::println!(
        "[PCMCIA] found {:02x}:{:02x}.{} [{:04x}:{:04x}] rev {:02x} class {:02x}:{:02x}:{:02x}",
        dev.bus,
        dev.device,
        dev.function,
        dev.vendor_id,
        dev.device_id,
        dev.revision_id,
        dev.class_code,
        dev.subclass,
        dev.prog_if
    );

    crate::println!(
        "[PCMCIA] IRQ={} pin={} command={:04x} status={:04x}",
        dev.interrupt_line,
        dev.interrupt_pin,
        dev.command,
        dev.status
    );

    let Some(bar0) = dev.get_bar(0) else {
        crate::println!("[PCMCIA] ERROR: BAR0 is not present");
        return;
    };

    let Some(bar_phys) = bar0.address() else {
        crate::println!("[PCMCIA] ERROR: BAR0 has no address");
        return;
    };

    crate::println!(
        "[PCMCIA] BAR0={} phys=0x{:08x} size=0x{:x}",
        if bar0.is_memory() { "MMIO" } else { "I/O" },
        bar_phys,
        bar0.size()
    );

    if !bar0.is_memory() {
        crate::println!("[PCMCIA] ERROR: BAR0 is not a memory BAR");
        return;
    }

    if bar0.size() < PC16_BASE_OFFSET + PC16_REG_SPACE {
        crate::println!(
            "[PCMCIA] ERROR: BAR0 too small for PC16 block (need >= 0x{:x})",
            PC16_BASE_OFFSET + PC16_REG_SPACE
        );
        return;
    }

    // The bridge must decode its MMIO BAR before the socket registers can be
    // accessed.  We do not enable bus mastering yet: no DMA is needed here.
    dev.enable_memory_space();

    let Some(pc16_phys) = bar_phys.checked_add(PC16_BASE_OFFSET) else {
        crate::println!("[PCMCIA] ERROR: BAR0 address overflow");
        return;
    };

    let Some(pc16_virt) = mmio_virt(pc16_phys) else {
        crate::println!("[PCMCIA] ERROR: cannot form higher-half MMIO address");
        return;
    };

    crate::println!(
        "[PCMCIA] PC16 regs phys=0x{:08x} virt=0x{:08x}",
        pc16_phys,
        pc16_virt
    );

    let socket = unsafe { Socket::new(pc16_virt) };

    unsafe {
        crate::println!(
            "[PCMCIA] PC16 id/rev={:02x} ifstat={:02x} pwctrl={:02x} igctrl={:02x} awinen={:02x}",
            socket.id_revision(),
            socket.status_raw(),
            socket.power_control(),
            socket.interrupt_control(),
            socket.address_window_enable()
        );

        // Establish the safe baseline.  This does NOT power the card.
        socket.disable_socket();

        let status = socket.status();
        crate::println!(
            "[PCMCIA] socket: present={} cd1={} cd2={} ready={} wp={} bvd1={} bvd2={} raw={:02x}",
            status.card_present(),
            status.card_detect_1,
            status.card_detect_2,
            status.ready,
            status.write_protected,
            status.bvd1,
            status.bvd2,
            status.raw
        );
    }

    crate::println!("[PCMCIA] R5C475II socket baseline initialized (power OFF)");
}
