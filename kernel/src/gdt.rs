use bitflags::bitflags;
use core::arch::asm;
use core::mem::size_of;

use crate::tss::TaskStateSegment;

const GDT_ENTRIES: usize = 7;

bitflags! {
    // bitflags! provides PartialEq, Eq, Clone, Copy, Debug, Hash, PartialOrd, Ord
    pub struct DescriptorFlags: u64 {
        const ACCESSED          = 1 << 40;
        const WRITABLE          = 1 << 41;
        const CONFORMING        = 1 << 42;
        const EXECUTABLE        = 1 << 43;
        const USER_SEGMENT      = 1 << 44;
        const DPL_RING_3        = 3 << 45;
        const PRESENT           = 1 << 47;
        const AVAILABLE         = 1 << 52;
        const LONG_MODE         = 1 << 53;
        const DEFAULT_SIZE      = 1 << 54;
        const GRANULARITY       = 1 << 55;

        const LIMIT_0_15        = 0xFFFF;
        const LIMIT_16_19       = 0xF << 48;
        const BASE_0_23         = 0xFF_FFFF << 16;
        const BASE_24_31        = 0xFF << 56;
    }
}

impl DescriptorFlags {
    const COMMON: Self = Self::from_bits_truncate(
        Self::USER_SEGMENT.bits()
            | Self::PRESENT.bits()
            | Self::WRITABLE.bits()
            | Self::ACCESSED.bits()
            | Self::LIMIT_0_15.bits()
            | Self::LIMIT_16_19.bits()
            | Self::GRANULARITY.bits(),
    );
    pub const KERNEL_DATA: Self =
        Self::from_bits_truncate(Self::COMMON.bits() | Self::DEFAULT_SIZE.bits());
    pub const KERNEL_CODE32: Self = Self::from_bits_truncate(
        Self::COMMON.bits() | Self::EXECUTABLE.bits() | Self::DEFAULT_SIZE.bits(),
    );
    pub const USER_DATA: Self =
        Self::from_bits_truncate(Self::KERNEL_DATA.bits() | Self::DPL_RING_3.bits());
    pub const USER_CODE32: Self =
        Self::from_bits_truncate(Self::KERNEL_CODE32.bits() | Self::DPL_RING_3.bits());
}

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
pub struct GdtEntry {
    entry: u64,
}

#[repr(C, packed)]
pub struct GlobalDescriptorTable {
    pub entries: [GdtEntry; GDT_ENTRIES],
}

#[repr(C, packed)]
pub struct GdtDescriptor {
    size: u16,
    offset: *const GlobalDescriptorTable,
}

pub static mut TSS: TaskStateSegment = TaskStateSegment::new();

pub static mut GDT: GlobalDescriptorTable = GlobalDescriptorTable {
    entries: [GdtEntry { entry: 0 }; GDT_ENTRIES],
};

impl GlobalDescriptorTable {
    pub fn init() {
        unsafe {
            let zero = GdtEntry { entry: 0 };

            // Стандартные flat 4 GiB дескрипторы (проверены тысячами осей)
            let kcode = GdtEntry {
                entry: 0x00CF9A000000FFFF,
            }; // kernel code   (0x08)
            let kdata = GdtEntry {
                entry: 0x00CF92000000FFFF,
            }; // kernel data   (0x10)
            let ucode = GdtEntry {
                entry: 0x00CFFA000000FFFF,
            }; // user code     (0x18 → 0x1B)
            let udata = GdtEntry {
                entry: 0x00CFF2000000FFFF,
            }; // user data     (0x20 → 0x23)
            let tss_desc = make_tss_descriptor();

            GDT.entries = [zero, kcode, kdata, ucode, udata, tss_desc, zero];
        }
    }

    pub fn load(&self) {
        let descriptor = GdtDescriptor {
            size: (GDT_ENTRIES * size_of::<GdtEntry>() - 1) as u16,
            offset: self,
        };

        unsafe {
            asm!("lgdt [{0:e}]", in(reg) &descriptor);
        }
    }

    pub fn load_tss(&self) {
        unsafe {
            asm!("ltr {0:x}", in(reg) 0x28u16); // индекс 5 → 0x28 (6 * 8 = 48 = 0x28)
        }
    }

    pub fn set_kernel_stack(&self, stack_top: u32) {
        unsafe {
            TSS.esp0 = stack_top;
            TSS.ss0 = 0x10;
        }
    }
}

fn make_tss_descriptor() -> GdtEntry {
    let base = unsafe { &TSS as *const TaskStateSegment as u32 };
    let limit = (size_of::<TaskStateSegment>() - 1) as u32;

    let mut desc: u64 = 0;
    desc |= (limit & 0xFFFF) as u64;
    desc |= ((base & 0xFFFF) as u64) << 16;
    desc |= ((base >> 16 & 0xFF) as u64) << 32;
    desc |= 0x89u64 << 40;
    desc |= ((limit >> 16 & 0xF) as u64) << 48;
    desc |= ((base >> 24 & 0xFF) as u64) << 56;

    GdtEntry { entry: desc }
}
