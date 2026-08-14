//! Low-level PCI Configuration Space access via 0xCF8 / 0xCFC

use crate::io::{inl, outl};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

#[inline]
fn make_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let bus = bus as u32;
    let device = (device & 0x1F) as u32;
    let function = (function & 0x07) as u32;
    let offset = (offset & 0xFC) as u32; // must be dword-aligned

    0x8000_0000 | (bus << 16) | (device << 11) | (function << 8) | offset
}

#[inline]
pub fn read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let addr = make_address(bus, device, function, offset);
    unsafe {
        outl(CONFIG_ADDRESS, addr);
        inl(CONFIG_DATA)
    }
}

#[inline]
pub fn write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let addr = make_address(bus, device, function, offset);
    unsafe {
        outl(CONFIG_ADDRESS, addr);
        outl(CONFIG_DATA, value);
    }
}

#[inline]
pub fn read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let data = read_u32(bus, device, function, offset & 0xFC);
    ((data >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

#[inline]
pub fn write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let aligned = offset & 0xFC;
    let shift = (offset & 2) * 8;
    let mut data = read_u32(bus, device, function, aligned);
    data &= !(0xFFFF << shift);
    data |= (value as u32) << shift;
    write_u32(bus, device, function, aligned, data);
}

#[inline]
pub fn read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let data = read_u32(bus, device, function, offset & 0xFC);
    ((data >> ((offset & 3) * 8)) & 0xFF) as u8
}

#[inline]
pub fn write_u8(bus: u8, device: u8, function: u8, offset: u8, value: u8) {
    let aligned = offset & 0xFC;
    let shift = (offset & 3) * 8;
    let mut data = read_u32(bus, device, function, aligned);
    data &= !(0xFF << shift);
    data |= (value as u32) << shift;
    write_u32(bus, device, function, aligned, data);
}