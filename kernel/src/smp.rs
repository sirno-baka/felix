//! Minimal x86 SMP bootstrap.
//!
//! The bootstrap processor discovers CPUs through the legacy Intel MP tables,
//! starts each application processor with INIT/SIPI, and gives every online CPU
//! a private stack, timer and scheduler entry point.

use alloc::collections::VecDeque;
use core::arch::{asm, global_asm, naked_asm};
use core::ptr::{copy_nonoverlapping, read_unaligned, read_volatile, write_unaligned, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, Ordering};

use crate::memory::paging::{phys_to_virt, KERNEL_PD_PHYS};
use crate::memory::resources::{ioremap, iounmap, reserve_and_ioremap, ResourceKind};

const MAX_CPUS: usize = 8;
const AP_STACK_SIZE: usize = 16 * 1024;
const TRAMPOLINE_PHYS: u32 = 0x0000_8000;
const TRAMPOLINE_VECTOR: u32 = TRAMPOLINE_PHYS >> 12;
const PARAM_CR3: u32 = 0x0000_8f00;
const PARAM_STACK: u32 = 0x0000_8f04;
const PARAM_ENTRY: u32 = 0x0000_8f08;
const PARAM_GDTR: u32 = 0x0000_8f0c;
const PARAM_GDT: u32 = 0x0000_8f20;

const LAPIC_ID: usize = 0x020;
const LAPIC_EOI: usize = 0x0b0;
const LAPIC_SVR: usize = 0x0f0;
const LAPIC_ESR: usize = 0x280;
const LAPIC_ICR_LOW: usize = 0x300;
const LAPIC_ICR_HIGH: usize = 0x310;
const LAPIC_LVT_TIMER: usize = 0x320;
const LAPIC_LVT_LINT0: usize = 0x350;
const LAPIC_TIMER_INITIAL: usize = 0x380;
const LAPIC_TIMER_DIVIDE: usize = 0x3e0;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;
pub const AP_TIMER_VECTOR: u8 = 0xf0;
pub const WORK_IPI_VECTOR: u8 = 0xf1;
pub const TLB_IPI_VECTOR: u8 = 0xf2;
const AP_TIMER_PERIODIC: u32 = 1 << 17;
const LVT_DELIVERY_EXTINT: u32 = 0x7 << 8;

#[repr(C, align(16))]
struct ApStacks([[u8; AP_STACK_SIZE]; MAX_CPUS]);

static mut AP_STACKS: ApStacks = ApStacks([[0; AP_STACK_SIZE]; MAX_CPUS]);
static mut AP_GDTS: [crate::gdt::PerCpuGdt; MAX_CPUS] =
    [const { crate::gdt::PerCpuGdt::new() }; MAX_CPUS];
static AP_ONLINE: AtomicU32 = AtomicU32::new(1);
static ONLINE_MASK: AtomicU32 = AtomicU32::new(1);
static LAPIC_VIRT: AtomicU32 = AtomicU32::new(0);
static CPU_APIC_IDS: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(u32::MAX) }; MAX_CPUS];
static AP_TIMER_TICKS: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static CPU_USER_TICKS: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static CPU_SYSTEM_TICKS: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static CPU_IDLE_TICKS: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static WORK_QUEUE: interrupt_sync::SpinMutex<VecDeque<SmpJob>> =
    interrupt_sync::SpinMutex::new(VecDeque::new());
static WORK_SUBMITTED: AtomicU32 = AtomicU32::new(0);
static WORK_COMPLETED: AtomicU32 = AtomicU32::new(0);
static WORK_CPU_MASK: AtomicU32 = AtomicU32::new(0);
static USER_SCHEDULING: AtomicBool = AtomicBool::new(false);
static CURRENT_TASK: [AtomicI8; MAX_CPUS] =
    [const { AtomicI8::new(-1) }; MAX_CPUS];
static CURRENT_CR3: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];
static IDLE_ESP: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static TLB_TARGET_CR3: AtomicU32 = AtomicU32::new(0);
static TLB_TARGET_PAGE: AtomicU32 = AtomicU32::new(0);
static TLB_ACKS: AtomicU32 = AtomicU32::new(0);
static TLB_SHOOTDOWN_BROKEN: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct SmpJob {
    function: fn(u32, usize),
    argument: u32,
}

/// Queue non-blocking kernel work for any online application processor.
/// Jobs must not sleep and must not access BSP-only device state.
pub fn submit(function: fn(u32, usize), argument: u32) {
    WORK_QUEUE.lock().push_back(SmpJob { function, argument });
    WORK_SUBMITTED.fetch_add(1, Ordering::Release);
    let base = LAPIC_VIRT.load(Ordering::Acquire) as *mut u8;
    if !base.is_null() {
        unsafe {
            // Fixed-delivery IPI to all processors except the sender.
            let _ = wait_icr(base);
            lapic_write(base, LAPIC_ICR_LOW, 0x000c_0000 | WORK_IPI_VECTOR as u32);
        }
    }
}

