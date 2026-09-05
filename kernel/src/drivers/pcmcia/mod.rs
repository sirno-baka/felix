//! PCMCIA/CardBus front-end and controller dispatch.
//!
//! Controller discovery is kept above individual controller drivers. Each
//! supported controller exposes a matcher and setup routine; the front-end
//! walks the PCI bus and dispatches to the matching controller driver.

mod ata;
mod cis;
mod controller;
mod pc16;

pub use ata::{AtaPio, IdentifyData};
pub use pc16::SocketStatus;
pub use controller::RicohR5c475;

pub const CF_IO_BASE: u16 = 0x01E0;
pub const CF_IO_END: u16 = 0x01EF;
pub const CF_MEM_PHYS: u32 = 0xF000_1000;
pub const CF_MEM_VIRT: u32 = 0xE000_1000;
pub const CF_MEM_SIZE: u32 = 0x1000;

const PCI_CLASS_BRIDGE: u8 = 0x06;
const PCI_SUBCLASS_CARD_BUS: u8 = 0x07;

/// PC Card function code from CISTPL_FUNCID.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CardType {
    Memory,
    Serial,
    Parallel,
    FixedDisk,
    Video,
    Network,
    Arcnet,
    SCSI,
    Unknown(u8),
}

impl CardType {
    pub const fn from_funcid(id: u8) -> Self {
        match id {
            0x00 => Self::Memory,
            0x01 => Self::Serial,
            0x02 => Self::Parallel,
            0x04 => Self::FixedDisk,
            0x06 => Self::Video,
            0x07 => Self::Network,
            0x08 => Self::Arcnet,
            0x09 => Self::SCSI,
            other => Self::Unknown(other),
        }
    }
}

/// Common information discovered from CIS before a card-specific driver is
/// selected.
#[derive(Copy, Clone, Debug)]
pub struct CardInfo {
    pub card_type: CardType,
    pub func_id: Option<u8>,
    pub config_base: Option<u32>,
    pub config_index: Option<u8>,
}

/// Initialized PCMCIA device selected by the front-end.
#[derive(Copy, Clone)]
pub enum PcmciaDevice {
    CompactFlash(CompactFlash),
    Unsupported(CardInfo),
}

impl PcmciaDevice {
    pub fn card_type(&self) -> CardType {
        match self {
            Self::CompactFlash(_) => CardType::FixedDisk,
            Self::Unsupported(info) => info.card_type,
        }
    }

    pub fn as_compact_flash(&self) -> Option<&CompactFlash> {
        match self {
            Self::CompactFlash(cf) => Some(cf),
            Self::Unsupported(_) => None,
        }
    }
}

/// Front-end handle for an initialized CompactFlash/ATA card.
#[derive(Copy, Clone)]
pub struct CompactFlash {
    ata: AtaPio,
    identify: IdentifyData,
}

impl CompactFlash {
    pub fn identify(&self) -> IdentifyData { self.identify }
    pub fn ata(&self) -> AtaPio { self.ata }
    pub fn sectors(&self) -> u64 { self.identify.sectors }
    pub fn supports_lba48(&self) -> bool { self.identify.lba48 }
    pub fn model(&self) -> &[u8; 40] { &self.identify.model }
}

fn print_socket_status(status: &SocketStatus) {
    crate::println!("[PCMCIA] socket:");
    crate::println!(
        "[PCMCIA]   present={} cd1={} cd2={}",
        status.card_present(), status.cd1, status.cd2
    );
    crate::println!(
        "[PCMCIA]   ready={} wp={} power_on={} gpi={}",
        status.ready, status.write_protected, status.power_on, status.gpi
    );
    crate::println!(
        "[PCMCIA]   bvd1={} bvd2={} raw={:02x}",
        status.bvd1, status.bvd2, status.raw
    );
}

