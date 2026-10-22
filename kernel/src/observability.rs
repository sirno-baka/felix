//! Low-overhead SMP diagnostics for latency-sensitive paths.
//!
//! Writers use one ring per logical CPU.  An event is published with a final
//! release-store of its sequence number, so IRQ and F12 readers never need a
//! lock.  The hot, uncontended kernel-lock path does not touch this module.

use core::arch::asm;
use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

const MAX_CPUS: usize = 64;
const TRACE_LEN: usize = 64;
const TRACE_DUMP_LEN: usize = 12;

pub const TRACE_TLB_SEND: u32 = 10;
pub const TRACE_TLB_ACK: u32 = 11;
pub const TRACE_TLB_DONE: u32 = 12;
pub const TRACE_TLB_TIMEOUT: u32 = 13;
pub const TRACE_TLB_IPI_FAIL: u32 = 14;
pub const TRACE_FUTEX_WAIT: u32 = 20;
pub const TRACE_FUTEX_WAKE: u32 = 21;
pub const TRACE_FUTEX_TIMEOUT: u32 = 22;
pub const TRACE_SOCKET_BLOCK: u32 = 30;
pub const TRACE_SOCKET_WAKE: u32 = 31;
pub const TRACE_POLL_BLOCK: u32 = 32;
pub const TRACE_POLL_WAKE: u32 = 33;

struct TraceEntry {
    sequence: AtomicU32,
    tsc: AtomicU32,
    kind: AtomicU32,
    a: AtomicU32,
    b: AtomicU32,
    c: AtomicU32,
    d: AtomicU32,
}

impl TraceEntry {
    const fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            tsc: AtomicU32::new(0),
            kind: AtomicU32::new(0),
            a: AtomicU32::new(0),
            b: AtomicU32::new(0),
            c: AtomicU32::new(0),
            d: AtomicU32::new(0),
        }
    }
}

#[repr(align(64))]
struct KernelLockMetrics {
    contended: AtomicU32,
    try_misses: AtomicU32,
    wait_cycles: AtomicU32,
    max_wait_cycles: AtomicU32,
    spin_loops: AtomicU32,
}

impl KernelLockMetrics {
    const fn new() -> Self {
        Self {
            contended: AtomicU32::new(0),
            try_misses: AtomicU32::new(0),
            wait_cycles: AtomicU32::new(0),
            max_wait_cycles: AtomicU32::new(0),
            spin_loops: AtomicU32::new(0),
        }
    }
}

#[repr(align(64))]
struct TraceRing {
    head: AtomicU32,
    entries: [TraceEntry; TRACE_LEN],
}

impl TraceRing {
    const fn new() -> Self {
        Self {
            head: AtomicU32::new(0),
            entries: [const { TraceEntry::new() }; TRACE_LEN],
        }
    }
}

static SYNC_TRACE: [TraceRing; MAX_CPUS] = [const { TraceRing::new() }; MAX_CPUS];
static IO_TRACE: [TraceRing; MAX_CPUS] = [const { TraceRing::new() }; MAX_CPUS];
static KERNEL_LOCK_METRICS: [KernelLockMetrics; MAX_CPUS] =
    [const { KernelLockMetrics::new() }; MAX_CPUS];

#[inline(always)]
pub fn read_tsc_low() -> u32 {
    let low: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") _,
            options(nomem, nostack)
        );
    }
    low
}

#[inline(always)]
fn bump(counter: &AtomicU32, amount: u32) {
    // Each slot has one owning CPU.  A relaxed load/store avoids a locked RMW
    // in latency-sensitive paths; a nested diagnostic IRQ may at worst lose a
    // sample, which is preferable to perturbing the workload being measured.
    counter.store(
        counter.load(Ordering::Relaxed).wrapping_add(amount),
        Ordering::Relaxed,
    );
}

#[inline]
pub fn trace_on_cpu(cpu: usize, kind: u32, a: u32, b: u32, c: u32, d: u32) {
    if cpu >= MAX_CPUS {
        return;
    }
    let ring = if kind >= TRACE_SOCKET_BLOCK {
        &IO_TRACE[cpu]
    } else {
        &SYNC_TRACE[cpu]
    };
    let mut sequence = ring.head.load(Ordering::Relaxed).wrapping_add(1);
    if sequence == 0 {
        sequence = 1;
    }
    ring.head.store(sequence, Ordering::Relaxed);
    let entry = &ring.entries[sequence as usize & (TRACE_LEN - 1)];
    entry.sequence.store(0, Ordering::Release);
    entry.tsc.store(read_tsc_low(), Ordering::Relaxed);
    entry.kind.store(kind, Ordering::Relaxed);
    entry.a.store(a, Ordering::Relaxed);
    entry.b.store(b, Ordering::Relaxed);
    entry.c.store(c, Ordering::Relaxed);
    entry.d.store(d, Ordering::Relaxed);
    entry.sequence.store(sequence, Ordering::Release);
}

#[inline]
pub fn record_kernel_lock_contention(cpu: usize, wait_cycles: u32, spin_loops: u32) {
    if cpu >= MAX_CPUS {
        return;
    }
    let metrics = &KERNEL_LOCK_METRICS[cpu];
    bump(&metrics.contended, 1);
    bump(&metrics.wait_cycles, wait_cycles);
    bump(&metrics.spin_loops, spin_loops);
    if wait_cycles > metrics.max_wait_cycles.load(Ordering::Relaxed) {
        metrics
            .max_wait_cycles
            .store(wait_cycles, Ordering::Relaxed);
    }
}

