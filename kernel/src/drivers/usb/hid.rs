//! HID class — boot keyboard and boot mouse (USB 1.1).

use super::desc::{self, Interface};
use super::ohci::Ohci;
use crate::println;

const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
const GET_REPORT: u8 = 0x01;
const PROTOCOL_BOOT: u16 = 0;

pub fn bind(hc: &Ohci, addr: u8, iface: &Interface) {
    match iface.protocol {
        desc::HID_BOOT_KEYBOARD => {
            println!("[usb-hid] keyboard addr={} iface={}", addr, iface.number);
            boot_setup(hc, addr, iface.number);
            let mut report = [0u8; 8];
            if get_report(hc, addr, iface.number, &mut report).is_ok() {
                println!("[usb-hid] kbd report {:02x?}", &report[..]);
            }
        }
        desc::HID_BOOT_MOUSE => {
            println!("[usb-hid] mouse addr={} iface={}", addr, iface.number);
            boot_setup(hc, addr, iface.number);
            let mut report = [0u8; 4];
            if get_report(hc, addr, iface.number, &mut report).is_ok() {
                println!(
                    "[usb-hid] mouse buttons={:02x} dx={} dy={}",
                    report[0], report[1] as i8, report[2] as i8
                );
            }
        }
        p => println!("[usb-hid] proto=0x{:02x} (not boot) addr={}", p, addr),
    }
}

fn boot_setup(hc: &Ohci, addr: u8, iface: u8) {
    let mut empty: [u8; 0] = [];
    let _ = hc.control(addr, &desc::setup(0x21, SET_PROTOCOL, PROTOCOL_BOOT, iface as u16, 0), &mut empty, false);
    let _ = hc.control(addr, &desc::setup(0x21, SET_IDLE, 0, iface as u16, 0), &mut empty, false);
}

fn get_report(hc: &Ohci, addr: u8, iface: u8, buf: &mut [u8]) -> Result<usize, &'static str> {
    let setup = desc::setup(0xA1, GET_REPORT, 0x0100, iface as u16, buf.len() as u16);
    hc.control(addr, &setup, buf, true)
}
