pub mod ide;

pub(crate) mod bar;
mod config;
pub(crate) mod device;
pub mod floppy;

pub mod class;

use alloc::vec::Vec;
use device::{PciDevice, read_device};

/// Enumerate all PCI devices on the system
pub fn enumerate() -> Vec<PciDevice> {
    let mut devices = Vec::new();

    for bus in 0..=255u8 {
        for device in 0..32u8 {
            // First check function 0
            if let Some(dev) = read_device(bus, device, 0) {
                let is_multi = dev.is_multifunction();
                devices.push(dev);

                // If multifunction — check remaining functions
                if is_multi {
                    for function in 1..8u8 {
                        if let Some(dev) = read_device(bus, device, function) {
                            devices.push(dev);
                        }
                    }
                }
            }
        }
    }

    devices
}

/// Простая таблица известных Vendor ID
fn vendor_name(vendor_id: u16) -> &'static str {
    match vendor_id {
        0x8086 => "Intel",
        0x1023 => "Trident",
        0x10EC => "Realtek",
        0x1002 => "ATI/AMD",
        0x10DE => "NVIDIA",
        0x1106 => "VIA",
        0x1039 => "SiS",
        0x10B9 => "ALi",
        0x104C => "Texas Instruments",
        0x1179 => "Toshiba",
        0x8086 => "Intel", // уже есть, но для ясности
        0x15AD => "VMware",
        0x1AF4 => "Red Hat (Virtio)",
        0x1234 => "QEMU",
        _ => "Unknown",
    }
}

/// Известные устройства (Vendor + Device)
fn device_name(vendor_id: u16, device_id: u16) -> &'static str {
    match (vendor_id, device_id) {
        // === Intel Ethernet (то, что нам нужно) ===
        (0x8086, 0x1229) => "82557/82558/82559 Fast Ethernet (PRO/100)",
        (0x8086, 0x1209) => "82559ER Fast Ethernet",
        (0x8086, 0x1030) => "82559 Fast Ethernet Controller",
        (0x8086, 0x1031) => "82801CAM (ICH3) PRO/100 VE",
        (0x8086, 0x1032) => "82801CAM (ICH3) PRO/100 VE",
        (0x8086, 0x100E) => "82540EM Gigabit Ethernet",
        (0x8086, 0x100F) => "82545EM Gigabit Ethernet",
        (0x8086, 0x10D3) => "82574L Gigabit Ethernet",

        // === Intel chipset / bridges ===
        (0x8086, 0x2415) => "82801AA AC'97 Audio",
        (0x8086, 0x2440) => "82801BA Hub Interface to PCI Bridge",
        (0x8086, 0x244B) => "82801BA/BAM (ICH2) LPC Interface",
        (0x8086, 0x244E) => "82801 PCI Bridge",
        (0x8086, 0x7110) => "82371AB/EB/MB PIIX4 ISA",
        (0x8086, 0x7111) => "82371AB/EB/MB PIIX4 IDE",
        (0x8086, 0x7113) => "82371AB/EB/MB PIIX4 ACPI",

        // === Trident (видеокарта твоего ноутбука) ===
        (0x1023, 0x9525) => "Cyber 9525",
        (0x1023, 0x9520) => "Cyber 9520",
        (0x1023, 0x9660) => "TGUI 9660",
        (0x1023, 0x9680) => "TGUI 9680",

        // === Realtek ===
        (0x10EC, 0x8139) => "RTL-8139 Fast Ethernet",
        (0x10EC, 0x8168) => "RTL8111/8168 Gigabit Ethernet",
        (0x10EC, 0x8169) => "RTL8169 Gigabit Ethernet",

        // === Virtio / QEMU (для тестов) ===
        (0x1AF4, 0x1000) => "Virtio network device",
        (0x1AF4, 0x1001) => "Virtio block device",
        (0x1AF4, 0x1050) => "Virtio GPU",
        (0x1234, 0x1111) => "QEMU Virtual Video Controller",

        // === Toshiba specific ===
        (0x1179, 0x0603) => "ToPIC95 PCI-CardBus Bridge",
        (0x1179, 0x060A) => "ToPIC95B PCI-CardBus Bridge",
        (0x1179, 0x060F) => "ToPIC97 PCI-CardBus Bridge",
        (0x1179, 0x0617) => "ToPIC100 PCI-CardBus Bridge",

        _ => "Unknown Device",
    }
}

/// Красивый вывод всех устройств
pub fn print_devices() {
    let devices = enumerate();
    crate::println!("=== PCI Devices ({} found) ===", devices.len());

    for dev in devices.iter() {
        let vendor = vendor_name(dev.vendor_id);
        let name = device_name(dev.vendor_id, dev.device_id);

        crate::println!(
            "{:02x}:{:02x}.{}  [{:04x}:{:04x}]  {} {}  | Class {:02x}:{:02x}:{:02x}  IRQ {}",
            dev.bus,
            dev.device,
            dev.function,
            dev.vendor_id,
            dev.device_id,
            vendor,
            name,
            dev.class_code,
            dev.subclass,
            dev.prog_if,
            dev.interrupt_line
        );
    }
    crate::println!("==============================");
}

/// Find first device with given Vendor ID + Device ID
pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    enumerate()
        .into_iter()
        .find(|d| d.vendor_id == vendor_id && d.device_id == device_id)
}

/// Find all devices of a specific class + subclass
pub fn find_by_class(class_code: u8, subclass: u8) -> Vec<PciDevice> {
    enumerate()
        .into_iter()
        .filter(|d| d.class_code == class_code && d.subclass == subclass)
        .collect()
}

/// Find network controllers (Class 0x02)
pub fn find_network_controllers() -> Vec<PciDevice> {
    find_by_class(0x02, 0x00) // Ethernet
}
