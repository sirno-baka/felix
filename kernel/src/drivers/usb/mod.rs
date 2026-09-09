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
    // are already connected. Root-hub insert/remove is polled from idle on
    // legacy shared-IRQ targets until the kernel has a shared INTx dispatcher.
    ohci::init_all();
}

/// Poll/drain controller hotplug events from process/task context. All USB
/// control transfers and driver probe/disconnect callbacks happen here, never
/// from hard IRQ context.
pub fn poll_events() {
    ohci::poll_hotplug();
}

