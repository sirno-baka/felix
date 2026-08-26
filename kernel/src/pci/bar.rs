//! PCI Base Address Registers

#[derive(Debug, Clone, Copy)]
pub enum Bar {
    Memory {
        address: u32,
        size: u32,
        prefetchable: bool,
        is_64bit: bool,
    },
    Io {
        address: u32,
        size: u32,
    },
    None,
}

impl Bar {
    pub fn address(&self) -> Option<u32> {
        match self {
            Bar::Memory { address, .. } => Some(*address),
            Bar::Io { address, .. } => Some(*address),
            Bar::None => None,
        }
    }

    pub fn size(&self) -> u32 {
        match self {
            Bar::Memory { size, .. } => *size,
            Bar::Io { size, .. } => *size,
            Bar::None => 0,
        }
    }

    pub fn is_memory(&self) -> bool {
        matches!(self, Bar::Memory { .. })
    }

    pub fn is_io(&self) -> bool {
        matches!(self, Bar::Io { .. })
    }
}
