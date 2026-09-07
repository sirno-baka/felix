//! USB driver registry and descriptor matching.
//!
//! The USB core owns enumeration and interface ownership. Drivers only declare
//! what they match and implement probe/disconnect callbacks. This is deliberately
//! small and static for the no_std kernel; dynamic module loading can be added later.

use super::desc::{DeviceDesc, Interface};
use super::ohci::Ohci;

#[derive(Clone, Copy)]
pub enum UsbMatch {
    InterfaceClass {
        class: u8,
        subclass: Option<u8>,
        protocol: Option<u8>,
    },
    DeviceClass {
        class: u8,
        subclass: Option<u8>,
        protocol: Option<u8>,
    },
    VidPid {
        vid: u16,
        pid: u16,
    },
    VidPidInterface {
        vid: u16,
        pid: u16,
        class: u8,
        subclass: Option<u8>,
        protocol: Option<u8>,
    },
}

impl UsbMatch {
    pub fn score(&self, dev: &DeviceDesc, iface: Option<&Interface>) -> Option<u8> {
        match *self {
            UsbMatch::InterfaceClass { class, subclass, protocol } => {
                let i = iface?;
                if i.class != class || subclass.map_or(false, |v| i.subclass != v)
                    || protocol.map_or(false, |v| i.protocol != v)
                {
                    return None;
                }
                Some(match (subclass, protocol) {
                    (Some(_), Some(_)) => 180,
                    (Some(_), None) => 150,
                    (None, Some(_)) => 140,
                    (None, None) => 120,
                })
            }
            UsbMatch::DeviceClass { class, subclass, protocol } => {
                if dev.class != class
                    || subclass.map_or(false, |v| dev.subclass != v)
                    || protocol.map_or(false, |v| dev.protocol != v)
                {
                    return None;
                }
                Some(match (subclass, protocol) {
                    (Some(_), Some(_)) => 170,
                    (Some(_), None) => 145,
                    _ => 110,
                })
            }
            UsbMatch::VidPid { vid, pid } => {
                if dev.vid == vid && dev.pid == pid { Some(255) } else { None }
            }
            UsbMatch::VidPidInterface { vid, pid, class, subclass, protocol } => {
                let i = iface?;
                if dev.vid != vid || dev.pid != pid || i.class != class
                    || subclass.map_or(false, |v| i.subclass != v)
                    || protocol.map_or(false, |v| i.protocol != v)
                {
                    return None;
                }
                Some(250)
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct UsbDriver {
    pub name: &'static str,
    pub matches: &'static [UsbMatch],
    pub probe: fn(&Ohci, u8, &crate::drivers::usb::device::UsbDevice, Option<&Interface>) -> Result<(), &'static str>,
    pub disconnect: fn(&Ohci, u8, u8),
}

pub fn best_match(
    drivers: &'static [UsbDriver],
    dev: &DeviceDesc,
    iface: Option<&Interface>,
) -> Option<(&'static UsbDriver, u8)> {
    let mut best: Option<(&UsbDriver, u8)> = None;
    for driver in drivers {
        for rule in driver.matches {
            if let Some(score) = rule.score(dev, iface) {
                if best.map_or(true, |(_, old)| score > old) {
                    best = Some((driver, score));
                }
            }
        }
    }
    best
}

/// Static registry. Order is only a tie-breaker; the match score wins first.
pub fn registry() -> &'static [UsbDriver] {
    static DRIVERS: &[UsbDriver] = &[super::msc::DRIVER, super::hid::DRIVER, super::hub::DRIVER];
    DRIVERS
}