fn run_pending_work(cpu_slot: usize) -> bool {
    let job = WORK_QUEUE.lock().pop_front();
    let Some(job) = job else {
        return false;
    };
    (job.function)(job.argument, cpu_slot);
    WORK_CPU_MASK.fetch_or(1 << cpu_slot, Ordering::Relaxed);
    WORK_COMPLETED.fetch_add(1, Ordering::Release);
    true
}

fn smoke_job(seed: u32, cpu_slot: usize) {
    // CPU-only workload with a deterministic result. `black_box` prevents the
    // optimizer from reducing it to a constant while APs execute in parallel.
    let mut value = seed ^ (cpu_slot as u32).wrapping_mul(0x9e37_79b9);
    for _ in 0..100_000 {
        value ^= value << 13;
        value ^= value >> 17;
        value ^= value << 5;
        core::hint::black_box(value);
    }
}

fn verify_parallel_work(ap_count: usize) {
    let before = WORK_COMPLETED.load(Ordering::Acquire);
    for i in 0..ap_count {
        submit(smoke_job, 0x534d_5000 | i as u32);
    }
    let expected = before + ap_count as u32;
    for _ in 0..20_000_000 {
        if WORK_COMPLETED.load(Ordering::Acquire) >= expected {
            break;
        }
        core::hint::spin_loop();
    }
    crate::println!(
        "[smp] worker test: completed={}/{} cpu-mask={:#x}",
        WORK_COMPLETED.load(Ordering::Acquire) - before,
        ap_count,
        WORK_CPU_MASK.load(Ordering::Acquire)
    );
}

global_asm!(
    r#"
    .section .rodata.ap_trampoline,"a"
    .code16
    .global ap_trampoline_start
ap_trampoline_start:
    cli
    cld
    xor ax, ax
    mov ds, ax
    lgdt [0x8f0c]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    // ljmp 0x0008:0x8040, encoded explicitly so the assembler cannot choose
    // a 32-bit immediate while this block is still in .code16.
    .byte 0xea, 0x40, 0x80, 0x08, 0x00

    .org 0x40
    .code32
ap_protected:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov eax, dword ptr [0x8f00]
    mov cr3, eax
    mov eax, cr4
    or eax, 0x10
    mov cr4, eax
    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    mov esp, dword ptr [0x8f04]
    xor ebp, ebp
    mov eax, dword ptr [0x8f08]
    jmp eax

    .global ap_trampoline_end
ap_trampoline_end:
    .code32
"#
);

unsafe extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_end: u8;
}

#[derive(Clone, Copy)]
struct MpInfo {
    lapic_phys: u32,
    apic_ids: [u8; MAX_CPUS],
    cpu_count: usize,
}

#[inline]
unsafe fn phys_ptr<T>(phys: u32) -> *const T {
    phys_to_virt(phys) as *const T
}

unsafe fn checksum(phys: u32, len: usize) -> u8 {
    let ptr = phys_ptr::<u8>(phys);
    let mut sum = 0u8;
    for i in 0..len {
        sum = sum.wrapping_add(read_volatile(ptr.add(i)));
    }
    sum
}

unsafe fn scan_mp_range(start: u32, len: u32) -> Option<u32> {
    let end = start.checked_add(len)?;
    let mut addr = start;
    while addr + 16 <= end {
        if read_unaligned(phys_ptr::<u32>(addr)) == u32::from_le_bytes(*b"_MP_")
            && checksum(addr, 16) == 0
        {
            return Some(addr);
        }
        addr += 16;
    }
    None
}

unsafe fn find_mp_floating_pointer() -> Option<u32> {
    let ebda_segment = read_unaligned(phys_ptr::<u16>(0x40e)) as u32;
    let ebda = ebda_segment << 4;
    if ebda != 0 {
        if let Some(addr) = scan_mp_range(ebda, 1024) {
            return Some(addr);
        }
    }

    let base_kb = read_unaligned(phys_ptr::<u16>(0x413)) as u32;
    if base_kb >= 1 {
        if let Some(addr) = scan_mp_range(base_kb * 1024 - 1024, 1024) {
            return Some(addr);
        }
    }
    scan_mp_range(0x000f_0000, 0x0001_0000)
}

unsafe fn discover_mp() -> Option<MpInfo> {
    let mp = find_mp_floating_pointer()?;
    let config_phys = read_unaligned(phys_ptr::<u32>(mp + 4));
    if config_phys == 0 || read_unaligned(phys_ptr::<u32>(config_phys)) != u32::from_le_bytes(*b"PCMP") {
        return None;
    }
    let table_len = read_unaligned(phys_ptr::<u16>(config_phys + 4)) as usize;
    if table_len < 44 || checksum(config_phys, table_len) != 0 {
        return None;
    }

    let entries = read_unaligned(phys_ptr::<u16>(config_phys + 34)) as usize;
    let lapic_phys = read_unaligned(phys_ptr::<u32>(config_phys + 36));
    let mut info = MpInfo {
        lapic_phys,
        apic_ids: [0; MAX_CPUS],
        cpu_count: 0,
    };
    let mut cursor = config_phys + 44;
    let table_end = config_phys + table_len as u32;
    for _ in 0..entries {
        if cursor >= table_end {
            return None;
        }
        let kind = read_volatile(phys_ptr::<u8>(cursor));
        let size = if kind == 0 { 20 } else { 8 };
        if cursor + size > table_end {
            return None;
        }
        if kind == 0 {
            let flags = read_volatile(phys_ptr::<u8>(cursor + 3));
            if flags & 1 != 0 && info.cpu_count < MAX_CPUS {
                info.apic_ids[info.cpu_count] = read_volatile(phys_ptr::<u8>(cursor + 1));
                info.cpu_count += 1;
            }
        }
        cursor += size;
    }
    Some(info)
}

