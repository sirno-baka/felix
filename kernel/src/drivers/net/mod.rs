pub mod i8255x;
pub mod tcp;

const TX_RING_SIZE: usize = 16;
const RX_RING_SIZE: usize = 16;
const RX_BUF_SIZE: usize = 1536;
const TX_BUF_SIZE: usize = 1536;


use crate::memory::paging::{PAGING, PAGE_SIZE, PTEFlags, PhysAddr, VirtAddr};
use crate::println;

fn map_mmio(phys: u32, size: u32) -> Result<usize, &'static str> {
    // Выбираем свободный виртуальный диапазон для MMIO
    // (можно 0xE000_0000 или любой другой, который точно свободен)
    const MMIO_VIRT_BASE: u32 = 0xE000_0000;

    let flags = PTEFlags::new()
        .present()
        .writable();
    // позже можно добавить .pcd() | .pwt() для uncacheable

    let mut paging = unsafe { PAGING.lock() };
    paging.map_physical_range(phys, size.max(0x1000), MMIO_VIRT_BASE, flags)?;

    Ok(MMIO_VIRT_BASE as usize)
}



/// Optional network bring-up (does not fail boot).
pub fn init_net() {
    match crate::drivers::net::i8255x::I8255x::init() {
        Ok(_) => {
            crate::net::stack::init();
            println!("[init] network ready");
        }
        Err(_) => println!("[init] I8255x init failed"),
    }
}
