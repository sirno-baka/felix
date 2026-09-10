//! Toshiba ToPIC100 (1179:0617) PCI/CardBus controller.
//!
//! ToPIC100 is a Yenta-compatible CardBus bridge: CardBus socket registers
//! live in BAR0 and the Intel 82365-compatible ExCA register bank starts at
//! BAR0 + 0x800.  The common PCMCIA core can therefore reuse `Pc16`; this
//! module only owns PCI/CardBus host setup and interrupt acknowledgement.

use core::ptr::{read_volatile, write_volatile};

use crate::memory::paging::{PAGING, PTEFlags};
use crate::pci;

use super::{CF_IO_BASE, CF_IO_END};
use super::controller::SocketController;
use super::pc16::Pc16;

pub const VENDOR_ID: u16 = 0x1179;
pub const DEVICE_ID: u16 = 0x0617;

const PCI_COMMAND: u8 = 0x04;
const PCI_BAR0: u8 = 0x10;
const PCI_CB_MEMORY_BASE_0: u8 = 0x1c;
const PCI_CB_MEMORY_LIMIT_0: u8 = 0x20;
const PCI_CB_IO_BASE_0: u8 = 0x2c;
const PCI_CB_IO_LIMIT_0: u8 = 0x30;
const PCI_INTERRUPT_LINE: u8 = 0x3c;

const PCI_COMMAND_IO: u16 = 1 << 0;
const PCI_COMMAND_MEMORY: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;

// Standard Yenta/CardBus socket registers in BAR0.
const CB_SOCKET_EVENT: u32 = 0x00;
const CB_SOCKET_MASK: u32 = 0x04;
const CB_SOCKET_STATE: u32 = 0x08;
const CB_SOCKET_FORCE: u32 = 0x0c;
const CB_SOCKET_CONTROL: u32 = 0x10;

// ToPIC97/100 PCI config registers.  We currently keep BIOS policy intact;
// these constants are used for diagnostics and to make ToPIC-specific setup
// explicit rather than accidentally applying Ricoh registers.
const TOPIC_SOCKET_CONTROL: u8 = 0x90;
const TOPIC_SLOT_CONTROL: u8 = 0xa0;
const TOPIC_SLOT_SLOTON: u8 = 0x80;
const TOPIC_SLOT_SLOTEN: u8 = 0x40;
const TOPIC97_INT_CONTROL: u8 = 0xa1;
const TOPIC_CARD_DETECT: u8 = 0xa3;
const TOPIC_REGISTER_CONTROL: u8 = 0xa4;
const TOPIC97_MISC1: u8 = 0xad;
const TOPIC97_MISC2: u8 = 0xae;

pub const BAR0_SIZE: u32 = 0x1000;
pub const BAR0_PHYS_FALLBACK: u32 = 0xF000_0000;
pub const BAR0_VIRT: u32 = 0xE000_0000;

#[derive(Copy, Clone)]
pub struct ToshibaTopic100 {
    pub pci: pci::device::PciDevice,
    pub bar0_phys: u32,
    pub bar0_virt: u32,
    pub bar0_size: u32,
}

impl ToshibaTopic100 {
    pub const fn new(
        pci: pci::device::PciDevice,
        bar0_phys: u32,
        bar0_virt: u32,
        bar0_size: u32,
    ) -> Self {
        Self { pci, bar0_phys, bar0_virt, bar0_size }
    }

    #[inline]
    unsafe fn mmio_read32(&self, offset: u32) -> u32 {
        read_volatile((self.bar0_virt + offset) as *const u32)
    }

    #[inline]
    unsafe fn mmio_write32(&self, offset: u32, value: u32) {
        write_volatile((self.bar0_virt + offset) as *mut u32, value);
        // Flush posted PCI write.
        let _ = read_volatile((self.bar0_virt + offset) as *const u32);
    }