unsafe fn scan_rsdp_range(start: u32, len: u32) -> Option<u32> {
    let end = start.checked_add(len)?;
    let mut addr = start;
    while addr + 20 <= end {
        let ptr = phys_ptr::<u8>(addr);
        let mut signature = [0u8; 8];
        for (i, byte) in signature.iter_mut().enumerate() {
            *byte = read_volatile(ptr.add(i));
        }
        if &signature == b"RSD PTR " && checksum(addr, 20) == 0 {
            return Some(addr);
        }
        addr += 16;
    }
    None
}

unsafe fn find_rsdp() -> Option<u32> {
    let ebda = (read_unaligned(phys_ptr::<u16>(0x40e)) as u32) << 4;
    if ebda != 0 {
        if let Some(addr) = scan_rsdp_range(ebda, 1024) {
            return Some(addr);
        }
    }
    scan_rsdp_range(0x000e_0000, 0x0002_0000)
}

unsafe fn checksum_ptr(ptr: *const u8, len: usize) -> u8 {
    let mut sum = 0u8;
    for i in 0..len {
        sum = sum.wrapping_add(read_volatile(ptr.add(i)));
    }
    sum
}

unsafe fn map_sdt(phys: u32, signature: [u8; 4]) -> Option<(*const u8, usize)> {
    let header = ioremap(phys as u64, 36, "acpi-sdt-header").ok()?;
    let header_ptr = header.0 as *const u8;
    if read_unaligned(header_ptr as *const u32) != u32::from_le_bytes(signature) {
        let _ = iounmap(header);
        return None;
    }
    let len = read_unaligned(header_ptr.add(4) as *const u32) as usize;
    let _ = iounmap(header);
    if len < 36 {
        return None;
    }
    let table = ioremap(phys as u64, len, "acpi-sdt").ok()?;
    let ptr = table.0 as *const u8;
    if checksum_ptr(ptr, len) != 0 {
        let _ = iounmap(table);
        return None;
    }
    Some((ptr, len))
}

unsafe fn discover_acpi() -> Option<MpInfo> {
    let rsdp = find_rsdp()?;
    let rsdt = read_unaligned(phys_ptr::<u32>(rsdp + 16));
    let (rsdt_ptr, rsdt_len) = map_sdt(rsdt, *b"RSDT")?;
    let entry_count = (rsdt_len - 36) / 4;
    let mut madt = 0;
    for i in 0..entry_count {
        let table = read_unaligned(rsdt_ptr.add(36 + i * 4) as *const u32);
        let header = ioremap(table as u64, 4, "acpi-signature").ok()?;
        let is_madt = read_unaligned(header.0 as *const u32) == u32::from_le_bytes(*b"APIC");
        let _ = iounmap(header);
        if is_madt {
            madt = table;
            break;
        }
    }
    if madt == 0 {
        return None;
    }

    let (madt_ptr, madt_len) = map_sdt(madt, *b"APIC")?;
    if madt_len < 44 {
        return None;
    }
    let mut info = MpInfo {
        lapic_phys: read_unaligned(madt_ptr.add(36) as *const u32),
        apic_ids: [0; MAX_CPUS],
        cpu_count: 0,
    };
    let mut cursor = 44usize;
    while cursor + 2 <= madt_len {
        let kind = read_volatile(madt_ptr.add(cursor));
        let len = read_volatile(madt_ptr.add(cursor + 1)) as usize;
        if len < 2 || cursor + len > madt_len {
            return None;
        }
        if kind == 0 && len >= 8 {
            let flags = read_unaligned(madt_ptr.add(cursor + 4) as *const u32);
            if flags & 3 != 0 && info.cpu_count < MAX_CPUS {
                info.apic_ids[info.cpu_count] = read_volatile(madt_ptr.add(cursor + 3));
                info.cpu_count += 1;
            }
        }
        cursor += len;
    }
    Some(info)
}

#[inline]
unsafe fn lapic_read(base: *mut u8, reg: usize) -> u32 {
    read_volatile(base.add(reg) as *const u32)
}

#[inline]
unsafe fn lapic_write(base: *mut u8, reg: usize, value: u32) {
    write_volatile(base.add(reg) as *mut u32, value);
    let _ = lapic_read(base, LAPIC_ID);
}

