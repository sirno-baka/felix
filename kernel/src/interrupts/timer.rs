// TIMER INTERRUPT HANDLER
// Triggers the scheduler and performs context switching

use crate::drivers::pic::PICS;
use crate::multitasking::task::{CPUState, WaitReason, TASK_MANAGER};
use crate::println;
use crate::time::uptime_ms;
use core::arch::asm;
use core::arch::naked_asm;

pub const TIMER_INT: u8 = 32;

// Poll the network by elapsed time, not by IRQ count, so changing the PIT
// frequency does not change the networking cadence.
const NET_POLL_EVERY_MS: u64 = 10;
static mut LAST_NET_POLL_MS: u64 = 0;

/// Naked interrupt handler for timer (IRQ0)
#[unsafe(naked)]
pub extern "C" fn timer() {
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
            // All interrupted GPRs are now safe on the stack, so AX can be
            // used to establish the kernel data selectors for Rust code.
            "cld",
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            // jiffies_inc is a normal Rust function and may clobber caller-saved
            // registers; call it only after the interrupted context is saved.
            "call jiffies_inc",
            "push esp",
            "call timer_handler",
            "add esp, 4",
            "mov esp, eax",
            "test edx, edx",
            "jz 3f",
            "call finish_kernel_handoff",
            "3:",
            // CPUState layout at the selected task's ESP:
            // eax,ebx,ecx,edx,esi,edi,ebp,eip,cs,eflags,esp,ss.
            // Pick DS/ES BEFORE restoring EAX/ECX; the previous code changed
            // CX after pop and silently corrupted userspace on every tick.
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
            "iretd"
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn timer_handler(esp: u32) -> u64 {
    unsafe {
        crate::smp::account_cpu_tick(esp as *const CPUState);
        // main.rs writes complete legacy PIC masks shortly before STI. Restore
        // registered PCI INTx lines once, after the system has actually entered
        // normal interrupt-driven operation.
        crate::drivers::shared_irq::late_restore_once();

        // Audio bottom half. This is deliberately outside the PCI shared IRQ:
        // the hard IRQ only reads/acks status and records completion. Mixing and
        // DMA refill happen here. poll() uses try_lock, so it never blocks the
        // timer when a syscall currently owns the audio core.
        static AUDIO_WAKE_PENDING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        let audio_progressed = crate::drivers::audio::poll_due()
            && crate::drivers::audio::poll();
        if audio_progressed {
            AUDIO_WAKE_PENDING.store(true, core::sync::atomic::Ordering::Release);
        }

        // === 1. Сетевой полл (неблокирующий) ===
        let now_ms = uptime_ms();
        let net_irq = crate::drivers::net::take_irq_pending();
        if net_irq || now_ms.saturating_sub(LAST_NET_POLL_MS) >= NET_POLL_EVERY_MS {
            LAST_NET_POLL_MS = now_ms;
            if !poll_network(now_ms as i64) && net_irq {
                crate::drivers::net::restore_irq_pending();
            }
        }
        crate::drivers::shared_irq::maintenance_tick();

        // TaskManager and the process-wide kernel structures are shared by all
        // processors. Keep this first SMP version simple: only one CPU may be
        // inside the scheduler/syscall core at a time.
        let new_esp = if !crate::multitasking::task::timer_may_schedule(esp as *const CPUState) {
            esp as u64
        } else if let Some(kernel) = crate::multitasking::task::try_lock_kernel() {
            crate::multitasking::task::set_kernel_lock_context(
                crate::multitasking::task::KERNEL_LOCK_CTX_BSP_TIMER,
            );
            // These sources are polled by the timer/device bottom halves. Wake
            // only their queues so restartable syscalls can recheck readiness.
            TASK_MANAGER.wake_waiters(WaitReason::Poll);
            TASK_MANAGER.wake_waiters(WaitReason::Socket);
            TASK_MANAGER.wake_waiters(WaitReason::Input);
            if AUDIO_WAKE_PENDING.swap(false, core::sync::atomic::Ordering::AcqRel) {
                TASK_MANAGER.wake_waiters(WaitReason::Audio);
            }
            let mut selected = TASK_MANAGER.schedule(esp as *mut CPUState) as u32;
            // AP timers are already live during late boot. Do not let them run
            // init against half-initialized PIC/PIT/VFS state. The first BSP
            // PIT context switch is the boot-complete barrier for SMP userspace.
            if !crate::smp::user_scheduling_enabled() {
                crate::smp::enable_user_scheduling();
            }
            selected = crate::signal::deliver_pending(selected);
            crate::multitasking::task::handoff_kernel_lock(kernel, selected)
        } else {
            esp as u64
        };

        // === 4. EOI ===
        PICS.end_interrupt(TIMER_INT);
        new_esp
    }
}

/// Безопасный полл из IRQ-контекста
unsafe fn poll_network(timestamp_ms: i64) -> bool {
    // Пытаемся взять стек без блокировки
    if let Some(mut guard) = crate::net::stack::NET_STACK.try_lock() {
        if let Some(ref mut stack) = *guard {
            stack.poll(timestamp_ms);
        }
        true
    } else {
        false
    }
    // Если лок занят (syscall как раз работает с сетью) — просто пропускаем этот тик.
    // Это нормально и безопасно.
}