fn prepare_socket(controller: &controller::RicohR5c475) -> bool {
    let pc16 = controller.pc16();

    unsafe {
        pc16.power_off();
        pc16.write_reg8(pc16::reg::IGCTRL, 0x00);
        pc16.write_reg8(pc16::reg::AWINEN, 0x00);
        pc16.write_reg8(pc16::reg::IOCTRL, 0x00);

        for (start, end, off) in [
            (pc16::reg::MEMWIN0_START, pc16::reg::MEMWIN0_END, pc16::reg::MEMWIN0_OFFSET),
            (pc16::reg::MEMWIN1_START, pc16::reg::MEMWIN1_END, pc16::reg::MEMWIN1_OFFSET),
            (pc16::reg::MEMWIN2_START, pc16::reg::MEMWIN2_END, pc16::reg::MEMWIN2_OFFSET),
            (pc16::reg::MEMWIN3_START, pc16::reg::MEMWIN3_END, pc16::reg::MEMWIN3_OFFSET),
        ] {
            pc16.write_reg16(start, 0);
            pc16.write_reg16(end, 0);
            pc16.write_reg16(off, 0);
        }

        pc16.write_reg16(pc16::reg::IOWIN0_START, 0);
        pc16.write_reg16(pc16::reg::IOWIN0_END, 0);
        pc16.write_reg16(pc16::reg::IOWIN0_OFFSET, 0);
        pc16.write_reg16(pc16::reg::IOWIN0_START + 0x04, 0);
        pc16.write_reg16(pc16::reg::IOWIN0_END + 0x04, 0);
        pc16.write_reg16(pc16::reg::IOWIN0_OFFSET + 0x04, 0);

        if !pc16.status().card_present() {
            crate::println!("[PCMCIA] no card detected");
            return false;
        }

        crate::println!("[PCMCIA] CARD DETECTED");
        crate::println!("[PCMCIA]   CD1={} CD2={}", pc16.status().cd1, pc16.status().cd2);
        crate::println!("[PCMCIA]   CSCHG={:02x} CSCINT={:02x}", pc16.cschg(), pc16.cscint());

        pc16.write_reg8(pc16::reg::PWCTRL, 0x00);
        pc16.write_reg8(pc16::reg::IGCTRL, 0x00);
        pc16.write_reg8(pc16::reg::AWINEN, 0x00);
        pc16.write_reg8(pc16::reg::MISCC1, 0x01);

        pc16.set_io_card_mode(true);
        pc16.configure_cf_attribute_window();
        pc16.configure_cf_io();
        pc16.write_reg8(pc16::reg::PWCTRL, 0xb0);

        crate::time::sleep(100);
        pc16.write_reg8(pc16::reg::IGCTRL, 0x69);
        crate::time::sleep(100);
    }

    true
}

fn init_fixed_disk(controller: &controller::RicohR5c475, info: CardInfo) -> Option<PcmciaDevice> {
    let Some(cfg) = info.config_base.zip(info.config_index) else {
        crate::println!("[PCMCIA] fixed-disk card has no CONFIG tuple");
        return None;
    };

    if !cis::configure_card(&controller.pc16(), cfg.0, cfg.1) {
        crate::println!("[PCMCIA] fixed-disk configuration failed");
        return None;
    }

    let ata = AtaPio::new(CF_IO_BASE);
    let identify = ata.identify()?;

    crate::println!("[PCMCIA] fixed-disk device online");
    Some(PcmciaDevice::CompactFlash(CompactFlash { ata, identify }))
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ControllerKind {
    RicohR5c475,
}

impl ControllerKind {
    fn name(self) -> &'static str {
        match self {
            Self::RicohR5c475 => "Ricoh R5C475/R5C475II",
        }
    }
}

fn match_controller(dev: &crate::pci::device::PciDevice) -> Option<ControllerKind> {
    // Keep controller matching in the PCMCIA front-end. Individual drivers
    // only describe which PCI IDs they support and how to initialize them.
    match (dev.vendor_id, dev.device_id) {
        (controller::VENDOR_ID, controller::DEVICE_ID)
        if dev.class_code == PCI_CLASS_BRIDGE
            && dev.subclass == PCI_SUBCLASS_CARD_BUS =>
            {
                Some(ControllerKind::RicohR5c475)
            }
        _ => None,
    }
}

fn setup_controller(
    kind: ControllerKind,
    dev: crate::pci::device::PciDevice,
) -> Option<controller::RicohR5c475> {
    match kind {
        ControllerKind::RicohR5c475 => controller::setup(dev),
    }
}

fn find_controller() -> Option<(ControllerKind, crate::pci::device::PciDevice)> {
    for dev in crate::pci::enumerate().into_iter() {
        // PCMCIA/CardBus controllers are PCI class 06:07. Only those devices
        // are offered to our controller-driver table.
        if dev.class_code != PCI_CLASS_BRIDGE || dev.subclass != PCI_SUBCLASS_CARD_BUS {
            continue;
        }

        crate::println!(
            "[PCMCIA] controller candidate {:02x}:{:02x}.{} [{:04x}:{:04x}] class={:02x}:{:02x}:{:02x}",
            dev.bus, dev.device, dev.function,
            dev.vendor_id, dev.device_id,
            dev.class_code, dev.subclass, dev.prog_if
        );

        if let Some(kind) = match_controller(&dev) {
            crate::println!("[PCMCIA] matched controller driver: {}", kind.name());
            return Some((kind, dev));
        }
    }

    crate::println!("[PCMCIA] no supported PCMCIA/CardBus controller found");
    None
}

