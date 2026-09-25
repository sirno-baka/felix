//! Shared legacy PCI INTx dispatcher.
//!
//! Old PCI machines routinely route USB, CardBus, audio and other devices to
//! the same PIC line (especially IRQ9/10/11). A handler must inspect its own
//! status registers and return `false` when the interrupt is not its own. The
//! PIC EOI is sent exactly once, after every registered owner has been checked.

use alloc::vec::Vec;
use core::arch::naked_asm;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::drivers::pic::PICS;
use crate::spin::KMutex;

pub type IrqHandler = fn(u8) -> bool;

const IRQ_LINES: usize = 16;
const HANDLERS_PER_IRQ: usize = 6;
const STRAY_LIMIT: u8 = 32;
const KERNEL_IMAGE_START: usize = 0xC100_0000;
const KERNEL_IMAGE_END: usize = KERNEL_IMAGE_START + 0x0040_0000;

#[derive(Clone, Copy)]
struct Line {
    handlers: [Option<IrqHandler>; HANDLERS_PER_IRQ],
    owners: [Option<&'static str>; HANDLERS_PER_IRQ],
    stray: u8,
    storm_cooldown: u8,
}

impl Line {
    const fn new() -> Self {
        Self {
            handlers: [None; HANDLERS_PER_IRQ],
            owners: [None; HANDLERS_PER_IRQ],
            stray: 0,
            storm_cooldown: 0,
        }
    }
}

static LINES: KMutex<[Line; IRQ_LINES]> = KMutex::new([Line::new(); IRQ_LINES]);
static HANDLED_COUNT: [AtomicU32; IRQ_LINES] =
    [const { AtomicU32::new(0) }; IRQ_LINES];
static UNHANDLED_COUNT: [AtomicU32; IRQ_LINES] =
    [const { AtomicU32::new(0) }; IRQ_LINES];

#[derive(Clone, Copy, Debug, Default)]
pub struct IrqStats {
    pub handled: u32,
    pub unhandled: u32,
    pub owners: u8,
}

pub fn owner_names(irq: u8) -> Vec<&'static str> {
    if irq as usize >= IRQ_LINES {
        return Vec::new();
    }
    LINES.lock()[irq as usize]
        .owners
        .iter()
        .flatten()
        .copied()
        .collect()
}

pub fn stats(irq: u8) -> Option<IrqStats> {
    if irq as usize >= IRQ_LINES {
        return None;
    }
    let owners = LINES.lock()[irq as usize]
        .handlers
        .iter()
        .filter(|handler| handler.is_some())
        .count() as u8;
    Some(IrqStats {
        handled: HANDLED_COUNT[irq as usize].load(Ordering::Relaxed),
        unhandled: UNHANDLED_COUNT[irq as usize].load(Ordering::Relaxed),
        owners,
    })
}
// main.rs currently rewrites the complete PIC masks just before STI. The first
// PIT tick repairs only the PCI lines that actually acquired an owner.
static LATE_RESTORED: AtomicBool = AtomicBool::new(false);

fn supported(irq: u8) -> bool {
    matches!(irq, 5 | 9 | 10 | 11)
}

/// Install or refresh the IDT vector for one supported legacy PCI line.
/// Registration happens during boot with IF=0, after IDT.load(); changing the
/// backing table is valid because IDTR keeps pointing at the same table.
unsafe fn install_vector(irq: u8) -> Result<(), &'static str> {
    let handler = match irq {
        5 => irq5 as u32,
        9 => irq9 as u32,
        10 => irq10 as u32,
        11 => irq11 as u32,
        _ => return Err("shared IRQ line is not supported"),
    };
    crate::interrupts::idt::IDT.add((32u8 + irq) as usize, handler);
    Ok(())
}

/// Register one status-checking owner for a shared PIC IRQ.
/// Duplicate registrations are ignored.
pub fn register(irq: u8, handler: IrqHandler) -> Result<(), &'static str> {
    register_named(irq, handler, "unknown")
}

pub fn register_named(
    irq: u8,
    handler: IrqHandler,
    owner: &'static str,
) -> Result<(), &'static str> {
    if !supported(irq) {
        return Err("shared IRQ line is not supported");
    }

    unsafe {
        install_vector(irq)?;
    }

    {
        let mut lines = LINES.lock();
        let line = &mut lines[irq as usize];
        if let Some(index) = line
            .handlers
            .iter()
            .position(|h| h.is_some_and(|h| h as usize == handler as usize))
        {
            line.owners[index] = Some(owner);
            return Ok(());
        }
        let Some(index) = line.handlers.iter().position(|h| h.is_none()) else {
            return Err("too many devices on shared IRQ");
        };
        line.handlers[index] = Some(handler);
        line.owners[index] = Some(owner);
        line.stray = 0;
        line.storm_cooldown = 0;
    }

    // This may later be overwritten by main's full-mask write. late_restore_once
    // repairs it after PIT begins running.
    PICS.unmask_irq(irq);
    LATE_RESTORED.store(false, Ordering::Release);
    crate::println!("[irq] registered shared IRQ{} owner", irq);
    Ok(())
}