fn cpu_slot_for_lapic(base: *mut u8) -> Option<usize> {
    let apic_id = unsafe { lapic_read(base, LAPIC_ID) >> 24 };
    CPU_APIC_IDS
        .iter()
        .position(|id| id.load(Ordering::Relaxed) == apic_id)
}

pub fn current_cpu_index() -> usize {
    // Do not read LAPIC_ID through MMIO here.  This function is used in every
    // scheduler/lock transition, and the legacy LAPIC window on some real
    // chipsets can occasionally stall an AP read indefinitely.  CPUID.1:EBX
    // reports the same initial 8-bit APIC ID without touching the MMIO bus.
    let apic_id = unsafe { core::arch::x86::__cpuid(1).ebx >> 24 };
    CPU_APIC_IDS
        .iter()
        .position(|id| id.load(Ordering::Relaxed) == apic_id)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Default)]
pub struct CpuTimes {
    pub user: u32,
    pub system: u32,
    pub idle: u32,
}

pub fn online_cpu_count() -> usize {
    (AP_ONLINE.load(Ordering::Acquire) as usize).clamp(1, MAX_CPUS)
}

pub fn cpu_slot_count() -> usize { MAX_CPUS }

pub fn cpu_times(cpu: usize) -> Option<CpuTimes> {
    if cpu >= MAX_CPUS || ONLINE_MASK.load(Ordering::Acquire) & (1 << cpu) == 0 {
        return None;
    }
    Some(CpuTimes {
        user: CPU_USER_TICKS[cpu].load(Ordering::Relaxed),
        system: CPU_SYSTEM_TICKS[cpu].load(Ordering::Relaxed),
        idle: CPU_IDLE_TICKS[cpu].load(Ordering::Relaxed),
    })
}

/// Account the execution interrupted by this CPU's scheduler timer. Counters
/// intentionally use local timer ticks: percentages are computed from deltas,
/// so AP timer calibration is not required.
static LAST_TIMER_EIP: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static LAST_TIMER_CS: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static LAST_TIMER_EFLAGS: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];
static SYSCALL_NUMBER: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(u32::MAX) }; MAX_CPUS];
static SYSCALL_PHASE: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];

static SCHEDULER_STAGE: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];

// Unlike LAST_TIMER_EIP, these markers advance with IF=0. F12 on another
// CPU can therefore locate a stalled syscall without taking the kernel lock.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum KernelWork {
    None, SpawnArgs, SpawnPath, SpawnRead, SpawnSlot, TaskPageDir, TaskStack,
    TaskMetadata, SpawnMappings, SpawnUserStack, SpawnHeap, SpawnElf,
    SpawnUserArgs, SpawnFds, SpawnPublish, SpawnFinish, ReapThreads,
    ReapUserStack, ReapUserPages, ReapPageDir, ReapKernelStack,
}

static KERNEL_WORK: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];

pub fn trace_kernel_work(work: KernelWork, progress: u32) {
    // One atomic snapshot keeps the stage and its progress consistent.
    KERNEL_WORK[current_cpu_index()].store(
        ((work as u32) << 24) | (progress & 0x00ff_ffff), Ordering::Relaxed);
}

fn kernel_work_name(work: u32) -> &'static str {
    const NAMES: [&str; 21] = [
        "none", "spawn/args", "spawn/path", "spawn/read", "spawn/slot",
        "task/pd", "task/kstack", "task/metadata", "spawn/mappings",
        "spawn/ustack", "spawn/heap", "spawn/elf", "spawn/argv",
        "spawn/fds", "spawn/publish", "spawn/finish", "reap/threads",
        "reap/ustack", "reap/pages", "reap/pd", "reap/kstack",
    ];
    NAMES.get(work as usize).copied().unwrap_or("unknown")
}

pub fn trace_scheduler(stage: u32) {
    SCHEDULER_STAGE[current_cpu_index()].store(stage, Ordering::Relaxed);
}

pub fn trace_syscall(number: u32, phase: u32) {
    let cpu = current_cpu_index();
    if phase == 1 {
        KERNEL_WORK[cpu].store(0, Ordering::Relaxed);
    }
    SYSCALL_NUMBER[cpu].store(number, Ordering::Relaxed);
    SYSCALL_PHASE[cpu].store(phase, Ordering::Relaxed);
}

pub fn trace_kernel_return() {
    trace_scheduler(0);
    trace_kernel_work(KernelWork::None, 0);
    SYSCALL_PHASE[current_cpu_index()].store(0, Ordering::Relaxed);
}

pub fn account_cpu_tick(state: *const crate::multitasking::task::CPUState) {
    let cpu = current_cpu_index();
    if cpu >= MAX_CPUS || state.is_null() {
        return;
    }
    LAST_TIMER_EIP[cpu].store(unsafe { (*state).eip }, Ordering::Relaxed);
    LAST_TIMER_CS[cpu].store(unsafe { (*state).cs }, Ordering::Relaxed);
    LAST_TIMER_EFLAGS[cpu].store(unsafe { (*state).eflags }, Ordering::Relaxed);
    let user = unsafe { (*state).cs & 3 == 3 };
    let current = CURRENT_TASK[cpu].load(Ordering::Relaxed);
    if current <= 0 {
        CPU_IDLE_TICKS[cpu].fetch_add(1, Ordering::Relaxed);
    } else if user {
        CPU_USER_TICKS[cpu].fetch_add(1, Ordering::Relaxed);
    } else {
        CPU_SYSTEM_TICKS[cpu].fetch_add(1, Ordering::Relaxed);
    }
}

