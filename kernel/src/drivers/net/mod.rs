pub mod e1000e_min;
pub mod i8255x;
pub mod rtl8139;
pub mod tcp;

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const TX_RING_SIZE: usize = 256;
const RX_RING_SIZE: usize = 256;
const RX_BUF_SIZE: usize = 1536;
const TX_BUF_SIZE: usize = 1536;

use crate::memory::resources::{MmioMapping, ResourceKind, map_resource};
use crate::net::ifconfig_dhcp;
use crate::println;

use self::e1000e_min::E1000e;
use self::i8255x::I8255x;
use self::rtl8139::Rtl8139;

static IRQ_PENDING: AtomicBool = AtomicBool::new(false);
static IRQ_EVENTS: AtomicU32 = AtomicU32::new(0);
static BOTTOM_HALVES: AtomicU32 = AtomicU32::new(0);
static BUDGET_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
static TARGETED_WAKES: AtomicU32 = AtomicU32::new(0);
static NEXT_POLL_MS: AtomicU32 = AtomicU32::new(0);

pub const READY_WORDS: usize = 4;

#[derive(Clone, Copy, Default)]
pub struct NetworkReady {
    pub read: [u32; READY_WORDS],
    pub write: [u32; READY_WORDS],
}

impl NetworkReady {
    pub fn is_empty(&self) -> bool {
        self.read.iter().all(|word| *word == 0)
            && self.write.iter().all(|word| *word == 0)
    }
}

static READY_READ: [AtomicU32; READY_WORDS] =
    [const { AtomicU32::new(0) }; READY_WORDS];
static READY_WRITE: [AtomicU32; READY_WORDS] =
    [const { AtomicU32::new(0) }; READY_WORDS];

pub(crate) fn mark_irq_pending() {
    IRQ_PENDING.store(true, Ordering::Release);
    IRQ_EVENTS.fetch_add(1, Ordering::Relaxed);
    crate::smp::request_network_work();
}

/// Request protocol progress after a syscall queued TX data or changed socket
/// state. This shares the pending bit with hardware IRQ work but does not
/// inflate the hardware IRQ counter.
pub fn request_poll() {
    IRQ_PENDING.store(true, Ordering::Release);
    crate::smp::request_network_work();
}

pub fn take_irq_pending() -> bool {
    IRQ_PENDING.swap(false, Ordering::AcqRel)
}

pub fn restore_irq_pending() {
    IRQ_PENDING.store(true, Ordering::Release);
}

pub fn set_next_poll_ms(deadline_ms: u64) {
    NEXT_POLL_MS.store(deadline_ms.min(u32::MAX as u64) as u32, Ordering::Release);
}

pub fn network_due(now_ms: u64) -> bool {
    if IRQ_PENDING.load(Ordering::Acquire) {
        return true;
    }
    let deadline = NEXT_POLL_MS.load(Ordering::Acquire);
    deadline != 0 && now_ms >= deadline as u64
}

fn publish_ready(ready: NetworkReady) {
    for word in 0..READY_WORDS {
        READY_READ[word].fetch_or(ready.read[word], Ordering::Release);
        READY_WRITE[word].fetch_or(ready.write[word], Ordering::Release);
    }
}

pub fn take_ready() -> NetworkReady {
    let mut ready = NetworkReady::default();
    for word in 0..READY_WORDS {
        ready.read[word] = READY_READ[word].swap(0, Ordering::AcqRel);
        ready.write[word] = READY_WRITE[word].swap(0, Ordering::AcqRel);
    }
    ready
}

pub fn restore_ready(ready: NetworkReady) {
    publish_ready(ready);
}

/// Execute a bounded network bottom half without the scheduler giant lock.
/// Returns true when another pass should run immediately.
pub fn run_bottom_half(now_ms: u64) -> bool {
    let irq = take_irq_pending();
    if !irq && !network_due(now_ms) {
        return false;
    }

    let Some(mut guard) = crate::net::stack::NET_STACK.try_lock() else {
        if irq {
            restore_irq_pending();
        }
        return true;
    };
    let Some(stack) = guard.as_mut() else {
        return false;
    };
    let outcome = stack.poll_budgeted(now_ms as i64, 64);
    drop(guard);

    BOTTOM_HALVES.fetch_add(1, Ordering::Relaxed);
    if outcome.budget_exhausted {
        BUDGET_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
    }
    if !outcome.ready.is_empty() {
        publish_ready(outcome.ready);
    }
    if outcome.more_work {
        restore_irq_pending();
    }
    outcome.more_work
}

pub fn record_targeted_wakes(count: u32) {
    TARGETED_WAKES.fetch_add(count, Ordering::Relaxed);
}

pub struct DebugSnapshot;

impl fmt::Display for DebugSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "net-bh: cpu={} irq={} runs={} budget={} wakes={} pending={} next_ms={}",
            crate::smp::network_cpu_index().map_or(-1, |cpu| cpu as i32),
            IRQ_EVENTS.load(Ordering::Relaxed),
            BOTTOM_HALVES.load(Ordering::Relaxed),
            BUDGET_EXHAUSTED.load(Ordering::Relaxed),
            TARGETED_WAKES.load(Ordering::Relaxed),
            IRQ_PENDING.load(Ordering::Relaxed),
            NEXT_POLL_MS.load(Ordering::Relaxed),
        )
    }
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

    pub fn rearm_interrupts(&self) {
        if let AnyNic::E1000e(nic) = self {
            nic.rearm_interrupts();
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
    let ok = match crate::drivers::net::e1000e_min::E1000e::init() {
        Ok(_) => {
            crate::net::stack::init_e1000e();
            println!("[init] network ready (e1000e-min)");
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
    // Kick DHCP/ARP once immediately; later protocol deadlines and RX/TX
    // completions are driven by the network worker.
    mark_irq_pending();
}
