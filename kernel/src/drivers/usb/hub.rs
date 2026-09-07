//! USB hub class (root hubs are handled inside OHCI; this is for external hubs).

use super::desc;
use super::ohci::Ohci;
use crate::println;

pub fn bind(hc: &Ohci, addr: u8) {
    let setup = desc::setup(0xA0, 6, (desc::DT_HUB as u16) << 8, 0, 9);
    let mut buf = [0u8; 9];
    match hc.control(addr, &setup, &mut buf, true) {
        Ok(_) => {
            let nports = buf[2].clamp(1, 8);
            println!("[usb-hub] addr={} downstream ports={}", addr, nports);
            scan_ports(hc, addr, nports);
        }
        Err(e) => println!("[usb-hub] GET_HUB_DESCRIPTOR: {}", e),
    }
}

const GET_STATUS: u8 = 0;
const SET_FEATURE: u8 = 3;
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;

fn scan_ports(hc: &Ohci, addr: u8, nports: u8) {
    let mut empty: [u8; 0] = [];
    for port in 1..=nports {
        let _ = hc.control(
            addr,
            &desc::setup(0x23, SET_FEATURE, PORT_POWER, port as u16, 0),
            &mut empty,
            false,
        );
    }
    crate::time::sleep(50);
    for port in 1..=nports {
        let mut st = [0u8; 4];
        if hc
            .control(addr, &desc::setup(0xA3, GET_STATUS, 0, port as u16, 4), &mut st, true)
            .is_err()
        {
            continue;
        }
        let status = u16::from_le_bytes([st[0], st[1]]);
        if status & 1 == 0 {
            continue;
        }
        println!("[usb-hub] port {} connected status={:04x}", port, status);
        let _ = hc.control(
            addr,
            &desc::setup(0x23, SET_FEATURE, PORT_RESET, port as u16, 0),
            &mut empty,
            false,
        );
        crate::time::sleep(20);
        let mut st = [0u8; 4];
        let _ = hc.control(addr, &desc::setup(0xA3, GET_STATUS, 0, port as u16, 4), &mut st, true);
        let status = u16::from_le_bytes([st[0], st[1]]);
        let ls = status & (1 << 9) != 0;
        match hc.address_and_bind(ls) {
            Ok(child) => println!("[usb-hub] port {} -> addr={}", port, child),
            Err(e) => println!("[usb-hub] port {}: {}", port, e),
        }
    }
}
