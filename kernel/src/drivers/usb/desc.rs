//! USB 2.0 / 1.1 descriptors and setup packets.

use alloc::vec::Vec;

pub const CLASS_PER_INTERFACE: u8 = 0x00;
pub const CLASS_AUDIO: u8 = 0x01;
pub const CLASS_CDC: u8 = 0x02;
pub const CLASS_HID: u8 = 0x03;
pub const CLASS_PHYSICAL: u8 = 0x05;
pub const CLASS_IMAGE: u8 = 0x06;
pub const CLASS_PRINTER: u8 = 0x07;
pub const CLASS_MSC: u8 = 0x08;
pub const CLASS_HUB: u8 = 0x09;
pub const CLASS_CDC_DATA: u8 = 0x0A;
pub const CLASS_SMART_CARD: u8 = 0x0B;
pub const CLASS_VIDEO: u8 = 0x0E;
pub const CLASS_VENDOR: u8 = 0xFF;

pub const HID_BOOT: u8 = 0x01;
pub const HID_BOOT_KEYBOARD: u8 = 0x01;
pub const HID_BOOT_MOUSE: u8 = 0x02;

pub const MSC_SCSI: u8 = 0x06;
pub const MSC_BBB: u8 = 0x50;

pub const DT_DEVICE: u8 = 1;
pub const DT_CONFIG: u8 = 2;
pub const DT_STRING: u8 = 3;
pub const DT_INTERFACE: u8 = 4;
pub const DT_ENDPOINT: u8 = 5;
pub const DT_HUB: u8 = 0x29;

pub fn setup(bm: u8, req: u8, value: u16, index: u16, len: u16) -> [u8; 8] {
    [
        bm,
        req,
        value as u8,
        (value >> 8) as u8,
        index as u8,
        (index >> 8) as u8,
        len as u8,
        (len >> 8) as u8,
    ]
}

pub fn get_descriptor(ty: u8, index: u8, len: u16) -> [u8; 8] {
    setup(0x80, 6, ((ty as u16) << 8) | index as u16, 0, len)
}

pub fn set_configuration(cfg: u8) -> [u8; 8] {
    setup(0x00, 9, cfg as u16, 0, 0)
}

pub fn set_interface(iface: u8, alt: u8) -> [u8; 8] {
    setup(0x01, 11, alt as u16, iface as u16, 0)
}

#[derive(Clone, Copy, Debug)]
pub struct DeviceDesc {
    pub usb: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vid: u16,
    pub pid: u16,
    pub num_configs: u8,
}

impl DeviceDesc {
    pub fn parse(d: &[u8]) -> Option<Self> {
        if d.len() < 18 {
            return None;
        }
        Some(Self {
            usb: u16::from_le_bytes([d[2], d[3]]),
            class: d[4],
            subclass: d[5],
            protocol: d[6],
            max_packet0: d[7],
            vid: u16::from_le_bytes([d[8], d[9]]),
            pid: u16::from_le_bytes([d[10], d[11]]),
            num_configs: d[17],
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub addr: u8,
    pub attr: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl Endpoint {
    pub fn number(&self) -> u8 {
        self.addr & 0x0F
    }
    pub fn dir_in(&self) -> bool {
        self.addr & 0x80 != 0
    }
    pub fn is_bulk(&self) -> bool {
        self.attr & 0x03 == 2
    }
    pub fn is_interrupt(&self) -> bool {
        self.attr & 0x03 == 3
    }
}

#[derive(Clone, Debug)]
pub struct Interface {
    pub number: u8,
    pub alt: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub value: u8,
    pub interfaces: Vec<Interface>,
}

impl Config {
    pub fn parse(raw: &[u8]) -> Option<Self> {
        if raw.len() < 9 || raw[1] != DT_CONFIG {
            return None;
        }
        let total = u16::from_le_bytes([raw[2], raw[3]]) as usize;
        let n = core::cmp::min(total, raw.len());
        let mut i = 9usize;
        let mut interfaces = Vec::new();
        while i + 2 <= n {
            let len = raw[i] as usize;
            if len < 2 || i + len > n {
                break;
            }
            match raw[i + 1] {
                DT_INTERFACE if len >= 9 => {
                    interfaces.push(Interface {
                        number: raw[i + 2],
                        alt: raw[i + 3],
                        class: raw[i + 5],
                        subclass: raw[i + 6],
                        protocol: raw[i + 7],
                        endpoints: Vec::new(),
                    });
                }
                DT_ENDPOINT if len >= 7 => {
                    if let Some(iface) = interfaces.last_mut() {
                        iface.endpoints.push(Endpoint {
                            addr: raw[i + 2],
                            attr: raw[i + 3],
                            max_packet: u16::from_le_bytes([raw[i + 4], raw[i + 5]]),
                            interval: raw[i + 6],
                        });
                    }
                }
                _ => {}
            }
            i += len;
        }
        Some(Self {
            value: raw[5],
            interfaces,
        })
    }
}

pub fn class_name(class: u8) -> &'static str {
    match class {
        CLASS_PER_INTERFACE => "per-interface",
        CLASS_AUDIO => "audio",
        CLASS_CDC => "cdc",
        CLASS_HID => "hid",
        CLASS_MSC => "msc",
        CLASS_HUB => "hub",
        CLASS_CDC_DATA => "cdc-data",
        CLASS_PRINTER => "printer",
        CLASS_VIDEO => "video",
        CLASS_VENDOR => "vendor",
        _ => "other",
    }
}