/// Initialize PCMCIA/CardBus by first discovering a supported PCI controller,
/// then inspecting the card CIS and dispatching to a card driver.
pub fn init() -> Option<PcmciaDevice> {
    let (kind, dev) = find_controller()?;

    crate::println!(
        "[PCMCIA] using {} at {:02x}:{:02x}.{}",
        kind.name(), dev.bus, dev.device, dev.function
    );

    let controller = setup_controller(kind, dev)?;
    unsafe {
        crate::println!(
            "[PCMCIA] PC16: phys=0x{:08x} virt=0x{:08x}",
            controller.bar0_phys + 0x800,
            controller.bar0_virt + 0x800
        );
        crate::println!(
            "[PCMCIA] PC16 initial: IDREV={:02x} IFSTAT={:02x} PWCTRL={:02x} IGCTRL={:02x} AWINEN={:02x}",
            controller.pc16().idrev(), controller.pc16().ifstat(),
            controller.pc16().pwctrl(), controller.pc16().igctrl(), controller.pc16().awinen()
        );
        print_socket_status(&controller.pc16().status());
    }

    if !prepare_socket(&controller) {
        return None;
    }

    cis::map_attribute_memory();
    let info = cis::read_cis()?;

    crate::println!(
        "[PCMCIA] CIS card type={:?} FUNCID={:?} CONFIG={:?} CFTABLE={:?}",
        info.card_type, info.func_id, info.config_base, info.config_index
    );

    match info.card_type {
        CardType::FixedDisk => init_fixed_disk(&controller, info),
        _ => {
            crate::println!("[PCMCIA] no card driver for type {:?}", info.card_type);
            Some(PcmciaDevice::Unsupported(info))
        }
    }
}

// /// Compatibility probe for callers that only want socket/card detection.
// pub fn probe() {
//     let _ = init();
// }
//
// pub fn socket_status() -> Option<SocketStatus> {
//     let dev = controller::find()?;
//     let bar0 = dev.read_u32(0x10) & 0xFFFF_FFF0;
//     if bar0 == 0 { return None; }
//     unsafe { Some(pc16::Pc16::new(controller::BAR0_VIRT).status()) }
// }
//
// pub fn card_present() -> bool {
//     socket_status().map(|s| s.card_present()).unwrap_or(false)
// }
//
// pub fn power_off() {
//     unsafe {
//         let pc16 = pc16::Pc16::new(controller::BAR0_VIRT);
//         pc16.power_off();
//         crate::println!("[PCMCIA] socket power OFF, PWCTRL={:02x}", pc16.pwctrl());
//     }
// }
//
// pub fn power_3v3() {
//     unsafe {
//         let pc16 = pc16::Pc16::new(controller::BAR0_VIRT);
//         pc16.power_3v3();
//         crate::println!("[PCMCIA] socket power 3.3V, PWCTRL={:02x} IFSTAT={:02x}", pc16.pwctrl(), pc16.ifstat());
//     }
// }
//
// pub fn power_5v() {
//     unsafe {
//         let pc16 = pc16::Pc16::new(controller::BAR0_VIRT);
//         pc16.power_5v();
//         crate::println!("[PCMCIA] socket power 5V, PWCTRL={:02x} IFSTAT={:02x}", pc16.pwctrl(), pc16.ifstat());
//     }
// }
//
// pub fn reset_assert() {
//     unsafe {
//         let pc16 = pc16::Pc16::new(controller::BAR0_VIRT);
//         pc16.card_reset_assert();
//         crate::println!("[PCMCIA] card RESET asserted, IGCTRL={:02x}", pc16.igctrl());
//     }
// }
//
// pub fn reset_deassert() {
//     unsafe {
//         let pc16 = pc16::Pc16::new(controller::BAR0_VIRT);
//         pc16.card_reset_deassert();
//         crate::println!("[PCMCIA] card RESET deasserted, IGCTRL={:02x}", pc16.igctrl());
//     }
// }
//
// pub fn set_io_mode() {
//     unsafe {
//         let pc16 = pc16::Pc16::new(controller::BAR0_VIRT);
//         pc16.set_io_card_mode(true);
//         crate::println!("[PCMCIA] I/O-card mode enabled, IGCTRL={:02x}", pc16.igctrl());
//     }
// }
//
// pub fn configure_io_16bit() {
//     unsafe {
//         let pc16 = pc16::Pc16::new(controller::BAR0_VIRT);
//         let ioctl = pc16.ioctrl() | pc16::ioctrl::IO0_16BIT | pc16::ioctrl::IO1_16BIT;
//         pc16.write_reg8(pc16::reg::IOCTRL, ioctl);
//         crate::println!("[PCMCIA] 16-bit I/O configured, IOCTRL={:02x}", pc16.ioctrl());
//     }
// }

