// DISK READER — INT 0x13 AH=42 (LBA DAP)
// iPXE/BIOS INT 13h destroys unreal-mode segment limits. High-memory copies
// must restore them first, otherwise writing to 0x01000000 #GPs and hangs.

use core::arch::asm;
use core::mem;

pub static mut DISK: Disk = Disk {
    lba: 0,
    buffer: 0,
};

#[repr(C, packed)]
struct DiskAddressPacket {
    size: u8,
    zero: u8,
    sectors: u16,
    offset: u16,
    segment: u16,
    lba: u64,
}

pub struct Disk {
    lba: u64,
    buffer: u16,
}

impl Disk {
    pub fn init(&mut self, lba: u64, buffer: u16) {
        self.lba = lba;
        self.buffer = buffer;
    }

    fn int13(&self, sectors: u16) {
        let dap = DiskAddressPacket {
            size: mem::size_of::<DiskAddressPacket>() as u8,
            zero: 0,
            sectors,
            offset: self.buffer,
            segment: 0x0000,
            lba: self.lba,
        };

        let dap_address = &dap as *const DiskAddressPacket as u16;

        // LLVM reserves ESI — never list si as an asm operand.
        // STI: iPXE HTTP SAN needs IRQs for the NIC.
        unsafe {
            asm!(
                "push ds",
                "push es",
                "push ax",
                "xor ax, ax",
                "mov ds, ax",
                "mov es, ax",
                "pop ax",
                "mov {1:x}, si",
                "mov si, {0:x}",
                "sti",
                "int 0x13",
                "cli",
                "cld",
                "jc fail",
                "mov si, {1:x}",
                "pop es",
                "pop ds",
                in(reg) dap_address,
                out(reg) _,
                in("ax") 0x4200u16,
                in("dx") 0x0080u16,
            );
        }
    }

    pub fn read_sector(&self) {
        self.int13(1);
    }

    /// Read `sectors` into self.buffer in a single BIOS call.
    pub fn read_low(&self, sectors: u16) {
        self.int13(sectors);
    }

    /// Read a whole block into the low scratch buffer, then copy to high memory.
    pub fn read_sectors(&mut self, sectors: u16, target: u32) {
        self.int13(sectors);
        crate::restore_unreal();
        copy_high(self.buffer as u32, target, sectors as u32 * 512);
        print!(".");
    }
}

fn copy_high(src: u32, dst: u32, len: u32) {
    unsafe {
        asm!(
            "2:",
            "mov eax, [{0:e}]",
            "mov [{1:e}], eax",
            "add {0:e}, 4",
            "add {1:e}, 4",
            "sub {2:e}, 4",
            "jnz 2b",
            inout(reg) src => _,
            inout(reg) dst => _,
            inout(reg) len => _,
            out("eax") _,
        );
    }
}