    fn restore_windows(&self) {
        self.pci.write_u32(PCI_BAR0, self.bar0_phys);
        self.pci.write_u32(
            PCI_CB_MEMORY_BASE_0,
            super::CF_MEM_PHYS & 0xffff_fff0,
        );
        self.pci.write_u32(
            PCI_CB_MEMORY_LIMIT_0,
            (super::CF_MEM_PHYS + super::CF_MEM_SIZE - 1) | 0x0f,
        );
        self.pci.write_u32(
            PCI_CB_IO_BASE_0,
            (CF_IO_BASE as u32) & 0xffff_fffc,
        );
        self.pci.write_u32(
            PCI_CB_IO_LIMIT_0,
            (CF_IO_END as u32) | 0x3,
        );
    }

    fn dump_topic_config(&self) {
        crate::println!(
            "[PCMCIA] ToPIC100 cfg CMD={:04x} BAR0={:08x} SCR={:08x} SLOT={:02x} INT={:02x} CD={:02x} RCR={:08x} M1={:02x} M2={:02x}",
            self.pci.read_u16(PCI_COMMAND),
            self.pci.read_u32(PCI_BAR0),
            self.pci.read_u32(TOPIC_SOCKET_CONTROL),
            self.pci.read_u8(TOPIC_SLOT_CONTROL),
            self.pci.read_u8(TOPIC97_INT_CONTROL),
            self.pci.read_u8(TOPIC_CARD_DETECT),
            self.pci.read_u32(TOPIC_REGISTER_CONTROL),
            self.pci.read_u8(TOPIC97_MISC1),
            self.pci.read_u8(TOPIC97_MISC2),
        );
    }

    fn dump_socket_decode(&self) {
        unsafe {
            let pc16 = self.pc16();
            crate::println!(
                "[PCMCIA] ToPIC100 MMIO event={:08x} mask={:08x} state={:08x} force={:08x} ctrl={:08x}",
                self.mmio_read32(CB_SOCKET_EVENT),
                self.mmio_read32(CB_SOCKET_MASK),
                self.mmio_read32(CB_SOCKET_STATE),
                self.mmio_read32(CB_SOCKET_FORCE),
                self.mmio_read32(CB_SOCKET_CONTROL),
            );
            crate::println!(
                "[PCMCIA] ToPIC100 ExCA IDREV={:02x} IFSTAT={:02x} PWCTRL={:02x} IGCTRL={:02x} AWINEN={:02x}",
                pc16.idrev(), pc16.ifstat(), pc16.pwctrl(), pc16.igctrl(), pc16.awinen()
            );
        }
    }
}

impl SocketController for ToshibaTopic100 {
    fn name(&self) -> &'static str { "Toshiba ToPIC100 (1179:0617)" }

    fn irq_line(&self) -> u8 {
        let irq = self.pci.read_u8(PCI_INTERRUPT_LINE);
        if irq == 0 || irq == 0xff { 11 } else { irq }
    }

    fn restore_host_decode(&self) {
        let command = self.pci.read_u16(PCI_COMMAND)
            | PCI_COMMAND_IO
            | PCI_COMMAND_MEMORY
            | PCI_COMMAND_BUS_MASTER;
        self.pci.write_u16(PCI_COMMAND, command & !0x0400);
        self.restore_windows();
    }

    fn pc16(&self) -> Pc16 { Pc16::new(self.bar0_virt) }

    unsafe fn enable_csc(&self, irq: u8) {
        // Yenta Card Detect event bits (CSTSCHG/CD1/CD2) plus the ExCA
        // card-detect CSC interrupt.  ToPIC100 uses the same BAR0 layout.
        const MASK_CD: u32 = 0x0000_0007;
        let ev = self.mmio_read32(CB_SOCKET_EVENT);
        self.mmio_write32(CB_SOCKET_EVENT, ev);
        self.mmio_write32(CB_SOCKET_MASK, MASK_CD);

        let pc16 = self.pc16();
        pc16.write_reg8(super::pc16::reg::CSCHG, 0xff);
        let irq = irq & 0x0f;
        pc16.write_reg8(super::pc16::reg::CSCINT, 0x08 | (irq << 4));
        self.pci.write_u8(PCI_INTERRUPT_LINE, irq);
    }

