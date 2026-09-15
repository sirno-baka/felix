//! Shared legacy PCI INTx dispatcher.
//!
//! Old PCI machines routinely route USB, CardBus, audio and other devices to
//! the same PIC line (especially IRQ9/10/11). A handler must inspect its own
//! status registers and return `false` when the interrupt is not its own. The
//! PIC EOI is sent exactly once, after every registered owner has been checked.

use core::arch::naked_asm;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::drivers::pic::PICS;
use crate::spin::KMutex;

pub type IrqHandler = fn(u8) -> bool;

const IRQ_LINES: usize = 16;
const HANDLERS_PER_IRQ: usize = 6;
const STRAY_LIMIT: u8 = 32;

#[derive(Clone, Copy)]
struct Line {
    handlers: [Option<IrqHandler>; HANDLERS_PER_IRQ],
    stray: u8,
}

impl Line {
    const fn new() -> Self {
        Self { handlers: [None; HANDLERS_PER_IRQ], stray: 0 }
    }
}

static LINES: KMutex<[Line; IRQ_LINES]> = KMutex::new([Line::new(); IRQ_LINES]);
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
    if !supported(irq) {
        return Err("shared IRQ line is not supported");
    }

    unsafe { install_vector(irq)?; }

    {
        let mut lines = LINES.lock();
        let line = &mut lines[irq as usize];
        if line.handlers.iter().flatten().any(|&h| h as usize == handler as usize) {
            return Ok(());
        }
        let Some(slot) = line.handlers.iter_mut().find(|h| h.is_none()) else {
            return Err("too many devices on shared IRQ");
        };
        *slot = Some(handler);
        line.stray = 0;
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
        if lines[irq].handlers.iter().any(|h| h.is_some()) {
            PICS.unmask_irq(irq as u8);
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn shared_irq_dispatch(irq: u32) {
    let irq = irq as u8;
    let mut handled = false;
    let mut mask_for_storm = false;

    {
        let mut lines = LINES.lock();
        if (irq as usize) < lines.len() {
            let line = &mut lines[irq as usize];
            for handler in line.handlers.iter().flatten() {
                handled |= handler(irq);
            }

            if handled {
                line.stray = 0;
            } else {
                line.stray = line.stray.saturating_add(1);
                if line.stray >= STRAY_LIMIT {
                    // A level-triggered unclaimed source can livelock the CPU.
                    // Mask the line and leave PIT/polling fallbacks operational.
                    line.stray = 0;
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
