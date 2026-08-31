//! Bind a configured USB device to a class driver.

use super::desc::{self, DeviceDesc};
use super::ohci::Ohci;
use super::{hid, hub, msc};
use crate::println;
use alloc::vec;

pub fn bind(hc: &Ohci, addr: u8, raw_dev: &[u8; 18]) {
    let Some(dd) = DeviceDesc::parse(raw_dev) else {
        return;
    };
    println!(
        "[usb] addr={} {:04x}:{:04x} class={}",
        addr,
        dd.vid,
        dd.pid,
        desc::class_name(dd.class)
    );

    let mut hdr = [0u8; 9];
    if hc.control(addr, &desc::get_descriptor(desc::DT_CONFIG, 0, 9), &mut hdr, true).is_err() {
        println!("[usb] GET_DESCRIPTOR config hdr failed");
        return;
    }
    let total = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
    let total = total.clamp(9, 256);
    let mut cfg_buf = vec![0u8; total];
    if hc
        .control(addr, &desc::get_descriptor(desc::DT_CONFIG, 0, total as u16), &mut cfg_buf, true)
        .is_err()
    {
        println!("[usb] GET_DESCRIPTOR config failed");
        return;
    }
    let Some(cfg) = desc::Config::parse(&cfg_buf) else {
        println!("[usb] bad config descriptor");
        return;
    };

    let mut empty: [u8; 0] = [];
    if let Err(e) = hc.control(addr, &desc::set_configuration(cfg.value), &mut empty, false) {
        println!("[usb] SET_CONFIGURATION: {}", e);
        return;
    }

    if dd.class == desc::CLASS_HUB {
        hub::bind(hc, addr);
        return;
    }

    for iface in cfg.interfaces.iter() {
        println!(
            "[usb] iface {} class={} sub=0x{:02x} proto=0x{:02x} eps={}",
            iface.number,
            desc::class_name(iface.class),
            iface.subclass,
            iface.protocol,
            iface.endpoints.len()
        );
        match iface.class {
            desc::CLASS_HID => hid::bind(hc, addr, iface),
            desc::CLASS_MSC => msc::bind(hc, addr, iface),
            desc::CLASS_HUB => hub::bind(hc, addr),
            desc::CLASS_CDC | desc::CLASS_CDC_DATA => {
                println!("[usb] CDC not implemented");
            }
            desc::CLASS_PRINTER => println!("[usb] printer not implemented"),
            desc::CLASS_VENDOR => println!("[usb] vendor-specific, skipped"),
            _ => {}
        }
    }
}