#[inline]
pub fn record_kernel_lock_try_miss(cpu: usize) {
    if cpu < MAX_CPUS {
        bump(&KERNEL_LOCK_METRICS[cpu].try_misses, 1);
    }
}

fn event_name(kind: u32) -> &'static str {
    match kind {
        TRACE_TLB_SEND => "tlb-send",
        TRACE_TLB_ACK => "tlb-ack",
        TRACE_TLB_DONE => "tlb-done",
        TRACE_TLB_TIMEOUT => "tlb-timeout",
        TRACE_TLB_IPI_FAIL => "tlb-ipi-fail",
        TRACE_FUTEX_WAIT => "futex-wait",
        TRACE_FUTEX_WAKE => "futex-wake",
        TRACE_FUTEX_TIMEOUT => "futex-timeout",
        TRACE_SOCKET_BLOCK => "socket-block",
        TRACE_SOCKET_WAKE => "socket-wake",
        TRACE_POLL_BLOCK => "poll-block",
        TRACE_POLL_WAKE => "poll-wake",
        _ => "unknown",
    }
}

fn fmt_event(
    f: &mut fmt::Formatter<'_>,
    sequence: u32,
    tsc: u32,
    kind: u32,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
) -> fmt::Result {
    write!(
        f,
        "  #{:08x} tsc={:08x} {:<13}",
        sequence,
        tsc,
        event_name(kind)
    )?;
    match kind {
        TRACE_TLB_SEND => writeln!(f, " epoch={} pd={:#x} page={:#x} targets={}", a, b, c, d),
        TRACE_TLB_ACK => writeln!(f, " epoch={} cr3={:#x} page={:#x}", a, b, c),
        TRACE_TLB_DONE => writeln!(f, " epoch={} cycles={} targets={}", a, b, c),
        TRACE_TLB_TIMEOUT => writeln!(
            f,
            " epoch={} cycles={} missing_cpu={} targets={}",
            a, b, c, d
        ),
        TRACE_TLB_IPI_FAIL => writeln!(f, " epoch={} apic={} pd={:#x} page={:#x}", a, b, c, d),
        TRACE_FUTEX_WAIT => writeln!(f, " key={:08x}:{:08x} slot={} timeout={}", b, a, c, d),
        TRACE_FUTEX_WAKE => writeln!(f, " key={:08x}:{:08x} requested={} woken={}", b, a, c, d),
        TRACE_FUTEX_TIMEOUT => writeln!(f, " key={:08x}:{:08x} slot={} tid={}", b, a, c, d),
        TRACE_SOCKET_BLOCK | TRACE_POLL_BLOCK => {
            writeln!(f, " slot={} syscall={}", a, b)
        }
        TRACE_SOCKET_WAKE | TRACE_POLL_WAKE => {
            writeln!(f, " woken={} slots={:#010x}", a, b)
        }
        _ => writeln!(f, " a={:08x} b={:08x} c={:08x} d={:08x}", a, b, c, d),
    }
}

pub struct DebugSnapshot;

fn fmt_trace_ring(
    f: &mut fmt::Formatter<'_>,
    label: &str,
    rings: &[TraceRing; MAX_CPUS],
) -> fmt::Result {
    writeln!(f, "recent {} trace:", label)?;
    let mut any_events = false;
    for cpu in 0..MAX_CPUS {
        let ring = &rings[cpu];
        let head = ring.head.load(Ordering::Acquire);
        if head == 0 {
            continue;
        }
        any_events = true;
        writeln!(f, " cpu{}:", cpu)?;
        let available = core::cmp::min(head as usize, TRACE_DUMP_LEN);
        for offset in (0..available).rev() {
            let sequence = head.wrapping_sub(offset as u32);
            let entry = &ring.entries[sequence as usize & (TRACE_LEN - 1)];
            let before = entry.sequence.load(Ordering::Acquire);
            if before != sequence {
                continue;
            }
            let tsc = entry.tsc.load(Ordering::Relaxed);
            let kind = entry.kind.load(Ordering::Relaxed);
            let a = entry.a.load(Ordering::Relaxed);
            let b = entry.b.load(Ordering::Relaxed);
            let c = entry.c.load(Ordering::Relaxed);
            let d = entry.d.load(Ordering::Relaxed);
            if entry.sequence.load(Ordering::Acquire) != before {
                continue;
            }
            fmt_event(f, sequence, tsc, kind, a, b, c, d)?;
        }
    }
    if !any_events {
        writeln!(f, " no events recorded")?;
    }
    Ok(())
}

impl fmt::Display for DebugSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "latency counters (TSC cycles, wrapping totals):")?;
        let mut any_metrics = false;
        for cpu in 0..MAX_CPUS {
            let metrics = &KERNEL_LOCK_METRICS[cpu];
            let contended = metrics.contended.load(Ordering::Relaxed);
            let misses = metrics.try_misses.load(Ordering::Relaxed);
            if contended == 0 && misses == 0 {
                continue;
            }
            any_metrics = true;
            writeln!(
                f,
                " cpu{} klock: contended={} try_miss={} wait={} max={} spins={}",
                cpu,
                contended,
                misses,
                metrics.wait_cycles.load(Ordering::Relaxed),
                metrics.max_wait_cycles.load(Ordering::Relaxed),
                metrics.spin_loops.load(Ordering::Relaxed),
            )?;
        }
        if !any_metrics {
            writeln!(f, " kernel lock: no contention observed")?;
        }

        fmt_trace_ring(f, "TLB/futex", &SYNC_TRACE)?;
        fmt_trace_ring(f, "socket/poll", &IO_TRACE)
    }
}
