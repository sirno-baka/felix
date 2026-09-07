//! PCMCIA controller and card-driver matching.
//!
//! Controllers are matched by PCI identity. Cards are matched from CIS data.
//! The front-end in `mod.rs` owns enumeration; individual drivers only probe
//! hardware they actually support.

use super::controller::SocketController;
use super::{CardInfo, CardType, PcmciaDevice};

pub struct ControllerDriver {
    pub name: &'static str,
    pub matches: fn(&crate::pci::device::PciDevice) -> bool,
    pub attach: fn(crate::pci::device::PciDevice) -> Option<alloc::boxed::Box<dyn SocketController>>,
}

pub struct CardDriver {
    pub name: &'static str,
    pub matches: fn(&CardInfo) -> bool,
    pub probe: fn(&dyn SocketController, CardInfo) -> Option<PcmciaDevice>,
    pub disconnect: fn(&dyn SocketController, CardInfo),
}

fn ricoh_matches(dev: &crate::pci::device::PciDevice) -> bool {
    super::controller::matches(dev)
}

fn ricoh_attach(
    dev: crate::pci::device::PciDevice,
) -> Option<alloc::boxed::Box<dyn SocketController>> {
    super::controller::attach(dev)
}

static CONTROLLERS: &[ControllerDriver] = &[
    ControllerDriver {
        name: "ricoh-r5c475",
        matches: ricoh_matches,
        attach: ricoh_attach,
    },
];

fn fixed_disk_matches(info: &CardInfo) -> bool {
    info.card_type == CardType::FixedDisk
}

fn fixed_disk_probe(
    controller: &dyn SocketController,
    info: CardInfo,
) -> Option<PcmciaDevice> {
    super::init_fixed_disk(controller, info)
}

fn fixed_disk_disconnect(_controller: &dyn SocketController, _info: CardInfo) {
    // ATA/CF currently has no persistent controller-side state to tear down.
    // The PCMCIA core owns the active-card lifetime; filesystem teardown is
    // handled by the removable-storage layer.
}

static CARD_DRIVERS: &[CardDriver] = &[
    CardDriver {
        name: "compact-flash-ata",
        matches: fixed_disk_matches,
        probe: fixed_disk_probe,
        disconnect: fixed_disk_disconnect,
    },
];

pub fn controllers() -> &'static [ControllerDriver] {
    CONTROLLERS
}

pub fn card_drivers() -> &'static [CardDriver] {
    CARD_DRIVERS
}
