//! PciDevice representation and methods

use super::bar::Bar;
use super::config;

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,

    pub vendor_id: u16,
    pub device_id: u16,

    pub command: u16,
    pub status: u16,

    pub revision_id: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class_code: u8,

    pub cache_line_size: u8,
    pub latency_timer: u8,
    pub header_type: u8,
    pub bist: u8,

    // Header Type 0 specific
    pub bars: [Bar; 6],
    pub cardbus_cis: u32,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    pub expansion_rom: u32,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
    pub min_grant: u8,
    pub max_latency: u8,
}

impl PciDevice {
    /// Read raw config space
    pub fn read_u8(&self, offset: u8) -> u8 {
        config::read_u8(self.bus, self.device, self.function, offset)
    }

    pub fn read_u16(&self, offset: u8) -> u16 {
        config::read_u16(self.bus, self.device, self.function, offset)
    }

    pub fn read_u32(&self, offset: u8) -> u32 {
        config::read_u32(self.bus, self.device, self.function, offset)
    }

    pub fn write_u8(&self, offset: u8, value: u8) {
        config::write_u8(self.bus, self.device, self.function, offset, value);
    }

    pub fn write_u16(&self, offset: u8, value: u16) {
        config::write_u16(self.bus, self.device, self.function, offset, value);
    }

    pub fn write_u32(&self, offset: u8, value: u32) {
        config::write_u32(self.bus, self.device, self.function, offset, value);
    }

    /// Enable Bus Mastering + Memory Space + I/O Space
    pub fn enable_bus_mastering(&self) {
        let mut cmd = self.read_u16(0x04);
        cmd |= 0x0007; // IO + Memory + BusMaster
        self.write_u16(0x04, cmd);
    }

    pub fn enable_memory_space(&self) {
        let mut cmd = self.read_u16(0x04);
        cmd |= 0x0002;
        self.write_u16(0x04, cmd);
    }

    pub fn enable_io_space(&self) {
        let mut cmd = self.read_u16(0x04);
        cmd |= 0x0001;
        self.write_u16(0x04, cmd);
    }

    pub fn get_bar(&self, index: usize) -> Option<&Bar> {
        if index < 6 {
            match &self.bars[index] {
                Bar::None => None,
                bar => Some(bar),
            }
        } else {
            None
        }
    }

    pub fn is_multifunction(&self) -> bool {
        (self.header_type & 0x80) != 0
    }

    pub fn header_type_raw(&self) -> u8 {
        self.header_type & 0x7F
    }
}

/// Probe the size of a BAR
fn probe_bar_size(bus: u8, device: u8, function: u8, offset: u8, is_io: bool) -> u32 {
    let original = config::read_u32(bus, device, function, offset);

    // Write all 1s
    config::write_u32(bus, device, function, offset, 0xFFFF_FFFF);
    let mut size = config::read_u32(bus, device, function, offset);

    // Restore
    config::write_u32(bus, device, function, offset, original);

    if is_io {
        size &= 0xFFFF_FFFC; // mask out the type bits
    } else {
        size &= 0xFFFF_FFF0;
    }

    if size == 0 {
        return 0;
    }

    // Size is the inverted value + 1
    (!size).wrapping_add(1)
}

pub fn read_device(bus: u8, device: u8, function: u8) -> Option<PciDevice> {
    let vendor_id = config::read_u16(bus, device, function, 0x00);
    if vendor_id == 0xFFFF {
        return None; // no device
    }

    let device_id = config::read_u16(bus, device, function, 0x02);
    let command = config::read_u16(bus, device, function, 0x04);
    let status = config::read_u16(bus, device, function, 0x06);

    let revision_id = config::read_u8(bus, device, function, 0x08);
    let prog_if = config::read_u8(bus, device, function, 0x09);
    let subclass = config::read_u8(bus, device, function, 0x0A);
    let class_code = config::read_u8(bus, device, function, 0x0B);

    let cache_line_size = config::read_u8(bus, device, function, 0x0C);
    let latency_timer = config::read_u8(bus, device, function, 0x0D);
    let header_type = config::read_u8(bus, device, function, 0x0E);
    let bist = config::read_u8(bus, device, function, 0x0F);

    let mut bars = [Bar::None; 6];
    let header_type_raw = header_type & 0x7F;

    // Only parse BARs for Header Type 0 (normal devices)
    if header_type_raw == 0 {
        let mut i = 0;
        while i < 6 {
            let offset = 0x10 + (i as u8) * 4;
            let bar_raw = config::read_u32(bus, device, function, offset);

            if bar_raw == 0 {
                i += 1;
                continue;
            }

            if (bar_raw & 1) != 0 {
                // I/O BAR
                let address = bar_raw & 0xFFFF_FFFC;
                let size = probe_bar_size(bus, device, function, offset, true);
                bars[i] = Bar::Io { address, size };
                i += 1;
            } else {
                // Memory BAR
                let is_64bit = (bar_raw & 0b110) == 0b100;
                let prefetchable = (bar_raw & 0b1000) != 0;
                let address = bar_raw & 0xFFFF_FFF0;
                let size = probe_bar_size(bus, device, function, offset, false);

                bars[i] = Bar::Memory {
                    address,
                    size,
                    prefetchable,
                    is_64bit,
                };

                if is_64bit {
                    // Skip next BAR (upper 32 bits of 64-bit address)
                    // For simplicity we ignore the upper 32 bits on 32-bit system
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
    }

    let cardbus_cis = if header_type_raw == 0 {
        config::read_u32(bus, device, function, 0x28)
    } else {
        0
    };

    let subsystem_vendor_id = if header_type_raw == 0 {
        config::read_u16(bus, device, function, 0x2C)
    } else {
        0
    };

    let subsystem_id = if header_type_raw == 0 {
        config::read_u16(bus, device, function, 0x2E)
    } else {
        0
    };

    let expansion_rom = if header_type_raw == 0 {
        config::read_u32(bus, device, function, 0x30)
    } else {
        0
    };

    let interrupt_line = config::read_u8(bus, device, function, 0x3C);
    let interrupt_pin = config::read_u8(bus, device, function, 0x3D);
    let min_grant = config::read_u8(bus, device, function, 0x3E);
    let max_latency = config::read_u8(bus, device, function, 0x3F);

    Some(PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        command,
        status,
        revision_id,
        prog_if,
        subclass,
        class_code,
        cache_line_size,
        latency_timer,
        header_type,
        bist,
        bars,
        cardbus_cis,
        subsystem_vendor_id,
        subsystem_id,
        expansion_rom,
        interrupt_line,
        interrupt_pin,
        min_grant,
        max_latency,
    })
}
