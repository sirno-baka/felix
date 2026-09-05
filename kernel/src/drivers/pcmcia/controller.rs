//! Ricoh R5C475II PCI/CardBus controller setup and diagnostics.

use core::ptr::{read_volatile, write_volatile};

use crate::memory::paging::{PAGING, PTEFlags};
use crate::pci;

use super::pc16::Pc16;

pub const VENDOR_ID: u16 = 0x1180;
pub const DEVICE_ID: u16 = 0x0475;

const PCI_COMMAND: u8 = 0x04;
const PCI_BAR0: u8 = 0x10;
const PCI_INTERRUPT_LINE: u8 = 0x3C;
const PCI_INTERRUPT_PIN: u8 = 0x3D;
const PCI_BRIDGE_CONTROL: u8 = 0x3E;
const PCI_CB_MEMORY_BASE_0: u8 = 0x1C;
const PCI_CB_MEMORY_LIMIT_0: u8 = 0x20;
const PCI_CB_IO_BASE_0: u8 = 0x2C;
const PCI_CB_IO_LIMIT_0: u8 = 0x30;
const PCI_RICOH_MISC_CONTROL: u8 = 0x82;

const PCI_COMMAND_IO: u16 = 1 << 0;
const PCI_COMMAND_MEMORY: u16 = 1 << 1;

pub const BAR0_SIZE: u32 = 0x1000;
pub const BAR0_PHYS_FALLBACK: u32 = 0xF000_0000;
pub const BAR0_VIRT: u32 = 0xE000_0000;

const CB_SOCKET_EVENT: u32 = 0x00;
const CB_SOCKET_MASK: u32 = 0x04;
const CB_SOCKET_STATE: u32 = 0x08;
const CB_SOCKET_FORCE: u32 = 0x0C;
const CB_SOCKET_CONTROL: u32 = 0x10;
const CB_SOCKET_POWER: u32 = 0x20;

#[derive(Copy, Clone)]
pub struct RicohR5c475 {
    pub pci: pci::device::PciDevice,
    pub bar0_phys: u32,
    pub bar0_virt: u32,
    pub bar0_size: u32,
}

impl RicohR5c475 {
    pub const fn new(pci: pci::device::PciDevice, bar0_phys: u32, bar0_virt: u32, bar0_size: u32) -> Self {
        Self { pci, bar0_phys, bar0_virt, bar0_size }
    }

    pub fn pc16(&self) -> Pc16 { Pc16::new(self.bar0_virt) }

    #[inline]
    unsafe fn mmio_read32(&self, offset: u32) -> u32 {
        read_volatile((self.bar0_virt + offset) as *const u32)
    }

    pub unsafe fn dump_cardbus_socket(&self) {
        unsafe {
            crate::println!("[PCMCIA] CardBus socket registers:");
            crate::println!("[PCMCIA]   +00 SOCKET_EVENT   = {:08x}", self.mmio_read32(CB_SOCKET_EVENT));
            crate::println!("[PCMCIA]   +04 SOCKET_MASK    = {:08x}", self.mmio_read32(CB_SOCKET_MASK));
            crate::println!("[PCMCIA]   +08 SOCKET_STATE   = {:08x}", self.mmio_read32(CB_SOCKET_STATE));
            crate::println!("[PCMCIA]   +0c SOCKET_FORCE   = {:08x}", self.mmio_read32(CB_SOCKET_FORCE));
            crate::println!("[PCMCIA]   +10 SOCKET_CONTROL = {:08x}", self.mmio_read32(CB_SOCKET_CONTROL));
            crate::println!("[PCMCIA]   +20 SOCKET_POWER   = {:08x}", self.mmio_read32(CB_SOCKET_POWER));
        }
    }

    pub fn dump_pci_config(&self) {
        crate::println!("[PCMCIA] PCI config dump:");
        let mut offset = 0u8;
        while offset < 0x40 {
            crate::println!("[PCMCIA]   {:02x}: {:08x}", offset, self.pci.read_u32(offset));
            offset += 4;
        }
        crate::println!("[PCMCIA] PCI command     = {:04x}", self.pci.read_u16(PCI_COMMAND));
        crate::println!("[PCMCIA] IRQ line/pin    = {:02x}/{:02x}", self.pci.read_u8(PCI_INTERRUPT_LINE), self.pci.read_u8(PCI_INTERRUPT_PIN));
        crate::println!("[PCMCIA] Bridge control  = {:04x}", self.pci.read_u16(PCI_BRIDGE_CONTROL));
        crate::println!("[PCMCIA] Ricoh misc 0x82 = {:04x}", self.pci.read_u16(PCI_RICOH_MISC_CONTROL));
    }

