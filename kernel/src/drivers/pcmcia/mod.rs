//! PCMCIA/CardBus front-end and controller dispatch.
//!
//! Controller discovery is kept above individual controller drivers. Each
//! supported controller exposes a matcher and setup routine; the front-end
//! walks the PCI bus and dispatches to the matching controller driver.

mod ata;
mod cis;
mod controller;
mod driver;
mod pc16;

pub use ata::{AtaPio, IdentifyData};
pub use pc16::SocketStatus;
pub use controller::{RicohR5c475, SocketController};

pub const CF_IO_BASE: u16 = 0xC000;
pub const CF_IO_END: u16 = 0xC00F;
pub const CF_MEM_PHYS: u32 = 0xF000_1000;
pub const CF_MEM_VIRT: u32 = 0xE000_1000;
pub const CF_MEM_SIZE: u32 = 0x1000;

static mut SOCKET: Option<alloc::boxed::Box<dyn controller::SocketController>> = None;
static mut CARD_COR: Option<(u32, u8)> = None;

/// Card currently owned by the PCMCIA core. Keeping the selected driver and
/// CIS identity here makes insert/remove symmetric and avoids probing a new
/// card on removal just to discover what used to be attached.
static ACTIVE_CARD: Mutex<Option<(CardInfo, &'static str)>> = Mutex::new(None);

pub fn active_card() -> Option<CardInfo> {
    ACTIVE_CARD.lock().as_ref().map(|(info, _)| *info)
}

pub(crate) fn store_card_cor(base: u32, index: u8) {
    unsafe { CARD_COR = Some((base, index)); }
}

/// Re-enable PCI I/O + ExCA windows if the task-file port floated (0xFF).
pub fn rearm_io() {
    unsafe {
        let Some(c) = SOCKET.as_ref() else {
            crate::println!("[PCMCIA] rearm: no socket");
            return;
        };
        c.restore_host_decode();
        let pc16 = c.pc16();
        pc16.write_reg8(pc16::reg::PWCTRL, 0xb0);
        // pulse RESET then IOCARD+IRQ like initial bring-up
        pc16.write_reg8(pc16::reg::IGCTRL, 0x69);
        crate::time::sleep(2);
        pc16.write_reg8(pc16::reg::IGCTRL, 0x29);
        pc16.set_io_card_mode(true);
        pc16.configure_cf_attribute_window();
        pc16.configure_cf_io();
        if let Some((base, idx)) = CARD_COR {
            let _ = cis::configure_card(&pc16, base, idx);
        }
        let st = crate::io::inb(CF_IO_BASE + 7);
        crate::println!(
            "[PCMCIA] rearm PWCTRL={:02x} IGCTRL={:02x} AWINEN={:02x} IOCTRL={:02x} tf={:02x}",
            pc16.pwctrl(),
            pc16.igctrl(),
            pc16.awinen(),
            pc16.ioctrl(),
            st
        );
    }
}

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use crate::drivers::pic::PICS;
use crate::sync::mutex::Mutex;
use crate::interrupts::idt::IDT;

static IRQ_LINE: AtomicU8 = AtomicU8::new(0xFF);
static PENDING: AtomicU8 = AtomicU8::new(0); // 1=insert 2=remove
static CARD_LIVE: AtomicBool = AtomicBool::new(false);
static DEBOUNCE: AtomicU8 = AtomicU8::new(0);

const EVT_NONE: u8 = 0;
const EVT_INSERT: u8 = 1;
const EVT_REMOVE: u8 = 2;

pub fn card_live() -> bool {
    CARD_LIVE.load(Ordering::Relaxed)
}

pub fn set_card_live(v: bool) {
    CARD_LIVE.store(v, Ordering::Relaxed);
}

#[unsafe(naked)]
pub extern "C" fn pcmcia_irq_stub() {
    unsafe {
        core::arch::naked_asm!(
            "cli",
            "pusha",
            "call {handler}",
            "popa",
            "iretd",
            handler = sym pcmcia_irq_handler,
        );
    }
}

#[unsafe(no_mangle)]
extern "C" fn pcmcia_irq_handler() {
    unsafe {
        if let Some(c) = SOCKET.as_ref() {
            let (cschg, ev, present) = c.ack_csc();
            let evt = if present { EVT_INSERT } else { EVT_REMOVE };
            PENDING.store(evt, Ordering::Relaxed);
            DEBOUNCE.store(20, Ordering::Relaxed); // ~20 timer ticks
            crate::println!(
                "[PCMCIA] CSC irq cschg={:02x} ev={:08x} present={}",
                cschg, ev, present
            );
        }
        let irq = IRQ_LINE.load(Ordering::Relaxed);
        if irq < 16 {
            PICS.end_interrupt(32 + irq);
        }
    }
}

/// Enable Card Detect IRQ. Call once after socket init.
pub fn enable_hotplug() {
    unsafe {
        let Some(c) = SOCKET.as_ref() else { return };
        let irq = c.irq_line();
        IRQ_LINE.store(irq, Ordering::Relaxed);
        c.restore_host_decode();
        c.enable_csc(irq);
        let vec = 32u8.wrapping_add(irq);
        IDT.add(vec as usize, pcmcia_irq_stub as u32);
        PICS.unmask_irq(irq);
        CARD_LIVE.store(c.pc16().status().card_present(), Ordering::Relaxed);
        crate::println!("[PCMCIA] hotplug IRQ{} vec={} CSCINT={:02x}", irq, vec, c.pc16().cscint());
    }
}

/// Deferred insert/remove. Safe to call from timer.
pub fn poll_hotplug() {
    unsafe {
        let Some(c) = SOCKET.as_ref() else { return };
        let present = c.pc16().status().card_present();
        let live = CARD_LIVE.load(Ordering::Relaxed);
        let irq_evt = PENDING.load(Ordering::Relaxed) != EVT_NONE;

        let mut left = DEBOUNCE.load(Ordering::Relaxed);
        if irq_evt || present != live {
            if left == 0 {
                DEBOUNCE.store(20, Ordering::Relaxed);
                return;
            }
            left = left.saturating_sub(1);
            DEBOUNCE.store(left, Ordering::Relaxed);
            if left != 0 {
                return;
            }
        } else {
            if left != 0 {
                DEBOUNCE.store(0, Ordering::Relaxed);
            }
            return;
        }

        PENDING.store(EVT_NONE, Ordering::Relaxed);
        let irq = IRQ_LINE.load(Ordering::Relaxed);
        if !present && live {
            crate::println!("[PCMCIA] card removed");
            CARD_LIVE.store(false, Ordering::Relaxed);
            c.enable_csc(irq);
            disconnect_card(c.as_ref());
        } else if present && !live {
            crate::println!("[PCMCIA] card inserted");
            c.enable_csc(irq);
            if let Some(device) = bind_card() {
                CARD_LIVE.store(true, Ordering::Relaxed);
                crate::filesystem::init::pcmcia_hotplug_device(device);
            }
        }
    }
}

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

impl crate::disk::interface::BlockDevice for CompactFlash {
    fn read_sectors(
        &self,
        numsects: u8,
        lba: u32,
        buf: u32,
    ) -> Result<(), u8> {
        self.ata.read_sectors(numsects, lba, buf)
    }

    fn write_sectors(
        &mut self,
        numsects: u8,
        lba: u32,
        buf: u32,
    ) -> Result<(), u8> {
        self.ata.write_sectors(numsects, lba, buf)
    }

    fn sector_size(&self) -> u32 {
        self.ata.sector_size()
    }
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

fn prepare_socket(controller: &dyn controller::SocketController) -> bool {
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
        pc16.write_reg8(pc16::reg::IGCTRL, 0x69); // IOCARD + RESET + IRQ9
        crate::time::sleep(50);
        pc16.write_reg8(pc16::reg::IGCTRL, 0x29); // RESET off
        crate::time::sleep(100);
    }

    true
}

fn init_fixed_disk(controller: &dyn controller::SocketController, info: CardInfo) -> Option<PcmciaDevice> {
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

fn attach_controller(
    dev: crate::pci::device::PciDevice,
) -> Option<alloc::boxed::Box<dyn controller::SocketController>> {
    driver::controllers()
        .iter()
        .find(|d| (d.matches)(&dev))
        .and_then(|d| (d.attach)(dev))
}

fn find_controller() -> Option<alloc::boxed::Box<dyn controller::SocketController>> {
    for dev in crate::pci::enumerate().into_iter() {
        if dev.class_code != PCI_CLASS_BRIDGE || dev.subclass != PCI_SUBCLASS_CARD_BUS {
            continue;
        }
        crate::println!(
            "[PCMCIA] controller candidate {:02x}:{:02x}.{} [{:04x}:{:04x}] class={:02x}:{:02x}:{:02x}",
            dev.bus, dev.device, dev.function,
            dev.vendor_id, dev.device_id,
            dev.class_code, dev.subclass, dev.prog_if
        );
        if let Some(ctrl) = attach_controller(dev) {
            crate::println!("[PCMCIA] matched controller driver: {}", ctrl.name());
            return Some(ctrl);
        }
    }
    crate::println!("[PCMCIA] no supported PCMCIA/CardBus controller found");
    None
}

/// Initialize PCMCIA/CardBus by first discovering a supported PCI controller,
/// then inspecting the card CIS and dispatching to a card driver.
pub fn init() -> Option<PcmciaDevice> {
    let controller = find_controller()?;
    crate::println!("[PCMCIA] using {}", controller.name());
    unsafe { SOCKET = Some(controller); }
    enable_hotplug();
    // unsafe {
    //     crate::println!(
    //         "[PCMCIA] PC16: phys=0x{:08x} virt=0x{:08x}",
    //         controller.bar0_phys + 0x800,
    //         controller.bar0_virt + 0x800
    //     );
    //     crate::println!(
    //         "[PCMCIA] PC16 initial: IDREV={:02x} IFSTAT={:02x} PWCTRL={:02x} IGCTRL={:02x} AWINEN={:02x}",
    //         controller.pc16().idrev(), controller.pc16().ifstat(),
    //         controller.pc16().pwctrl(), controller.pc16().igctrl(), controller.pc16().awinen()
    //     );
    //     print_socket_status(&controller.pc16().status());
    // }
    let controller = unsafe { SOCKET.as_ref().unwrap().as_ref() };
    unsafe { print_socket_status(&controller.pc16().status()); }
    if !prepare_socket(controller) {
        return None;
    }

    cis::map_attribute_memory();
    let info = cis::read_cis()?;

    crate::println!(
        "[PCMCIA] CIS card type={:?} FUNCID={:?} CONFIG={:?} CFTABLE={:?}",
        info.card_type, info.func_id, info.config_base, info.config_index
    );

    bind_by_cis(controller, info)
}

fn bind_by_cis(controller: &dyn controller::SocketController, info: CardInfo) -> Option<PcmciaDevice> {
    for driver in driver::card_drivers().iter() {
        if !(driver.matches)(&info) {
            continue;
        }
        crate::println!("[PCMCIA] matched card driver: {}", driver.name);
        let device = (driver.probe)(controller, info)?;
        *ACTIVE_CARD.lock() = Some((info, driver.name));
        return Some(device);
    }

    crate::println!("[PCMCIA] no card driver for type {:?}", info.card_type);
    Some(PcmciaDevice::Unsupported(info))
}

fn disconnect_card(controller: &dyn controller::SocketController) {
    let active = ACTIVE_CARD.lock().take();
    let Some((info, driver_name)) = active else {
        crate::println!("[PCMCIA] remove with no active card");
        return;
    };

    if let Some(driver) = driver::card_drivers()
        .iter()
        .find(|d| d.name == driver_name)
    {
        crate::println!("[PCMCIA] disconnect card driver: {}", driver.name);
        (driver.disconnect)(controller, info);
    }

    crate::filesystem::init::pcmcia_hotplug(false);
}

/// Re-probe CIS and pick a card driver. Socket must already be up.
pub fn bind_card() -> Option<PcmciaDevice> {
    unsafe {
        let Some(controller) = SOCKET.as_ref() else { return None };
        if !controller.pc16().status().card_present() {
            return None;
        }
        rearm_io();
        cis::map_attribute_memory();
        let info = cis::read_cis()?;
        crate::println!(
            "[PCMCIA] bind CIS type={:?} FUNCID={:?} CFTABLE={:?}",
            info.card_type, info.func_id, info.config_index
        );
        bind_by_cis(controller.as_ref(), info)
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

