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
    // main.rs reaches USB only after rootfs + /dev are mounted. Probe audio
    // first: its PCI enumeration sizes BARs, and doing that after OHCI is
    // already operational would briefly disable MMIO decode on a live HCD.
    crate::drivers::audio::init();

    // init_all() starts every OHCI controller and enumerates already-connected
    // ports. Root-hub insert/remove remains polling-driven; OHCI interrupt
    // sources stay disabled on the fragile C1M shared-IRQ path.
    ohci::init_all();
}

/// Poll/drain controller hotplug events from process/task context. All USB
/// control transfers and driver probe/disconnect callbacks happen here, never
/// from hard IRQ context.
pub fn poll_events() {
    ohci::poll_hotplug();
}