/// Called from PIT. Exactly once after boot's final mask rewrite, restore only
/// shared lines that have registered owners. A storm-masked line is not
/// repeatedly re-enabled because this function becomes a no-op after one pass.
pub fn late_restore_once() {
    if LATE_RESTORED.swap(true, Ordering::AcqRel) {
        return;
    }
    let lines = LINES.lock();
    for irq in 0..IRQ_LINES {
        if lines[irq].handlers.iter().any(|h| h.is_some())
            && lines[irq].storm_cooldown == 0
        {
            PICS.unmask_irq(irq as u8);
        }
    }
}

/// Let a temporarily masked shared line recover after polling bottom halves
/// have had time to clear a source whose controller lock was busy in hard IRQ.
/// A truly unclaimed level source is rate-limited instead of wedging the CPU.
pub fn maintenance_tick() {
    let mut rearm = [false; IRQ_LINES];
    {
        let mut lines = LINES.lock();
        for irq in 0..IRQ_LINES {
            let line = &mut lines[irq];
            if line.storm_cooldown > 0 {
                line.storm_cooldown -= 1;
                if line.storm_cooldown == 0 {
                    line.stray = 0;
                    rearm[irq] = line.handlers.iter().any(|h| h.is_some());
                }
            }
        }
    }
    for (irq, &should_rearm) in rearm.iter().enumerate() {
        if should_rearm {
            PICS.unmask_irq(irq as u8);
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn shared_irq_dispatch(irq: u32) {
    let irq = irq as u8;
    let mut handled = false;
    let mut mask_for_storm = false;

    let handlers = {
        let lines = LINES.lock();
        if (irq as usize) < lines.len() {
            lines[irq as usize].handlers
        } else {
            [None; HANDLERS_PER_IRQ]
        }
    };

    // Never hold the registry lock while entering a device handler. Apart
    // from shortening hard-IRQ time, this also avoids lock ordering between
    // the registry and individual audio/network controller locks.
    for handler in handlers.iter().flatten() {
        let address = *handler as usize;
        // An indirect branch from ring 0 must never be allowed to enter a
        // userspace stack if registry memory is damaged. The linker reserves
        // this exact 4 MiB higher-half interval for the kernel image.
        if !(KERNEL_IMAGE_START..KERNEL_IMAGE_END).contains(&address) {
            continue;
        }
        handled |= handler(irq);
    }

    {
        let mut lines = LINES.lock();
        if (irq as usize) < lines.len() {
            let line = &mut lines[irq as usize];
            if handled {
                HANDLED_COUNT[irq as usize].fetch_add(1, Ordering::Relaxed);
                line.stray = 0;
            } else {
                UNHANDLED_COUNT[irq as usize].fetch_add(1, Ordering::Relaxed);
                line.stray = line.stray.saturating_add(1);
                if line.stray >= STRAY_LIMIT {
                    // A level-triggered unclaimed source can livelock the CPU.
                    // Mask it long enough for polling bottom halves to clear a
                    // temporarily inaccessible controller, then retry.
                    line.stray = 0;
                    line.storm_cooldown = 20;
                    mask_for_storm = true;
                }
            }
        }
    }

    if mask_for_storm {
        PICS.mask_irq(irq);
    }

    // Exactly one EOI for the shared vector, never one per device.
    PICS.end_interrupt(32u8 + irq);
}

macro_rules! shared_irq_entry {
    ($name:ident, $irq:literal) => {
        #[unsafe(naked)]
        pub extern "C" fn $name() {
            unsafe {
                naked_asm!(
                    "cli",
                    "push ebp",
                    "push edi",
                    "push esi",
                    "push edx",
                    "push ecx",
                    "push ebx",
                    "push eax",
                    "cld",
                    "mov ax, 0x10",
                    "mov ds, ax",
                    "mov es, ax",
                    concat!("push ", stringify!($irq)),
                    "call shared_irq_dispatch",
                    "add esp, 4",
                    // CPUState-like stack here: eax..ebp then hardware eip/cs/
                    // eflags. Select data segments before restoring saved GPRs.
                    "mov ax, [esp + 32]",
                    "and ax, 3",
                    "cmp ax, 3",
                    "jne 1f",
                    "mov ax, 0x23",
                    "jmp 2f",
                    "1:",
                    "mov ax, 0x10",
                    "2:",
                    "mov ds, ax",
                    "mov es, ax",
                    "pop eax",
                    "pop ebx",
                    "pop ecx",
                    "pop edx",
                    "pop esi",
                    "pop edi",
                    "pop ebp",
                    "iretd",
                );
            }
        }
    };
}

shared_irq_entry!(irq5, 5);
shared_irq_entry!(irq9, 9);
shared_irq_entry!(irq10, 10);
shared_irq_entry!(irq11, 11);