    unsafe fn ack_csc(&self) -> (u8, u32, bool) {
        let pc16 = self.pc16();
        let cschg = pc16.cschg();
        pc16.write_reg8(super::pc16::reg::CSCHG, cschg);

        let ev = self.mmio_read32(CB_SOCKET_EVENT);
        self.mmio_write32(CB_SOCKET_EVENT, ev);

        (cschg, ev, pc16.status().card_present())
    }
}

pub fn matches(dev: &pci::device::PciDevice) -> bool {
    dev.vendor_id == VENDOR_ID && dev.device_id == DEVICE_ID
}

pub fn attach(
    dev: pci::device::PciDevice,
) -> Option<alloc::boxed::Box<dyn SocketController>> {
    Some(alloc::boxed::Box::new(setup(dev)?))
}

fn probe_bar_size(dev: &pci::device::PciDevice, original: u32) -> u32 {
    let old_command = dev.read_u16(PCI_COMMAND);
    dev.write_u16(
        PCI_COMMAND,
        old_command & !(PCI_COMMAND_IO | PCI_COMMAND_MEMORY),
    );
    dev.write_u32(PCI_BAR0, 0xffff_ffff);
    let mask = dev.read_u32(PCI_BAR0);
    dev.write_u32(PCI_BAR0, original);
    dev.write_u16(PCI_COMMAND, old_command);

    if mask == 0 || mask == 0xffff_ffff {
        return 0;
    }
    let size_mask = mask & 0xffff_fff0;
    if size_mask == 0 { 0 } else { (!size_mask).wrapping_add(1) }
}

fn map_bar0(phys: u32, size: u32) {
    let map_size = ((size as usize) + 0xfff) & !0xfff;
    let flags = PTEFlags::new().present().writable();
    let mut paging = unsafe { PAGING.lock() };
    let _ = paging.map_physical_range(phys, map_size as u32, BAR0_VIRT, flags);
}

pub fn setup(dev: pci::device::PciDevice) -> Option<ToshibaTopic100> {
    let original_bar0 = dev.read_u32(PCI_BAR0);
    let old_command = dev.read_u16(PCI_COMMAND);

    let size = match probe_bar_size(&dev, original_bar0) {
        0 => BAR0_SIZE,
        n => n,
    };

    let mut bar0_phys = original_bar0 & 0xffff_fff0;
    if bar0_phys == 0 {
        bar0_phys = BAR0_PHYS_FALLBACK;
        dev.write_u32(PCI_BAR0, bar0_phys);
    }
    bar0_phys = dev.read_u32(PCI_BAR0) & 0xffff_fff0;
    if bar0_phys == 0 {
        dev.write_u16(PCI_COMMAND, old_command);
        return None;
    }

    // ToPIC has an explicit slot gate in PCI config space.  On this Toshiba
    // BIOS the bridge is enumerated with SLOT_CONTROL=0, so BAR0/ExCA reads
    // float as 0xff until the slot is switched on and enabled.
    let slot = dev.read_u8(TOPIC_SLOT_CONTROL);
    dev.write_u8(TOPIC_SLOT_CONTROL, slot | TOPIC_SLOT_SLOTON | TOPIC_SLOT_SLOTEN);

    map_bar0(bar0_phys, size);

    let controller = ToshibaTopic100::new(dev, bar0_phys, BAR0_VIRT, size);
    controller.restore_host_decode();

    crate::println!(
        "[PCMCIA] ToPIC100 BAR0 phys={:08x} virt={:08x} size={:x} irq={}",
        bar0_phys,
        BAR0_VIRT,
        size,
        controller.irq_line(),
    );
    controller.dump_topic_config();
    controller.dump_socket_decode();

    Some(controller)
}