pub fn current_task_slot() -> i8 {
    CURRENT_TASK[current_cpu_index()].load(Ordering::Acquire)
}

pub fn task_slot_on_cpu(cpu: usize) -> i8 {
    CURRENT_TASK
        .get(cpu)
        .map_or(-1, |slot| slot.load(Ordering::Acquire))
}

pub fn set_current_task_slot(slot: i8) {
    CURRENT_TASK[current_cpu_index()].store(slot, Ordering::Release);
}

pub fn set_current_cr3(page_dir_phys: u32) {
    CURRENT_CR3[current_cpu_index()].store(page_dir_phys, Ordering::Release);
}

pub fn set_idle_esp(cpu: usize, esp: u32) {
    IDLE_ESP[cpu].store(esp, Ordering::Release);
}

pub fn idle_esp(cpu: usize) -> u32 {
    IDLE_ESP[cpu].load(Ordering::Acquire)
}

pub fn user_scheduling_enabled() -> bool {
    USER_SCHEDULING.load(Ordering::Acquire)
}

pub fn enable_user_scheduling() {
    USER_SCHEDULING.store(true, Ordering::Release);
    crate::println!("[smp] AP userspace scheduling enabled");
}

pub unsafe fn set_cpu_kernel_stack(cpu: usize, stack: u32) {
    if cpu == 0 {
        crate::gdt::TSS.esp0 = stack;
    } else if cpu < MAX_CPUS {
        AP_GDTS[cpu].set_kernel_stack(stack);
    }
}

unsafe fn configure_local_timer(base: *mut u8) {
    lapic_write(base, LAPIC_SVR, lapic_read(base, LAPIC_SVR) | 0x100 | 0xff);
    // Divide the Local APIC timer clock by 16 and run a periodic interrupt.
    // Exact frequency is intentionally left uncalibrated at this stage; the
    // timer is used to prove independent per-CPU interrupt delivery.
    lapic_write(base, LAPIC_TIMER_DIVIDE, 0x3);
    lapic_write(
        base,
        LAPIC_LVT_TIMER,
        AP_TIMER_PERIODIC | AP_TIMER_VECTOR as u32,
    );
    lapic_write(base, LAPIC_TIMER_INITIAL, 1_000_000);
}

/// Route the cascaded 8259 PIC through the BSP Local APIC. Some firmware
/// leaves LINT0 masked while booting in legacy PIC mode; QEMU happens to leave
/// a usable virtual-wire setup, but real machines are not required to do so.
unsafe fn configure_bsp_virtual_wire(base: *mut u8) {
    let previous = lapic_read(base, LAPIC_LVT_LINT0);
    // Preserve firmware-selected polarity and trigger mode. The vector field
    // is ignored for ExtINT delivery and the mask bit is deliberately clear.
    let electrical = previous & ((1 << 13) | (1 << 15));
    lapic_write(base, LAPIC_LVT_LINT0, LVT_DELIVERY_EXTINT | electrical);
    crate::println!(
        "[smp] BSP virtual wire LINT0={:#010x} (was {:#010x})",
        lapic_read(base, LAPIC_LVT_LINT0),
        previous
    );
}

