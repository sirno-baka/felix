//! Shared legacy PCI INTx dispatcher.
//!
//! Old PCI machines routinely route USB, CardBus, audio and other devices to
//! the same PIC line (especially IRQ9/10/11).  A handler must therefore first
//! inspect its own device status and return `false` when the interrupt is not
//! its own.  The PIC EOI is sent exactly once, after all registered owners have
//! had a chance to inspect/ack their hardware.

use core::arch::naked_asm;

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
        Self {
            handlers: [None; HANDLERS_PER_IRQ],
            stray: 0,
        }
    }
}

static LINES: KMutex<[Line; IRQ_LINES]> = KMutex::new([Line::new(); IRQ_LINES]);

/// Install the legacy PCI vectors we currently allow to be shared.
///
/// IRQ0/1/12 remain owned by PIT/keyboard/mouse.  IRQ5/9/10/11 cover the
/// normal legacy PCI routing used by QEMU and the Sony C1M-era hardware.
pub unsafe fn install(idt: &mut crate::interrupts::idt::InterruptDescriptorTable) {
    idt.add(32 + 5, irq5 as u32);
    idt.add(32 + 9, irq9 as u32);
    idt.add(32 + 10, irq10 as u32);
    idt.add(32 + 11, irq11 as u32);
}

fn supported(irq: u8) -> bool {
    matches!(irq, 5 | 9 | 10 | 11)
}

/// Register one status-checking owner for a shared PIC IRQ.
/// Duplicate registrations are ignored.
pub fn register(irq: u8, handler: IrqHandler) -> Result<(), &'static str> {
    if !supported(irq) {
        return Err("shared IRQ line is not installed in IDT");
    }

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

    // Safe during boot (IF=0), and safe later because Pic mask writes are one
    // byte and this call is not made from the interrupt itself.
    PICS.unmask_irq(irq);
    crate::println!("[irq] registered shared IRQ{} owner", irq);
    Ok(())
}

/// Re-enable all lines that currently have registered owners.  Useful after
/// code that rewrites complete PIC mask bytes.
pub fn restore_masks() {
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
                    // An unclaimed level-triggered INTx source can otherwise
                    // livelock the CPU forever. Mask first; report below after
                    // releasing the registry lock.
                    line.stray = 0;
                    mask_for_storm = true;
                }
            }
        }
    }

    if mask_for_storm {
        PICS.mask_irq(irq);
        crate::println!("[irq] IRQ{} storm: no owner claimed it; masked", irq);
    }

    // One EOI for the shared vector, never one EOI per device.
    PICS.end_interrupt(32 + irq);
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
                    // Restore DS/ES according to the interrupted CPL without
                    // clobbering the saved EAX value on the stack.
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
