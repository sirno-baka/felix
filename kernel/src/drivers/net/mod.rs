pub mod e1000e;
pub mod i8255x;
pub mod rtl8139;
pub mod tcp;

use core::sync::atomic::{AtomicBool, Ordering};

const TX_RING_SIZE: usize = 256;
const RX_RING_SIZE: usize = 256;
const RX_BUF_SIZE: usize = 1536;
const TX_BUF_SIZE: usize = 1536;

use crate::memory::resources::{MmioMapping, ResourceKind, map_resource};
use crate::net::ifconfig_dhcp;
use crate::println;

use self::e1000e::E1000e;
use self::i8255x::I8255x;
use self::rtl8139::Rtl8139;

static IRQ_PENDING: AtomicBool = AtomicBool::new(false);

pub(crate) fn mark_irq_pending() {
    IRQ_PENDING.store(true, Ordering::Release);
}

pub fn take_irq_pending() -> bool {
    IRQ_PENDING.swap(false, Ordering::AcqRel)
}

pub fn restore_irq_pending() {
    IRQ_PENDING.store(true, Ordering::Release);
}

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

    pub fn can_transmit(&self) -> bool {
        match self {
            AnyNic::E1000e(n) => n.can_transmit(),
            // The legacy drivers synchronously reclaim their tiny TX slots.
            AnyNic::I8255x(_) | AnyNic::Rtl8139(_) => true,
        }
    }
}

fn map_mmio(phys: u32, size: u32) -> Result<MmioMapping, &'static str> {
    map_resource(
        phys as u64,
        size.max(4096) as usize,
        ResourceKind::Mmio,
        "nic-mmio",
    )
    .map_err(|_| "NIC MMIO resource/map failed")
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
