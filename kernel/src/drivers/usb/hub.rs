//! USB hub class (root hubs are handled inside OHCI; this is for external hubs).

use super::desc;
use super::ohci::Ohci;
use crate::println;

pub fn bind(hc: &Ohci, addr: u8) {
    let setup = desc::setup(0xA0, 6, (desc::DT_HUB as u16) << 8, 0, 9);
    let mut buf = [0u8; 9];
    match hc.control(addr, &setup, &mut buf, true) {
        Ok(_) => {
            let nports = buf[2];
            println!("[usb-hub] addr={} downstream ports={}", addr, nports);
        }
        Err(e) => println!("[usb-hub] GET_HUB_DESCRIPTOR: {}", e),
    }
}
