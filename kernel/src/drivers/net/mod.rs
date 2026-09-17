pub mod e1000e;
pub mod i8255x;
pub mod rtl8139;
pub mod tcp;

const TX_RING_SIZE: usize = 16;
const RX_RING_SIZE: usize = 128;
const RX_BUF_SIZE: usize = 1536;
const TX_BUF_SIZE: usize = 1536;

use crate::memory::paging::{
    PTEFlags, PhysAddr, VirtAddr, KERNEL_MMIO_BASE, KERNEL_MMIO_END, PAGE_SIZE, PAGING,
};
use crate::net::ifconfig_dhcp;
use crate::println;

use self::e1000e::E1000e;
use self::i8255x::I8255x;
use self::rtl8139::Rtl8139;

pub enum AnyNic {
    E1000e(E1000e),
    I8255x(I8255x),
    Rtl8139(Rtl8139),
}

impl AnyNic {
    pub fn mac(&self) -> [u8; 6] {
        match self {
            AnyNic::E1000e(n) => n.mac(),
            AnyNic::I8255x(n) => n.mac(),
            AnyNic::Rtl8139(n) => n.mac(),
        }
    }

    pub fn send(&self, data: &[u8]) -> Result<(), &'static str> {
        match self {
            AnyNic::E1000e(n) => n.send(data),
            AnyNic::I8255x(n) => n.send(data),
            AnyNic::Rtl8139(n) => n.send(data),
        }
    }

    pub fn recv(&self, buf: &mut [u8]) -> Option<usize> {
        match self {
            AnyNic::E1000e(n) => n.recv(buf),
            AnyNic::I8255x(n) => n.recv(buf),
            AnyNic::Rtl8139(n) => n.recv(buf),
        }
    }
}

fn map_mmio(phys: u32, size: u32) -> Result<usize, &'static str> {
    // Keep NIC registers outside the higher-half direct map.  Mapping BAR0 at
    // 0xE0000000 used to replace the direct-map alias of physical 0x20000000
    // on machines with >= 512 MiB RAM; the frame allocator could then hand
    // that RAM to DMA and `phys + KERNEL_OFFSET` would point at MMIO instead.
    const MMIO_VIRT_BASE: u32 = KERNEL_MMIO_BASE;

    let map_size = size.max(PAGE_SIZE as u32);
    let map_end = MMIO_VIRT_BASE
        .checked_add(map_size)
        .ok_or("NIC MMIO range overflow")?;
    if map_end > KERNEL_MMIO_END {
        return Err("NIC MMIO BAR does not fit reserved window");
    }

    // Device registers must be uncached on real hardware. Volatile accesses
    // alone do not prevent the CPU cache from retaining an MMIO cache line.
    let flags = PTEFlags::new()
        .present()
        .writable()
        .write_through()
        .cache_disable();

    let mut paging = unsafe { PAGING.lock() };
    paging.map_physical_range(phys, map_size, MMIO_VIRT_BASE, flags)?;
    let actual = paging.dir.translate(MMIO_VIRT_BASE);
    println!(
        "net: MMIO phys={:#010x} size={:#x} -> virt={:#010x} actual={:?}",
        phys, map_size, MMIO_VIRT_BASE, actual
    );
    if actual != Some(phys) {
        return Err("NIC MMIO mapping verification failed");
    }

    Ok(MMIO_VIRT_BASE as usize)
}

/// Optional network bring-up (does not fail boot).
pub fn init_net() {
    let ok = match crate::drivers::net::e1000e::E1000e::init() {
        Ok(_) => {
            crate::net::stack::init_e1000e();
            println!("[init] network ready (e1000e/82579LM)");
            true
        }
        Err(err) => {
            if err != "82579LM not found" {
                println!("e1000e: initialization failed: {}", err);
            }
            false
        }
    };
    let ok = if !ok {
        match crate::drivers::net::i8255x::I8255x::init() {
            Ok(_) => {
                crate::net::stack::init();
                println!("[init] network ready (i8255x)");
                true
            }
            Err(_) => false,
        }
    } else {
        true
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

    ifconfig_dhcp();
}
