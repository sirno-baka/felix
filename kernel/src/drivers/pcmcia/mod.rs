//! PCMCIA / CardBus support.
//!
//! The first layer deliberately talks only to the socket controller.  Card
//! identification (CIS), card type selection and ATA/CF support live above it.

pub mod ricoh_r5c475;

/// Probe the Ricoh R5C475II PCMCIA/CardBus controller.
///
/// This is intentionally a small, deterministic probe: it finds the PCI
/// function, enables PCI memory decoding, maps the controller's BAR0 through
/// the kernel's higher-half mapping, and leaves the socket powered off.
pub fn init() {
    ricoh_r5c475::probe();
}
