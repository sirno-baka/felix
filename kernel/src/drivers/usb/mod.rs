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
    // Audio is initialized by main.rs immediately before USB. Do not probe it
    // again here: HDA controller reset is destructive to an already configured
    // stream and would also register /dev/audio twice.

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
