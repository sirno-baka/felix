//! USB 1.1 host stack. Only OHCI — UHCI is not used.
//!
//! Layers:
//!   ohci     — host controller (control + bulk)
//!   desc     — descriptors / setup packets
//!   device   — bind by class
//!   hid/msc/hub — class drivers

pub mod desc;
pub mod device;
pub mod driver;
pub mod hid;
pub mod hub;
pub mod msc;
pub mod ohci;

/// Probe every PCI OHCI controller and bind class drivers.
pub fn init() {
    // init_all() both starts every OHCI controller and enumerates ports that
    // are already connected. Later insert/remove events arrive through RHSC
    // and are drained by poll_events().
    ohci::init_all();
}

/// Drain controller hotplug events from process/task context. The IRQ handler
/// only acknowledges RHSC and sets an atomic bit; all USB control transfers and
/// driver probe/disconnect callbacks happen here.
pub fn poll_events() {
    ohci::poll_hotplug();
}

