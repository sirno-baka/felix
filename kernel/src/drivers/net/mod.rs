pub mod i8255x;
pub mod rtl8139;
pub mod tcp;

const TX_RING_SIZE: usize = 16;
const RX_RING_SIZE: usize = 128;
const RX_BUF_SIZE: usize = 1536;
const TX_BUF_SIZE: usize = 1536;

use crate::memory::paging::{PAGE_SIZE, PAGING, PTEFlags, PhysAddr, VirtAddr};
use crate::println;

use self::i8255x::I8255x;
use self::rtl8139::Rtl8139;

pub enum AnyNic {
    I8255x(I8255x),
    Rtl8139(Rtl8139),
}

impl AnyNic {
    pub fn mac(&self) -> [u8; 6] {
        match self {
            AnyNic::I8255x(n) => n.mac(),
            AnyNic::Rtl8139(n) => n.mac(),
        }
    }

    pub fn send(&self, data: &[u8]) -> Result<(), &'static str> {
        match self {
            AnyNic::I8255x(n) => n.send(data),
            AnyNic::Rtl8139(n) => n.send(data),
        }
    }

    pub fn recv(&self, buf: &mut [u8]) -> Option<usize> {
        match self {
            AnyNic::I8255x(n) => n.recv(buf),
            AnyNic::Rtl8139(n) => n.recv(buf),
        }
    }
}

fn map_mmio(phys: u32, size: u32) -> Result<usize, &'static str> {
    // Выбираем свободный виртуальный диапазон для MMIO
    // (можно 0xE000_0000 или любой другой, который точно свободен)
    const MMIO_VIRT_BASE: u32 = 0xE000_0000;

    let flags = PTEFlags::new().present().writable();
    // позже можно добавить .pcd() | .pwt() для uncacheable

    let mut paging = unsafe { PAGING.lock() };
    paging.map_physical_range(phys, size.max(0x1000), MMIO_VIRT_BASE, flags)?;

    Ok(MMIO_VIRT_BASE as usize)
}

/// Optional network bring-up (does not fail boot).
pub fn init_net() {
    let ok = match crate::drivers::net::i8255x::I8255x::init() {
        Ok(_) => {
            crate::net::stack::init();
            println!("[init] network ready (i8255x)");
            true
        }
        Err(_) => false,
    };
    if !ok {
        match crate::drivers::net::rtl8139::Rtl8139::init() {
            Ok(_) => {
                crate::net::stack::init_rtl8139();
                println!("[init] network ready (rtl8139)");
            }
            Err(_) => println!("[init] no supported NIC"),
        }
    }
    crate::drivers::usb::init();
}
