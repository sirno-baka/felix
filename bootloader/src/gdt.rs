//GLOBAL DESCRIPTOR TABLE
use core::arch::asm;
use core::mem::size_of;
use bitflags::bitflags;

use crate::tss::TaskStateSegment;   // ← если tss.rs в той же crate

const GDT_ENTRIES: usize = 6;       // было 5


bitflags! {
    /// Flags for a GDT descriptor. Not all flags are valid for all descriptor types.
    #[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy)]
    pub struct DescriptorFlags: u64 {
        /// Set by the processor if this segment has been accessed. Only cleared by software.
        /// _Setting_ this bit in software prevents GDT writes on first use.
        const ACCESSED          = 1 << 40;
        /// For 32-bit data segments, sets the segment as writable. For 32-bit code segments,
        /// sets the segment as _readable_. In 64-bit mode, ignored for all segments.
        const WRITABLE          = 1 << 41;
        /// For code segments, sets the segment as “conforming”, influencing the
        /// privilege checks that occur on control transfers. For 32-bit data segments,
        /// sets the segment as "expand down". In 64-bit mode, ignored for data segments.
        const CONFORMING        = 1 << 42;
        /// This flag must be set for code segments and unset for data segments.
        const EXECUTABLE        = 1 << 43;
        /// This flag must be set for user segments (in contrast to system segments).
        const USER_SEGMENT      = 1 << 44;
        /// These two bits encode the Descriptor Privilege Level (DPL) for this descriptor.
        /// If both bits are set, the DPL is Ring 3, if both are unset, the DPL is Ring 0.
        const DPL_RING_3        = 3 << 45;
        /// Must be set for any segment, causes a segment not present exception if not set.
        const PRESENT           = 1 << 47;
        /// Available for use by the Operating System
        const AVAILABLE         = 1 << 52;
        /// Must be set for 64-bit code segments, unset otherwise.
        const LONG_MODE         = 1 << 53;
        /// Use 32-bit (as opposed to 16-bit) operands. If [`LONG_MODE`][Self::LONG_MODE] is set,
        /// this must be unset. In 64-bit mode, ignored for data segments.
        const DEFAULT_SIZE      = 1 << 54;
        /// Limit field is scaled by 4096 bytes. In 64-bit mode, ignored for all segments.
        const GRANULARITY       = 1 << 55;

        /// Bits `0..=15` of the limit field (ignored in 64-bit mode)
        const LIMIT_0_15        = 0xFFFF;
        /// Bits `16..=19` of the limit field (ignored in 64-bit mode)
        const LIMIT_16_19       = 0xF << 48;
        /// Bits `0..=23` of the base field (ignored in 64-bit mode, except for fs and gs)
        const BASE_0_23         = 0xFF_FFFF << 16;
        /// Bits `24..=31` of the base field (ignored in 64-bit mode, except for fs and gs)
        const BASE_24_31        = 0xFF << 56;
    }
}

impl DescriptorFlags {
    // Flags that we set for all our default segments
    const COMMON: Self = Self::from_bits_truncate(
        Self::USER_SEGMENT.bits()
            | Self::PRESENT.bits()
            | Self::WRITABLE.bits()
            | Self::ACCESSED.bits()
            | Self::LIMIT_0_15.bits()
            | Self::LIMIT_16_19.bits()
            | Self::GRANULARITY.bits(),
    );
    /// A kernel data segment (64-bit or flat 32-bit)
    pub const KERNEL_DATA: Self =
        Self::from_bits_truncate(Self::COMMON.bits() | Self::DEFAULT_SIZE.bits());
    /// A flat 32-bit kernel code segment
    pub const KERNEL_CODE32: Self = Self::from_bits_truncate(
        Self::COMMON.bits() | Self::EXECUTABLE.bits() | Self::DEFAULT_SIZE.bits(),
    );

    /// A user data segment (64-bit or flat 32-bit)
    pub const USER_DATA: Self =
        Self::from_bits_truncate(Self::KERNEL_DATA.bits() | Self::DPL_RING_3.bits());
    /// A flat 32-bit user code segment
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
    entries: [GdtEntry { entry: 0 }; GDT_ENTRIES], // временно нули
};

impl GlobalDescriptorTable {
    pub fn init() {
        unsafe {
            let zero   = GdtEntry { entry: 0 };
            let kcode  = GdtEntry { entry: DescriptorFlags::KERNEL_CODE32.bits() };
            let kdata  = GdtEntry { entry: DescriptorFlags::KERNEL_DATA.bits() };
            let ucode  = GdtEntry { entry: DescriptorFlags::USER_CODE32.bits() };
            let udata  = GdtEntry { entry: DescriptorFlags::USER_DATA.bits() };
            let tss_desc = make_tss_descriptor();

            GDT.entries = [zero, kcode, kdata, ucode, udata, tss_desc];
        }
    }

    pub fn load(&self) {
        let descriptor = GdtDescriptor {
            size: (GDT_ENTRIES * size_of::<GdtEntry>() - 1) as u16,
            offset: self,
        };

        unsafe {
            // Кладём дескриптор на стек и загружаем по адресу
            asm!(
            "lgdt [{0}]",
            in(reg) &descriptor as *const _ as u32,
            options(nostack, preserves_flags)
            );
        }
    }

    pub fn load_tss(&self) {
        unsafe {
            asm!("ltr {0:x}", in(reg) 0x28u16); // индекс 5 → 0x28
        }
    }

    pub fn set_kernel_stack(&self, stack_top: u32) {
        unsafe {
            TSS.esp0 = stack_top;
            // ss0 можно тоже поставить на kernel data
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
    desc |= 0x89u64 << 40;                    // TSS 32-bit available + present
    desc |= ((limit >> 16 & 0xF) as u64) << 48;
    desc |= ((base >> 24 & 0xFF) as u64) << 56;

    GdtEntry { entry: desc }
}