    pub fn dump(&self) {
        unsafe { self.dump_cardbus_socket(); }
        unsafe { self.pc16().dump(); }
    }
}

fn probe_bar_size(dev: &pci::device::PciDevice, offset: u8, original: u32) -> u32 {
    let old_command = dev.read_u16(PCI_COMMAND);
    dev.write_u16(PCI_COMMAND, old_command & !(PCI_COMMAND_IO | PCI_COMMAND_MEMORY));
    dev.write_u32(offset, 0xFFFF_FFFF);
    let mask = dev.read_u32(offset);
    dev.write_u32(offset, original);
    dev.write_u16(PCI_COMMAND, old_command);
    crate::println!("[PCMCIA] BAR sizing: original={:08x} mask={:08x}", original, mask);
    if mask == 0 || mask == 0xFFFF_FFFF { return 0; }
    let size_mask = if (mask & 1) != 0 { mask & 0xFFFF_FFFC } else { mask & 0xFFFF_FFF0 };
    if size_mask == 0 { 0 } else { (!size_mask).wrapping_add(1) }
}

fn map_bar0(phys: u32, size: u32) {
    let map_size = ((size as usize) + 0xFFF) & !0xFFF;
    crate::println!("[PCMCIA] mapping MMIO phys=0x{:08x} size=0x{:x} -> virt=0x{:08x}", phys, map_size, BAR0_VIRT);
    let flags = PTEFlags::new().present().writable();
    let mut paging = unsafe { PAGING.lock() };
    let _ = paging.map_physical_range(phys, map_size as u32, BAR0_VIRT, flags);
}

pub fn setup(dev: pci::device::PciDevice) -> Option<RicohR5c475> {
    let original_bar0 = dev.read_u32(PCI_BAR0);
    let old_command = dev.read_u16(PCI_COMMAND);
    dev.write_u16(PCI_COMMAND, old_command & !(PCI_COMMAND_IO | PCI_COMMAND_MEMORY));

    let size = match probe_bar_size(&dev, PCI_BAR0, original_bar0) {
        0 => BAR0_SIZE,
        n => n,
    };

    let mut bar0_phys = original_bar0 & 0xFFFF_FFF0;
    if bar0_phys == 0 {
        bar0_phys = BAR0_PHYS_FALLBACK;
        crate::println!("[PCMCIA] BAR0 unassigned -> 0x{:08x}", bar0_phys);
        dev.write_u32(PCI_BAR0, bar0_phys);
    }
    bar0_phys = dev.read_u32(PCI_BAR0) & 0xFFFF_FFF0;
    if bar0_phys == 0 {
        dev.write_u16(PCI_COMMAND, old_command);
        return None;
    }

    dev.write_u16(PCI_COMMAND, old_command | 0x0002);
    dev.write_u16(PCI_RICOH_MISC_CONTROL, 0x00a0);
    dev.write_u16(PCI_BRIDGE_CONTROL, 0x0780);

    map_bar0(bar0_phys, size);
    let controller = RicohR5c475::new(dev, bar0_phys, BAR0_VIRT, size);

    unsafe {
        crate::println!(
            "[PCMCIA] MMIO verify: +00={:08x} +04={:08x} +08={:08x}",
            controller.mmio_read32(0),
            controller.mmio_read32(4),
            controller.mmio_read32(CB_SOCKET_STATE)
        );
        let pc16 = controller.pc16();
        crate::println!("[PCMCIA] PC16 verify: IDREV={:02x} IFSTAT={:02x}", pc16.idrev(), pc16.ifstat());
    }

    dev.write_u32(PCI_CB_MEMORY_BASE_0, super::CF_MEM_PHYS & 0xfffffff0);
    dev.write_u32(PCI_CB_MEMORY_LIMIT_0, (super::CF_MEM_PHYS + super::CF_MEM_SIZE - 1) | 0x0f);
    dev.write_u32(PCI_CB_IO_BASE_0, (super::CF_IO_BASE as u32) & 0xffff_fffc);
    dev.write_u32(PCI_CB_IO_LIMIT_0, (super::CF_IO_END as u32) | 0x3);

    crate::println!("[PCMCIA] CardBus MEM0: {:08x}-{:08x}", dev.read_u32(PCI_CB_MEMORY_BASE_0), dev.read_u32(PCI_CB_MEMORY_LIMIT_0));
    crate::println!("[PCMCIA] CardBus IO0:  {:08x}-{:08x}", dev.read_u32(PCI_CB_IO_BASE_0), dev.read_u32(PCI_CB_IO_LIMIT_0));

    Some(controller)
}