#[unsafe(naked)]
pub extern "C" fn ap_timer_interrupt() {
    unsafe {
        naked_asm!(
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
            "push esp",
            "call ap_timer_handler",
            "add esp, 4",
            "mov esp, eax",
            "test edx, edx",
            "jz 3f",
            "call finish_kernel_handoff",
            "3:",
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

#[unsafe(no_mangle)]
extern "C" fn ap_timer_handler(esp: u32) -> u64 {
    let base = LAPIC_VIRT.load(Ordering::Acquire) as *mut u8;
    if base.is_null() {
        return esp as u64;
    }
    account_cpu_tick(esp as *const crate::multitasking::task::CPUState);
    if let Some(slot) = cpu_slot_for_lapic(base) {
        AP_TIMER_TICKS[slot].fetch_add(1, Ordering::Relaxed);
    }
    let new_esp = if user_scheduling_enabled()
        && crate::multitasking::task::timer_may_schedule(esp as *const crate::multitasking::task::CPUState)
    {
        let Some(kernel) = crate::multitasking::task::try_lock_kernel() else {
            unsafe { lapic_write(base, LAPIC_EOI, 0) };
            return esp as u64;
        };
        crate::multitasking::task::set_kernel_lock_context(
            crate::multitasking::task::KERNEL_LOCK_CTX_AP_TIMER,
        );
        trace_scheduler(1);
        let next = unsafe {
            crate::multitasking::task::TASK_MANAGER
                .schedule(esp as *mut crate::multitasking::task::CPUState) as u32
        };
        let next = crate::signal::deliver_pending(next);
        trace_scheduler(9);
        crate::multitasking::task::handoff_kernel_lock(kernel, next)
    } else {
        esp as u64
    };
    unsafe { lapic_write(base, LAPIC_EOI, 0) };
    new_esp
}

#[unsafe(naked)]
pub extern "C" fn work_ipi_interrupt() {
    unsafe {
        naked_asm!(
            "push ebp",
            "push edi",
            "push esi",
            "push edx",
            "push ecx",
            "push ebx",
            "push eax",
            "call work_ipi_ack",
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

#[unsafe(no_mangle)]
extern "C" fn work_ipi_ack() {
    let base = LAPIC_VIRT.load(Ordering::Acquire) as *mut u8;
    if !base.is_null() {
        unsafe { lapic_write(base, LAPIC_EOI, 0) };
    }
}

pub fn shootdown_tlb(page_dir_phys: u32, page: u32) -> bool {
    if TLB_SHOOTDOWN_BROKEN.load(Ordering::Acquire) {
        return false;
    }
    let base = LAPIC_VIRT.load(Ordering::Acquire) as *mut u8;
    if base.is_null() {
        return true;
    }
    let sender = current_cpu_index();
    let mut targets = [0u8; MAX_CPUS];
    let mut count = 0usize;
    for cpu in 0..MAX_CPUS {
        if ONLINE_MASK.load(Ordering::Acquire) & (1 << cpu) != 0 && cpu != sender && CURRENT_CR3[cpu].load(Ordering::Acquire) == page_dir_phys {
            targets[count] = CPU_APIC_IDS[cpu].load(Ordering::Acquire) as u8;
            count += 1;
        }
    }
    if count == 0 {
        return true;
    }
    TLB_TARGET_CR3.store(page_dir_phys, Ordering::Relaxed);
    TLB_TARGET_PAGE.store(page & !0xfff, Ordering::Relaxed);
    TLB_ACKS.store(0, Ordering::Release);
    for apic_id in targets[..count].iter().copied() {
        if !unsafe { send_ipi(base, apic_id, TLB_IPI_VECTOR as u32) } {
            TLB_SHOOTDOWN_BROKEN.store(true, Ordering::Release);
            return false;
        }
    }
    for _ in 0..10_000_000 {
        if TLB_ACKS.load(Ordering::Acquire) >= count as u32 {
            return true;
        }
        core::hint::spin_loop();
    }
    // Never let one missing IPI acknowledgement freeze the giant kernel lock.
    // The caller must retain the physical frame and virtual range, so a stale
    // remote TLB entry cannot alias reused memory. Disable later transactions
    // because a delayed acknowledgement must not satisfy a newer request.
    TLB_SHOOTDOWN_BROKEN.store(true, Ordering::Release);
    false
}

#[unsafe(naked)]
pub extern "C" fn tlb_ipi_interrupt() {
    unsafe {
        naked_asm!(
            "push ds", "push es",
            "push eax", "push ecx", "push edx", "cld",
            "mov ax, 0x10", "mov ds, ax", "mov es, ax",
            "call tlb_ipi_ack",
            "pop edx", "pop ecx", "pop eax",
            "pop es", "pop ds",
            "iretd",
        );
    }
}

#[unsafe(no_mangle)]
extern "C" fn tlb_ipi_ack() {
    let current: u32;
    unsafe { asm!("mov {}, cr3", out(reg) current, options(nomem, nostack)) };
    if current == TLB_TARGET_CR3.load(Ordering::Acquire) {
        crate::memory::paging::PageDirectory::flush_page(TLB_TARGET_PAGE.load(Ordering::Relaxed));
    }
    TLB_ACKS.fetch_add(1, Ordering::Release);
    let base = LAPIC_VIRT.load(Ordering::Acquire) as *mut u8;
    if !base.is_null() {
        unsafe { lapic_write(base, LAPIC_EOI, 0) };
    }
}

unsafe fn wait_icr(base: *mut u8) -> bool {
    for _ in 0..1_000_000 {
        if lapic_read(base, LAPIC_ICR_LOW) & ICR_DELIVERY_PENDING == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

unsafe fn short_delay() {
    for _ in 0..200_000 {
        core::hint::spin_loop();
    }
}

unsafe fn send_ipi(base: *mut u8, apic_id: u8, command: u32) -> bool {
    if !wait_icr(base) {
        return false;
    }
    lapic_write(base, LAPIC_ICR_HIGH, (apic_id as u32) << 24);
    lapic_write(base, LAPIC_ICR_LOW, command);
    wait_icr(base)
}

unsafe fn install_trampoline() -> Result<(), &'static str> {
    let start = &ap_trampoline_start as *const u8;
    let end = &ap_trampoline_end as *const u8;
    let len = end.offset_from(start) as usize;
    if len == 0 || len > (PARAM_CR3 - TRAMPOLINE_PHYS) as usize {
        return Err("AP trampoline does not fit low-memory page");
    }
    copy_nonoverlapping(start, phys_to_virt(TRAMPOLINE_PHYS) as *mut u8, len);

    write_unaligned(phys_to_virt(PARAM_CR3) as *mut u32, KERNEL_PD_PHYS);
    write_unaligned(phys_to_virt(PARAM_ENTRY) as *mut u32, ap_entry as usize as u32);
    write_unaligned(phys_to_virt(PARAM_GDTR) as *mut u16, 24 - 1);
    write_unaligned(phys_to_virt(PARAM_GDTR + 2) as *mut u32, PARAM_GDT);
    write_unaligned(phys_to_virt(PARAM_GDT) as *mut u64, 0);
    write_unaligned(phys_to_virt(PARAM_GDT + 8) as *mut u64, 0x00cf_9a00_0000_ffff);
    write_unaligned(phys_to_virt(PARAM_GDT + 16) as *mut u64, 0x00cf_9200_0000_ffff);
    Ok(())
}

/// Discover and start all enabled application processors.
pub fn init() {
    let Some(info) = (unsafe { discover_acpi().or_else(|| discover_mp()) }) else {
        crate::println!("[smp] no ACPI MADT or Intel MP table; running uniprocessor");
        return;
    };
    if info.cpu_count <= 1 {
        crate::println!("[smp] one processor reported");
        return;
    }

    let lapic = match reserve_and_ioremap(
        info.lapic_phys as u64,
        0x1000,
        ResourceKind::Mmio,
        "local-apic",
    ) {
        Ok(v) => v.0 as *mut u8,
        Err(_) => {
            crate::println!("[smp] failed to map Local APIC");
            return;
        }
    };
    LAPIC_VIRT.store(lapic as u32, Ordering::Release);

    unsafe {
        if install_trampoline().is_err() {
            crate::println!("[smp] AP trampoline setup failed");
            return;
        }
        lapic_write(lapic, LAPIC_SVR, lapic_read(lapic, LAPIC_SVR) | 0x100 | 0xff);
        lapic_write(lapic, LAPIC_EOI, 0);
        lapic_write(lapic, LAPIC_ESR, 0);

        let bsp_id = (lapic_read(lapic, LAPIC_ID) >> 24) as u8;
        // ACPI and MP tables do not promise that their first processor entry is
        // the BSP. The scheduler reserves logical cpu0 for the BSP because it
        // owns the PIT and all legacy device IRQs.
        CPU_APIC_IDS[0].store(bsp_id as u32, Ordering::Release);
        let mut logical = 1usize;
        for &apic_id in &info.apic_ids[..info.cpu_count] {
            if apic_id != bsp_id && logical < MAX_CPUS {
                CPU_APIC_IDS[logical].store(apic_id as u32, Ordering::Release);
                logical += 1;
            }
        }
        configure_bsp_virtual_wire(lapic);
        crate::println!(
            "[smp] topology: {} CPU(s), BSP APIC {}, LAPIC={:#x}",
            info.cpu_count,
            bsp_id,
            info.lapic_phys
        );

        for slot in 1..logical {
            let apic_id = CPU_APIC_IDS[slot].load(Ordering::Acquire) as u8;
            let before = AP_ONLINE.load(Ordering::Acquire);
            let stack_top = AP_STACKS.0[slot].as_ptr().add(AP_STACK_SIZE) as u32;
            write_unaligned(phys_to_virt(PARAM_STACK) as *mut u32, stack_top);
            core::sync::atomic::fence(Ordering::SeqCst);
            crate::println!("[smp] starting APIC {} stack={:#x}", apic_id, stack_top);

            // INIT assert/deassert followed by two SIPIs, as required by the
            // universal startup algorithm for integrated Local APICs.
            if !send_ipi(lapic, apic_id, 0x0000_c500) {
                crate::println!("[smp] APIC {} INIT timed out", apic_id);
                break;
            }
            short_delay();
            crate::println!("[smp] APIC {} INIT asserted", apic_id);
            let _ = send_ipi(lapic, apic_id, 0x0000_8500);
            short_delay();
            crate::println!("[smp] APIC {} INIT deasserted", apic_id);
            let sipi = 0x0000_0600 | TRAMPOLINE_VECTOR;
            let _ = send_ipi(lapic, apic_id, sipi);
            short_delay();
            crate::println!("[smp] APIC {} SIPI sent", apic_id);
            if AP_ONLINE.load(Ordering::Acquire) == before {
                let _ = send_ipi(lapic, apic_id, sipi);
            }

            let mut online = false;
            for _ in 0..2_000_000 {
                if AP_ONLINE.load(Ordering::Acquire) > before {
                    online = true;
                    break;
                }
                core::hint::spin_loop();
            }
            if online {
                crate::println!("[smp] APIC {} online", apic_id);
            } else {
                crate::println!("[smp] APIC {} startup timed out", apic_id);
                // Parameters are shared: a late AP must not take the next AP stack.
                break;
            }
        }
    }
    crate::println!("[smp] {} processor(s) online", AP_ONLINE.load(Ordering::Acquire));

    // Give each AP timer enough time to fire at least once, then report the
    // per-CPU counters. This catches an AP that reached Rust but cannot receive
    // interrupts because its IDT/LAPIC setup is incomplete.
    unsafe { short_delay() };
    for slot in 0..info.cpu_count {
        let apic_id = CPU_APIC_IDS[slot].load(Ordering::Acquire) as u8;
        if apic_id != unsafe { (lapic_read(lapic, LAPIC_ID) >> 24) as u8 } {
            crate::println!(
                "[smp] APIC {} local-timer ticks={}",
                apic_id,
                AP_TIMER_TICKS[slot].load(Ordering::Acquire)
            );
        }
    }
    verify_parallel_work(AP_ONLINE.load(Ordering::Acquire).saturating_sub(1) as usize);
}

#[unsafe(no_mangle)]
extern "C" fn ap_entry() -> ! {
    let lapic = LAPIC_VIRT.load(Ordering::Acquire) as *mut u8;
    let Some(slot) = cpu_slot_for_lapic(lapic) else {
        // Never alias an unrecognised AP onto CPU0's per-CPU state/stack.
        // A duplicate CPU slot corrupts CURRENT_TASK, CR3 tracking and TSS state.
        loop {
            unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
        }
    };
    let stack_top = unsafe { AP_STACKS.0[slot].as_ptr().add(AP_STACK_SIZE) as u32 };
    unsafe {
        AP_GDTS[slot].init_and_load(stack_top);
        crate::interrupts::idt::IDT.load();
        configure_local_timer(lapic);
    }
    set_current_cr3(unsafe { KERNEL_PD_PHYS });
    ONLINE_MASK.fetch_or(1 << slot, Ordering::Release);
    AP_ONLINE.fetch_add(1, Ordering::Release);
    loop {
        let _ = run_pending_work(slot);
        unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
    }
}

/// Read only atomics: F12 must also work while another CPU holds the kernel lock.
pub struct DebugSnapshot;

impl core::fmt::Display for DebugSnapshot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let lock_ctx = crate::multitasking::task::KERNEL_LOCK_CONTEXT.load(Ordering::Relaxed);
        let lock_ctx_name = match lock_ctx {
            crate::multitasking::task::KERNEL_LOCK_CTX_SYSCALL => "syscall",
            crate::multitasking::task::KERNEL_LOCK_CTX_BSP_TIMER => "bsp_timer",
            crate::multitasking::task::KERNEL_LOCK_CTX_AP_TIMER => "ap_timer",
            crate::multitasking::task::KERNEL_LOCK_CTX_EXCEPTION => "exception",
            _ => "none",
        };
        writeln!(f, "PIT={} kernel_lock={} userspace={} owner={} lock_ctx={}",
            crate::time::jiffies(),
            crate::multitasking::task::SMP_KERNEL_LOCK.is_locked(),
            user_scheduling_enabled(),
            crate::multitasking::task::KERNEL_LOCK_OWNER.load(Ordering::Relaxed),
            lock_ctx_name)?;
        for cpu in 0..MAX_CPUS {
            if ONLINE_MASK.load(Ordering::Acquire) & (1 << cpu) == 0 { continue; }
            writeln!(f, "cpu{} apic={} slot={} ap_ticks={} u/s/i={}/{}/{} cr3={:#x}",
                cpu, CPU_APIC_IDS[cpu].load(Ordering::Relaxed),
                CURRENT_TASK[cpu].load(Ordering::Relaxed),
                AP_TIMER_TICKS[cpu].load(Ordering::Relaxed),
                CPU_USER_TICKS[cpu].load(Ordering::Relaxed),
                CPU_SYSTEM_TICKS[cpu].load(Ordering::Relaxed),
                CPU_IDLE_TICKS[cpu].load(Ordering::Relaxed),
                CURRENT_CR3[cpu].load(Ordering::Relaxed))?;
            let last_flags = LAST_TIMER_EFLAGS[cpu].load(Ordering::Relaxed);
            writeln!(f, "  eip={:#010x} cs={:#x} eflags={:#010x} IF={} last_syscall={} phase={} sched={}",
                LAST_TIMER_EIP[cpu].load(Ordering::Relaxed),
                LAST_TIMER_CS[cpu].load(Ordering::Relaxed),
                last_flags,
                (last_flags >> 9) & 1,
                SYSCALL_NUMBER[cpu].load(Ordering::Relaxed),
                SYSCALL_PHASE[cpu].load(Ordering::Relaxed),
                SCHEDULER_STAGE[cpu].load(Ordering::Relaxed))?;
            let work = KERNEL_WORK[cpu].load(Ordering::Relaxed);
            writeln!(f, "  work={} progress={:#x}",
                kernel_work_name(work >> 24), work & 0x00ff_ffff)?;
        }
        if let Some(_guard) = crate::multitasking::task::SMP_KERNEL_LOCK.try_lock() {
            unsafe { crate::multitasking::task::TASK_MANAGER.fmt_debug_tasks(f)?; }
        } else {
            writeln!(f, "tasks: unavailable (kernel lock held)")?;
        }
        Ok(())
    }
}
