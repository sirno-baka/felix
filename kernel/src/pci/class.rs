//! Common PCI Class Codes

pub mod class {
    pub const MASS_STORAGE: u8 = 0x01;
    pub const NETWORK: u8 = 0x02;
    pub const DISPLAY: u8 = 0x03;
    pub const MULTIMEDIA: u8 = 0x04;
    pub const MEMORY: u8 = 0x05;
    pub const BRIDGE: u8 = 0x06;
    pub const SIMPLE_COMM: u8 = 0x07;
    pub const BASE_SYSTEM: u8 = 0x08;
    pub const INPUT: u8 = 0x09;
    pub const DOCKING: u8 = 0x0A;
    pub const PROCESSOR: u8 = 0x0B;
    pub const SERIAL_BUS: u8 = 0x0C;
}

pub mod subclass {
    // Network
    pub const ETHERNET: u8 = 0x00;

    // Mass Storage
    pub const IDE: u8 = 0x01;
    pub const FLOPPY: u8 = 0x02;
    pub const ATA: u8 = 0x05;
    pub const SATA: u8 = 0x06;
    pub const NVME: u8 = 0x08;

    // Display
    pub const VGA: u8 = 0x00;

    // Bridge
    pub const HOST: u8 = 0x00;
    pub const ISA: u8 = 0x01;
    pub const PCI_TO_PCI: u8 = 0x04;
}