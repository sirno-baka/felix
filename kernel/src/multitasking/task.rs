//TASK MANAGER

use crate::drivers::pic::wait;
use crate::filesystem::file::FileDescriptorTable;
use crate::memory::paging::{
    alloc_kernel_stack, alloc_task_page_dir, copy_kernel_mappings, PDEFlags, PageDirectory,
    PhysAddr, VirtAddr, KERNEL_OFFSET,
};
use crate::{gdt, print, println};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::arch::asm;
use core::sync::atomic::{AtomicI8, AtomicU32, AtomicU8, Ordering};
use core::u32::MAX;

// Rust networking, TLS and thread startup can nest several sizeable kernel
// frames in one syscall. 64 KiB was demonstrably insufficient: the stack
// reached stack_base+8 and the following push faulted at stack_base-4,
// corrupting the page directory allocated immediately below it.
pub const STACK_SIZE: usize = 256 * 1024;
/// Space above the saved CPUState for the hardware interrupt frame (~few dozen bytes).
pub const HEADROOM: usize = 256;
/// Fixed scheduler slots. PID is deliberately independent from this index.
/// 32 keeps the scheduler simple on i386 while removing the old 8-process limit.
pub const MAX_TASKS: i8 = 32;
pub const USER_HEAP_BASE: u32 = 0x4000_0000;
/// Per-thread userspace stacks live below the main process stack.  A slot gets
/// a fixed 256 KiB arena: 128 KiB mapped stack followed by an unmapped guard.
pub const USER_THREAD_STACK_BASE: u32 = 0xB000_0000;
pub const USER_THREAD_STACK_STRIDE: u32 = 256 * 1024;
pub const USER_THREAD_STACK_PAGES: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Generic,
    Input,
    Timer,
    Child,
    Thread,
    Poll,
    Socket,
    Audio,
    Futex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    New,
    Runnable,
    Running(u8),
    Blocked(WaitReason),
    Stopped,
    Zombie,
}

/// Отслеживает сколько выделений памяти используют каждую страницу.
/// Страница размапливается только когда счётчик достигает 0.
#[derive(Clone)]
pub struct PageRefcounts {
    entries: Vec<(u32, u32)>, // (page_addr, refcount)
}

impl PageRefcounts {
    pub const fn new() -> Self {
        PageRefcounts {
            entries: Vec::new(),
        }
    }

    /// Увеличивает счётчик для страницы. Возвращает true если страница
    /// была новой (нужно смапить).
    pub fn inc(&mut self, page_addr: u32) -> bool {
        for (addr, count) in &mut self.entries {
            if *addr == page_addr {
                *count += 1;
                return false;
            }
        }
        self.entries.push((page_addr, 1));
        true
    }

    /// Уменьшает счётчик для страницы. Возвращает true если страница
    /// больше не используется (можно размапить).
    pub fn dec(&mut self, page_addr: u32) -> bool {
        for i in 0..self.entries.len() {
            if self.entries[i].0 == page_addr {
                self.entries[i].1 -= 1;
                if self.entries[i].1 == 0 {
                    self.entries.swap_remove(i);
                    return true;
                }
                return false;
            }
        }
        false
    }
}

// Stack and PD live in allocated frames — NOT inline.
// An inline 32KiB stack + 4KiB PD made Task ~37KiB; sys_execve from userspace
// put that on the current task's 32KiB kernel stack and smashed WM BSS.
pub struct Task {
    pub stack_base: u32,              // virt, STACK_SIZE bytes
    pub page_dir: *mut PageDirectory, // virt via phys_to_virt
    pub page_dir_phys: u32,           // goes into CR3
    pub cpu_state_ptr: u32,
    pub running: bool,
    pub state: TaskState,
    pub kernel_stack: u32,
    pub fd_table: FileDescriptorTable,
    pub heap_next: u32,
    /// Next free VA for anonymous mmap (grows up).
    pub mmap_next: u32,
    /// Page-aligned holes returned by munmap inside the anonymous mmap arena.
    /// Reusing them keeps long-running allocation-heavy processes from
    /// exhausting virtual address space even when physical frames are recycled.
    pub mmap_free: Vec<(u32, u32)>,
    pub page_refcounts: PageRefcounts,
    /// Stable process identity. Scheduler slot is an implementation detail.
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
    /// Internal scheduler parent slot, retained only for fd inheritance/debugging.
    pub parent: i8,
    /// Per-process working directory inherited across spawn.
    pub cwd: String,
    /// Logical controlling TTY id (session/foreground process-group state).
    pub tty_id: i16,
    /// Concrete PTY slave backing this process' controlling terminal. This is
    /// kept separately so /dev/tty still works after stdio descriptors change.
    pub pty_id: i16,
    pub zombie: bool,
    pub stopped: bool,
    /// Signal that caused the most recent job-control stop.
    pub stop_signal: u32,
    /// Child-state transitions are latched until waitpid consumes them.
    pub wait_stopped_pending: bool,
    pub wait_continued_pending: bool,
    /// 0 for normal exit, otherwise the terminating signal number. Kept
    /// separately so waitpid can construct a real Unix wait status.
    pub term_signal: u32,
    pub exit_code: i32,
    /// Short process name used by ps/task_list. NUL padded UTF-8/ASCII.
    pub name: [u8; 32],
    pub pending_signals: u32,
    /// Per-signal handlers: 0=SIG_DFL, 1=SIG_IGN, else userspace addr.
    pub signal_handlers: [u32; 32],
    /// Linux-like thread id.  For a process leader `tid == pid`.
    pub tid: i32,
    /// Scheduler slot containing the process leader and shared process state.
    pub leader_slot: i8,
    /// Non-leaders borrow the leader's address space and only own their kernel
    /// stack.  Process resources are released exactly once by the leader.
    pub is_thread: bool,
    /// A joined thread is reaped independently from the process leader.
    pub thread_exited: bool,
    pub thread_exit_value: u32,
    /// Detached threads are reclaimed after switching away from their kernel stack.
    pub thread_detached: bool,
    /// Userspace-owned pointer used by the standard library's TLS key table.
    pub tls_ptr: u32,
    pub user_stack_bottom: u32,
    pub user_stack_top: u32,
    pub wake_deadline_ms: u64,
    pub connect_deadline_ms: u64,
    /// Kernel wait key for FUTEX_WAIT. Shared futexes use the physical word
    /// address; FUTEX_PRIVATE_FLAG uses a process-scoped virtual key.
    pub futex_key: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CPUState {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub eip: u32,
    pub cs: u32,
    pub eflags: u32,
    pub esp: u32,
    pub ss: u32,
}

impl Task {
    /// Called only after the task can no longer run, from another task's stack.
    fn release_memory(&mut self) {
        use crate::memory::paging::{phys_to_virt, PAGING};
        use crate::smp::{trace_kernel_work, KernelWork};
        interrupt_sync::without_interrupts(|| unsafe {
            if self.is_thread {
                let mut addr = self.user_stack_bottom;
                while addr < self.user_stack_top {
                    trace_kernel_work(KernelWork::ReapUserStack, addr >> 12);
                    if let Some(phys) = (*self.page_dir).translate(addr) {
                        (*self.page_dir).unmap(addr);
                        if crate::smp::shootdown_tlb(self.page_dir_phys, addr) {
                            crate::memory::shared::release_user_frame(phys >> 12);
                        }
                    }
                    addr += 4096;
                }
                for offset in (0..STACK_SIZE).step_by(4096) {
                    trace_kernel_work(KernelWork::ReapKernelStack, offset as u32 >> 12);
                    PAGING
                        .lock()
                        .free_phys_frame((self.stack_base - KERNEL_OFFSET + offset as u32) >> 12);
                }
                return;
            }

            for index in 0..768 {
                trace_kernel_work(KernelWork::ReapUserPages, index as u32);
                let entry = (*self.page_dir).entries[index];
                // Identity large pages are borrowed kernel mappings.
                if entry & PDEFlags::PRESENT == 0 || entry & PDEFlags::DIR_PAGE_SIZE != 0 {
                    continue;
                }
                if entry & PDEFlags::USER == 0 {
                    continue;
                }
                let table_phys = entry & 0xffff_f000;
                let table = phys_to_virt(table_phys) as *const u32;
                for slot in 0..1024 {
                    let page = *table.add(slot);
                    if page & 1 != 0 {
                        crate::memory::shared::release_user_frame((page & 0xffff_f000) >> 12);
                    }
                }
                PAGING.lock().free_phys_frame(table_phys >> 12);
                (*self.page_dir).entries[index] = 0;
            }
            trace_kernel_work(KernelWork::ReapPageDir, self.page_dir_phys >> 12);
            PAGING.lock().free_phys_frame(self.page_dir_phys >> 12);
            for offset in (0..STACK_SIZE).step_by(4096) {
                trace_kernel_work(KernelWork::ReapKernelStack, offset as u32 >> 12);
                PAGING
                    .lock()
                    .free_phys_frame((self.stack_base - KERNEL_OFFSET + offset as u32) >> 12);
            }
        });
    }

    pub fn pd(&self) -> &PageDirectory {
        unsafe { &*self.page_dir }
    }

    pub fn pd_mut(&mut self) -> &mut PageDirectory {
        unsafe { &mut *self.page_dir }
    }

    pub unsafe fn switch_address_space(&self) {
        asm!("mov cr3, {}", in(reg) self.page_dir_phys);
    }

    pub fn new() -> Self {
        use crate::smp::{trace_kernel_work, KernelWork};
        trace_kernel_work(KernelWork::TaskPageDir, 0);
        let (page_dir, page_dir_phys) = alloc_task_page_dir();
        trace_kernel_work(KernelWork::TaskStack, 0);
        let stack_base = alloc_kernel_stack(STACK_SIZE);
        let stack_phys = stack_base - KERNEL_OFFSET;
        assert!(
            page_dir_phys < stack_phys || page_dir_phys >= stack_phys + STACK_SIZE as u32,
            "page directory overlaps kernel stack"
        );
        trace_kernel_work(KernelWork::TaskMetadata, 0);
        Task {
            stack_base,
            page_dir,
            page_dir_phys,
            cpu_state_ptr: 0,
            running: false,
            state: TaskState::New,
            fd_table: FileDescriptorTable::new(),
            kernel_stack: 0,
            heap_next: 0,
            mmap_next: 0x6000_0000,
            mmap_free: Vec::new(),
            page_refcounts: PageRefcounts::new(),
            pid: -1,
            ppid: -1,
            pgid: -1,
            sid: -1,
            parent: -1,
            cwd: "/".to_string(),
            tty_id: -1,
            pty_id: -1,
            zombie: false,
            stopped: false,
            stop_signal: 0,
            wait_stopped_pending: false,
            wait_continued_pending: false,
            term_signal: 0,
            exit_code: 0,
            name: [0; 32],
            pending_signals: 0,
            signal_handlers: [0; 32],
            tid: -1,
            leader_slot: -1,
            is_thread: false,
            thread_exited: false,
            thread_exit_value: 0,
            thread_detached: false,
            tls_ptr: 0,
            user_stack_bottom: 0,
            user_stack_top: 0,
            wake_deadline_ms: 0,
            connect_deadline_ms: 0,
            futex_key: 0,
        }
    }

    /// Construct a schedulable context borrowing an existing process address
    /// space.  It deliberately does not allocate a second page directory.
    pub fn new_thread(page_dir: *mut PageDirectory, page_dir_phys: u32) -> Self {
        let stack_base = alloc_kernel_stack(STACK_SIZE);
        let stack_phys = stack_base - KERNEL_OFFSET;
        assert!(
            page_dir_phys < stack_phys || page_dir_phys >= stack_phys + STACK_SIZE as u32,
            "shared page directory overlaps thread kernel stack"
        );
        Task {
            stack_base,
            page_dir,
            page_dir_phys,
            cpu_state_ptr: 0,
            running: false,
            state: TaskState::New,
            fd_table: FileDescriptorTable::new(),
            kernel_stack: 0,
            heap_next: 0,
            mmap_next: 0,
            mmap_free: Vec::new(),
            page_refcounts: PageRefcounts::new(),
            pid: -1,
            ppid: -1,
            pgid: -1,
            sid: -1,
            parent: -1,
            cwd: "/".to_string(),
            tty_id: -1,
            pty_id: -1,
            zombie: false,
            stopped: false,
            stop_signal: 0,
            wait_stopped_pending: false,
            wait_continued_pending: false,
            term_signal: 0,
            exit_code: 0,
            name: [0; 32],
            pending_signals: 0,
            signal_handlers: [0; 32],
            tid: -1,
            leader_slot: -1,
            is_thread: true,
            thread_exited: false,
            thread_exit_value: 0,
            thread_detached: false,
            tls_ptr: 0,
            user_stack_bottom: 0,
            user_stack_top: 0,
            wake_deadline_ms: 0,
            connect_deadline_ms: 0,
            futex_key: 0,
        }
    }

    pub fn new_idle() -> Self {
        Self::new()
    }
    pub fn new_task() -> Self {
        Self::new()
    }

    pub fn sleep(&mut self) {
        self.running = false;
        self.state = TaskState::Blocked(WaitReason::Generic);
    }
    pub fn wake(&mut self) {
        self.running = true;
        self.state = TaskState::Runnable;
    }
    pub fn block(&mut self, reason: WaitReason) {
        self.running = false;
        self.state = TaskState::Blocked(reason);
    }
    pub fn stop(&mut self) {
        self.running = false;
        self.state = TaskState::Stopped;
    }
    pub fn mark_zombie(&mut self) {
        self.running = false;
        self.state = TaskState::Zombie;
    }

    pub fn init(&mut self, entry_point: u32, user_stack_top: u32, heap_start: u32) {
        self.running = true;
        self.state = TaskState::Runnable;

        let kernel_stack_top = self.stack_base + STACK_SIZE as u32;
        self.kernel_stack = kernel_stack_top;

        let state_ptr = (kernel_stack_top as usize - HEADROOM - core::mem::size_of::<CPUState>())
            as *mut CPUState;
        self.cpu_state_ptr = state_ptr as u32;

        unsafe {
            *state_ptr = CPUState {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
                esi: 0,
                edi: 0,
                ebp: 0,
                eip: entry_point,
                cs: 0x1B,
                eflags: 0x202,
                esp: user_stack_top,
                ss: 0x23,
            };
        }

        self.fd_table = FileDescriptorTable::new();
        self.heap_next = heap_start;
    }
}

pub struct TaskManager {
    pub(crate) tasks: [Option<Task>; MAX_TASKS as usize],
    pub(crate) task_count: i8,
    pub(crate) current_task: i8,
    first_switch: bool,
    next_pid: i32,
}

pub static mut TASK_MANAGER: TaskManager = TaskManager {
    tasks: init_tasks_array(),
    task_count: 0,
    current_task: -1,
    first_switch: true,
    next_pid: 1,
};

pub static SMP_KERNEL_LOCK: interrupt_sync::SpinMutex<()> =
    interrupt_sync::SpinMutex::new(());

pub static KERNEL_LOCK_OWNER: AtomicI8 = AtomicI8::new(-1);
pub static KERNEL_LOCK_CONTEXT: AtomicU8 = AtomicU8::new(0);

pub const KERNEL_LOCK_CTX_NONE: u8 = 0;
pub const KERNEL_LOCK_CTX_SYSCALL: u8 = 1;
pub const KERNEL_LOCK_CTX_BSP_TIMER: u8 = 2;
pub const KERNEL_LOCK_CTX_AP_TIMER: u8 = 3;
pub const KERNEL_LOCK_CTX_EXCEPTION: u8 = 4;

/// Debug/allocator invariant: a physical frame handed out for a new mapping
/// must never still belong to the kernel stack of a live task.
pub fn live_kernel_stack_owner(frame: u32) -> Option<usize> {
    unsafe {
        for (slot, task) in TASK_MANAGER.tasks.iter().enumerate() {
            let Some(task) = task.as_ref() else { continue };
            if task.stack_base < KERNEL_OFFSET {
                continue;
            }
            let first = (task.stack_base - KERNEL_OFFSET) >> 12;
            let pages = (STACK_SIZE / 4096) as u32;
            if frame >= first && frame < first.saturating_add(pages) {
                return Some(slot);
            }
        }
    }
    None
}

pub fn try_lock_kernel() -> Option<interrupt_sync::SpinMutexGuard<'static, ()>> {
    let guard = SMP_KERNEL_LOCK.try_lock()?;
    KERNEL_LOCK_OWNER.store(crate::smp::current_cpu_index() as i8, Ordering::Relaxed);
    Some(guard)
}

#[inline]
pub fn set_kernel_lock_context(context: u8) {
    KERNEL_LOCK_CONTEXT.store(context, Ordering::Relaxed);
}

pub fn lock_kernel() -> interrupt_sync::SpinMutexGuard<'static, ()> {
    loop {
        if let Some(guard) = try_lock_kernel() {
            return guard;
        }
        // Lock release does not generate an interrupt. Sleeping with HLT here
        // can strand a CPU forever when its Local APIC timer is slow or missed
        // on real hardware. Keep IPIs serviceable and actively recheck.
        unsafe { asm!("sti", "pause", "cli", options(nomem, nostack)) };
    }
}
// The assembly epilogue owns this guard until it has left the outgoing stack.
// Releasing it in Rust would let another CPU resume/reap that stack while the
// old CPU is still returning through it. EDX:EAX carries (locked, next ESP).
pub fn handoff_kernel_lock(
    guard: interrupt_sync::SpinMutexGuard<'static, ()>,
    next_esp: u32,
) -> u64 {
    // Ring 3 must always run with maskable interrupts enabled. A corrupted or
    // accidentally reused CPUState with IF clear makes that CPU stop receiving
    // its Local APIC timer forever, leaving TASK_OWNER pinned to a dead CPU.
    // Enforce the architectural userspace invariant at the single handoff
    // point shared by syscalls, timers, and user-fault recovery.
    if next_esp != 0 {
        unsafe {
            let state = &mut *(next_esp as *mut CPUState);
            if state.cs & 3 == 3 {
                state.eflags |= 0x0000_0202;
            }
        }
    }
    core::mem::forget(guard);
    (1u64 << 32) | next_esp as u64
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn finish_kernel_handoff() {
    // Called only after MOV ESP from an epilogue whose EDX flag is nonzero.
    crate::smp::trace_kernel_return();
    KERNEL_LOCK_CONTEXT.store(KERNEL_LOCK_CTX_NONE, Ordering::Relaxed);
    KERNEL_LOCK_OWNER.store(-1, Ordering::Relaxed);
    SMP_KERNEL_LOCK.force_unlock();
}

/// Kernel continuations cannot migrate: they may hold CPU-local state or be
/// waiting to acquire the giant lock. Only userspace and CPU idle may switch.
pub fn timer_may_schedule(state: *const CPUState) -> bool {
    unsafe { (*state).cs & 3 == 3 || crate::smp::current_task_slot() <= 0 }
}

static TASK_OWNER: [AtomicI8; MAX_TASKS as usize] =
    [const { AtomicI8::new(-1) }; MAX_TASKS as usize];
const fn init_tasks_array() -> [Option<Task>; MAX_TASKS as usize] {
    [const { None }; MAX_TASKS as usize]
}

impl TaskManager {
    pub fn init(&mut self) {
        self.tasks[0] = Some(Task::new_idle());
        let task = self.tasks[0].as_mut().unwrap();
        let pd_phys = task.page_dir_phys;
        copy_kernel_mappings(task.pd_mut(), pd_phys);

        let stack_top = task.stack_base + STACK_SIZE as u32;
        let state_ptr =
            (stack_top as usize - HEADROOM - core::mem::size_of::<CPUState>()) as *mut CPUState;

        unsafe {
            *state_ptr = CPUState {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
                esi: 0,
                edi: 0,
                ebp: 0,
                eip: idle as u32,
                cs: 0x08,
                eflags: 0x0000_0202, // IF=1, reserved1=1
                esp: stack_top,
                ss: 0x10,
            };
            task.cpu_state_ptr = state_ptr as u32;
            task.kernel_stack = stack_top;
            task.running = true;
            task.state = TaskState::Running(0);
            task.pid = 0;
            task.tid = 0;
            task.leader_slot = 0;
            task.ppid = -1;
            task.pgid = 0;
            task.sid = 0;
            task.parent = -1;
            task.cwd = "/".to_string();
            task.tty_id = -1;
            task.pty_id = -1;
            task.stopped = false;
            task.stop_signal = 0;
            task.wait_stopped_pending = false;
            task.wait_continued_pending = false;
            task.term_signal = 0;
            task.name[..4].copy_from_slice(b"idle");

            gdt::TSS.esp0 = task.kernel_stack;
            gdt::TSS.ss0 = 0x10;
        }

        self.task_count = 1;
        self.current_task = 0;
        TASK_OWNER[0].store(0, Ordering::Release);
        crate::smp::set_current_task_slot(0);
        self.first_switch = true;
        self.next_pid = 1;

        println!(
            "[TASK] Idle | kstack={:#x} cpu_state={:#x} pd_phys={:#x} Task={}",
            task.kernel_stack,
            task.cpu_state_ptr,
            task.page_dir_phys,
            core::mem::size_of::<Task>(),
        );
    }

    //add given task to next slot
    pub fn add_task(&mut self, entry_point: u32, user_stack_top: u32, heap_start: u32) {
        let free_slot = self.get_free_slot();
        if free_slot < 0 {
            println!("[TASK] No free slot!");
            return;
        }
        let pid = self.alloc_pid();
        let mut task = Task::new_task();
        task.init(entry_point, user_stack_top, heap_start);
        task.pid = pid;
        task.tid = pid;
        task.leader_slot = free_slot;
        task.ppid = 0;
        task.pgid = pid;
        task.sid = pid;
        task.parent = 0;
        task.cwd = "/".to_string();
        task.tty_id = -1;
        self.tasks[free_slot as usize] = Some(task);
        self.task_count += 1;
    }

    pub(crate) fn reparent_children_of(&mut self, dead_pid: i32) {
        if dead_pid <= 0 {
            return;
        }
        let init_slot = if dead_pid != 1 {
            self.slot_by_pid(1)
                .filter(|slot| self.tasks[*slot].as_ref().map_or(false, |t| !t.zombie))
        } else {
            None
        };
        for task in self.tasks.iter_mut().flatten() {
            if task.ppid != dead_pid {
                continue;
            }
            if let Some(slot) = init_slot {
                task.ppid = 1;
                task.parent = slot as i8;
            } else {
                task.ppid = 0;
                task.parent = 0;
            }
        }
    }

    //remove task
    pub fn remove_task(&mut self, id: usize) {
        if id != 0
            && id < self.tasks.len()
            && TASK_OWNER[id].load(Ordering::Acquire) < 0
        {
            if let Some(pid) = self.tasks[id].as_ref().map(|t| t.pid) {
                self.reparent_children_of(pid);
                self.tasks[id] = None;
                self.task_count -= 1;
            }
        }
    }

    pub fn remove_current_task(&mut self) {
        let current = crate::smp::current_task_slot();
        if current >= 0 {
            self.remove_task(current as usize);
        }
    }

    //CPU SCHEDULER LOGIC
    pub fn schedule(&mut self, cpu_state: *mut CPUState) -> *mut CPUState {
        crate::smp::trace_scheduler(2);
        self.wake_expired_timers(crate::time::uptime_ms());
        crate::smp::trace_scheduler(3);
        let cpu = crate::smp::current_cpu_index();
        if cpu != 0 {
            return self.schedule_ap(cpu, cpu_state);
        }
        if self.tasks[0].is_none() {
            return cpu_state;
        }
        if self.first_switch {
            self.first_switch = false;
            // Kernel bootstrap is not a task. Jump into a constructed CPUState.
            // Prefer pid>=1 if execve already spawned the shell — otherwise the
            // first tick always entered idle and pid=1 waited for IRQ0 #2.
            // On real PIC/APIC that second tick is often missing.
            self.current_task = 0;
            let next = self.claim_next_task(0, 0);
            if next != 0 {
                self.current_task = next;
            }
            crate::smp::set_current_task_slot(self.current_task);
            // PIC policy belongs to boot/device code, not the scheduler. In
            // particular, do not overwrite main.rs masks here: IRQ9 must remain
            // masked while OHCI/ToPIC share the legacy line and use polling.
            let task = unsafe { self.tasks[self.current_task as usize].as_ref().unwrap() };
            let new_cpustate = task.cpu_state_ptr as *mut CPUState;
            println!("[TASK] first switch -> {}", self.current_task);

            unsafe {
                gdt::TSS.esp0 = task.kernel_stack;
                crate::smp::set_current_cr3(task.page_dir_phys);
                task.switch_address_space();
            }
            if let Some(task) = self.tasks[self.current_task as usize].as_mut() {
                task.state = TaskState::Running(0);
            }
            return new_cpustate;
        }

        // Сохраняем состояние текущей задачи
        if self.current_task >= 0 {
            if let Some(ref mut task) = self.tasks[self.current_task as usize] {
                task.cpu_state_ptr = cpu_state as u32;
                if task.running {
                    task.state = TaskState::Runnable;
                }
            }
            if self.current_task > 0 {
                TASK_OWNER[self.current_task as usize].store(-1, Ordering::Release);
                self.wake_waiters(WaitReason::Child);
                self.wake_waiters(WaitReason::Thread);
            }
        }

        // Выбираем следующую задачу
        self.current_task = self.claim_next_task(0, self.current_task);

        if self.current_task < 0
            || self.tasks[self.current_task as usize].is_none()
            || !self.tasks[self.current_task as usize]
                .as_ref()
                .unwrap()
                .running
        {
            self.current_task = 0;
        }
        crate::smp::set_current_task_slot(self.current_task);
        if let Some(task) = self.tasks[self.current_task as usize].as_mut() {
            task.state = TaskState::Running(0);
        }

        let task = unsafe { self.tasks[self.current_task as usize].as_ref().unwrap() };
        // println!("[SCHEDULE] switching to task {} | pd_phys={:#x} | eip={:#x}",
        //          self.current_task, &task.page_dir as *const _ as u32, task.cpu_state_ptr);
        let new_cpustate = task.cpu_state_ptr as *mut CPUState;
        // внутри schedule, перед task.page_dir.switch()

        unsafe {
            gdt::TSS.esp0 = task.kernel_stack;
            crate::smp::set_current_cr3(task.page_dir_phys);
            // --- ELF @ 0x400000 ---
            // let virt = 0x0040_4000u32;
            // let page_num = virt >> 12;
            // let pd_idx = (page_num >> 10) as usize;      // 1
            // let pt_idx = (page_num & 0x3FF) as usize;    // 4
            //
            // let pde = task.page_dir.entries[pd_idx];
            // println!("[SCHED] PDE[{}]={:#x}", pd_idx, pde);
            //
            // if (pde & 1) != 0 {
            //     let pt_phys = pde & 0xFFFF_F000;
            //     let pt = crate::memory::paging::phys_to_virt(pt_phys) as *const [u32; 1024];
            //     let pte = unsafe { (*pt)[pt_idx] };
            //     println!("[SCHED] PTE[{}] virt={:#x} = {:#x}", pt_idx, virt, pte);
            // } else {
            //     println!("[SCHED] PDE[{}] NOT PRESENT — ELF not mapped!", pd_idx);
            // }
            //
            // // --- stack @ 0xBFFFF000 ---
            // let stack_page = 0xBFFF_F000u32;
            // let spn = stack_page >> 12;
            // let spd = (spn >> 10) as usize;              // 767
            // let spt = (spn & 0x3FF) as usize;            // 1023
            // println!("[SCHED] PDE[{}] (stack)={:#x}", spd, task.page_dir.entries[spd]);
            task.switch_address_space();
        }

        new_cpustate
    }

    fn wake_expired_timers(&mut self, now_ms: u64) {
        for task in self.tasks.iter_mut().flatten() {
            match task.state {
                TaskState::Blocked(WaitReason::Timer) if now_ms >= task.wake_deadline_ms => {
                    task.wake_deadline_ms = 0;
                    task.wake();
                }
                TaskState::Blocked(WaitReason::Poll)
                    if task.wake_deadline_ms != 0 && now_ms >= task.wake_deadline_ms =>
                {
                    task.wake()
                }
                TaskState::Blocked(WaitReason::Futex)
                    if task.wake_deadline_ms != 0 && now_ms >= task.wake_deadline_ms =>
                {
                    // FUTEX_WAIT is not a restartable syscall. Its saved
                    // userspace frame must carry -ETIMEDOUT when the timer
                    // wakes it, while FUTEX_WAKE leaves the preloaded eax=0.
                    if task.cpu_state_ptr != 0 {
                        unsafe {
                            (*(task.cpu_state_ptr as *mut CPUState)).eax = (-110i32) as u32;
                        }
                    }
                    task.futex_key = 0;
                    task.wake_deadline_ms = 0;
                    task.wake();
                }
                _ => {}
            }
        }
    }

    pub fn wake_waiters(&mut self, reason: WaitReason) {
        for task in self.tasks.iter_mut().flatten() {
            if task.state == TaskState::Blocked(reason) {
                task.wake();
            }
        }
    }

    fn schedule_ap(&mut self, cpu: usize, cpu_state: *mut CPUState) -> *mut CPUState {
        crate::smp::trace_scheduler(4);
        let previous = crate::smp::current_task_slot();
        if previous < 0 {
            crate::smp::set_idle_esp(cpu, cpu_state as u32);
        } else if let Some(task) = self.tasks[previous as usize].as_mut() {
            task.cpu_state_ptr = cpu_state as u32;
            if task.running {
                task.state = TaskState::Runnable;
            }
            TASK_OWNER[previous as usize].store(-1, Ordering::Release);
            self.wake_waiters(WaitReason::Child);
            self.wake_waiters(WaitReason::Thread);
        }

        crate::smp::trace_scheduler(5);
        let next = self.claim_next_task(cpu as i8, previous);
        crate::smp::trace_scheduler(6);
        if next <= 0 {
            crate::smp::set_current_task_slot(-1);
            // Leave the outgoing address space before it can be reclaimed.
            unsafe { asm!("mov cr3, {}", in(reg) crate::memory::paging::KERNEL_PD_PHYS); }
            crate::smp::set_current_cr3(unsafe { crate::memory::paging::KERNEL_PD_PHYS });
            let idle = crate::smp::idle_esp(cpu);
            crate::smp::trace_scheduler(7);
            return if idle != 0 { idle as *mut CPUState } else { cpu_state };
        }

        crate::smp::trace_scheduler(8);
        crate::smp::set_current_task_slot(next);
        if let Some(task) = self.tasks[next as usize].as_mut() {
            task.state = TaskState::Running(cpu as u8);
        }
        crate::smp::trace_scheduler(81);
        let task = self.tasks[next as usize].as_ref().unwrap();
        // Never print from the AP timer while it owns the giant kernel lock.
        // Legacy VGA/port-I/O output from an AP can stall on real chipsets and
        // would leave every other CPU waiting for this lock forever.
        unsafe {
            crate::smp::set_cpu_kernel_stack(cpu, task.kernel_stack);
            crate::smp::trace_scheduler(82);
            crate::smp::set_current_cr3(task.page_dir_phys);
            crate::smp::trace_scheduler(83);
            task.switch_address_space();
            crate::smp::trace_scheduler(84);
        }
        task.cpu_state_ptr as *mut CPUState
    }

    fn claim_next_task(&self, cpu: i8, after: i8) -> i8 {
        let mut slot = if after < 1 { 1 } else { (after + 1) % MAX_TASKS };
        for _ in 1..MAX_TASKS {
            if slot == 0 {
                slot = 1;
            }
            // A task owner is only valid while that CPU reports the same
            // current slot.  Losing this invariant used to leave a woken task
            // permanently unselectable: every CPU went idle although the task
            // was Runnable.  Repair the stale reservation while the giant
            // scheduler lock gives us exclusive access to task state.
            let owner = TASK_OWNER[slot as usize].load(Ordering::Acquire);
            if owner >= 0
                && crate::smp::task_slot_on_cpu(owner as usize) != slot
            {
                let _ = TASK_OWNER[slot as usize].compare_exchange(
                    owner,
                    -1,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                );
            }
            if self.tasks[slot as usize].as_ref().is_some_and(|task| {
                task.running
                    && task.state == TaskState::Runnable
                    && !task.zombie
                    && !task.thread_exited
            })
                && TASK_OWNER[slot as usize]
                    .compare_exchange(-1, cpu, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                return slot;
            }
            slot = (slot + 1) % MAX_TASKS;
        }
        0
    }

    pub fn get_next_task(&self) -> i8 {
        if self.task_count <= 0 {
            return 0;
        }

        let mut i = (self.current_task + 1) % MAX_TASKS;
        for _ in 0..MAX_TASKS {
            if let Some(ref task) = self.tasks[i as usize] {
                if task.running
                    && task.state == TaskState::Runnable
                    && TASK_OWNER[i as usize].load(Ordering::Acquire) < 0
                {
                    return i;
                }
            }
            i = (i + 1) % MAX_TASKS;
        }
        0
    }

    pub fn alloc_pid(&mut self) -> i32 {
        // PID 0 is reserved for idle/kernel. Monotonic reuse is intentionally
        // avoided until i32 wrap, which is effectively unreachable here.
        let pid = self.next_pid.max(1);
        self.next_pid = self.next_pid.wrapping_add(1);
        if self.next_pid <= 0 {
            self.next_pid = 1;
        }
        pid
    }

    pub fn slot_by_pid(&self, pid: i32) -> Option<usize> {
        self.tasks.iter().enumerate().find_map(|(slot, task)| {
            task.as_ref()
                .filter(|t| t.pid == pid && t.leader_slot == slot as i8)
                .map(|_| slot)
        })
    }

    pub fn slot_by_tid(&self, tid: i32) -> Option<usize> {
        self.tasks
            .iter()
            .enumerate()
            .find_map(|(slot, task)| task.as_ref().filter(|t| t.tid == tid).map(|_| slot))
    }

    pub fn process_slot(&self, slot: usize) -> usize {
        self.tasks
            .get(slot)
            .and_then(|t| t.as_ref())
            .map(|t| {
                if t.leader_slot >= 0 {
                    t.leader_slot as usize
                } else {
                    slot
                }
            })
            .unwrap_or(slot)
    }

    pub fn thread_slots(&self, leader_slot: usize) -> Vec<usize> {
        self.tasks
            .iter()
            .enumerate()
            .filter_map(|(slot, task)| {
                task.as_ref()
                    .filter(|t| t.leader_slot == leader_slot as i8)
                    .map(|_| slot)
            })
            .collect()
    }

    pub fn live_thread_count(&self, leader_slot: usize) -> usize {
        self.tasks
            .iter()
            .flatten()
            .filter(|t| t.leader_slot == leader_slot as i8 && !t.thread_exited)
            .count()
    }

    pub fn current_pid(&self) -> i32 {
        let current = crate::smp::current_task_slot();
        if current < 0 {
            return -1;
        }
        self.tasks[current as usize]
            .as_ref()
            .map(|t| t.pid)
            .unwrap_or(-1)
    }

    pub fn slots_in_pgid(&self, pgid: i32) -> Vec<usize> {
        self.tasks
            .iter()
            .enumerate()
            .filter_map(|(slot, task)| {
                task.as_ref()
                    .filter(|t| t.pgid == pgid && !t.zombie && t.leader_slot == slot as i8)
                    .map(|_| slot)
            })
            .collect()
    }

    pub fn get_free_slot(&mut self) -> i8 {
        self.reap_detached_threads();
        self.reap_orphans();
        for i in 0..MAX_TASKS {
            if self.tasks[i as usize].is_none() {
                return i as i8;
            }
        }
        // Do not steal a zombie that still belongs to a live parent: doing so
        // destroys waitpid semantics. PID 1 is responsible for orphan reaping.
        -1
    }

    /// Reclaim detached threads only after execution has moved to another
    /// kernel stack. The currently executing slot is deliberately skipped.
    pub fn reap_detached_threads(&mut self) {
        for slot in 1..MAX_TASKS as usize {
            if TASK_OWNER[slot].load(Ordering::Acquire) >= 0 {
                continue;
            }
            let reap = self.tasks[slot].as_ref().map_or(false, |task| {
                task.is_thread && task.thread_exited && task.thread_detached
            });
            if reap {
                self.reap_thread(slot);
            }
        }
    }

    fn parent_gone(&self, ppid: i32) -> bool {
        ppid <= 0 || self.slot_by_pid(ppid).is_none()
    }

    /// Repair any stale parent references. Normal parent death is handled by
    /// reap(), which reparents children to PID 1. This path mainly covers old
    /// tasks created before that parent was tracked or forced slot removal.
    pub fn reap_orphans(&mut self) {
        let init_slot = self
            .slot_by_pid(1)
            .filter(|slot| self.tasks[*slot].as_ref().map_or(false, |t| !t.zombie));
        let mut unreapable_zombies = Vec::new();
        for i in 1..MAX_TASKS as usize {
            let missing_parent = self.tasks[i]
                .as_ref()
                .map_or(false, |t| t.pid != 1 && self.parent_gone(t.ppid));
            if !missing_parent {
                continue;
            }
            if let Some(slot) = init_slot {
                if let Some(ref mut task) = self.tasks[i] {
                    task.ppid = 1;
                    task.parent = slot as i8;
                }
            } else if self.tasks[i].as_ref().map_or(false, |t| t.zombie) {
                unreapable_zombies.push(i);
            }
        }
        for id in unreapable_zombies {
            let _ = self.reap(id);
        }
    }

    pub fn get_current_slot(&self) -> i8 {
        crate::smp::current_task_slot()
    }

    /// Find a zombie child by stable PID. `want_pid == -1` means any child.
    /// Returns (slot, child_pid, exit_code) without removing the task.
    pub fn find_zombie_child(&self, parent_pid: i32, want_pid: i32) -> Option<(usize, i32, i32)> {
        for i in 0..MAX_TASKS as usize {
            if let Some(ref t) = self.tasks[i] {
                if t.zombie && t.ppid == parent_pid {
                    if want_pid < 0 || want_pid == t.pid {
                        return Some((i, t.pid, t.exit_code));
                    }
                }
            }
        }
        None
    }

    /// Must hold the kernel lock; an exited sibling may still be entering the kernel.
    pub fn siblings_quiescent(&self, leader: usize) -> bool {
        self.thread_slots(leader).iter().all(|&slot| slot == leader || TASK_OWNER[slot].load(Ordering::Acquire) < 0)
    }

    pub fn process_quiescent(&self, leader: usize) -> bool {
        self.thread_slots(leader).iter().all(|&slot| {
            TASK_OWNER[slot].load(Ordering::Acquire) < 0
        })
    }

    /// Reap (free) a zombie task slot. Returns true on success.
    pub fn reap(&mut self, id: usize) -> bool {
        if id == 0 || id >= self.tasks.len() || !self.process_quiescent(id) {
            return false;
        }
        let dead_pid = if let Some(ref t) = self.tasks[id] {
            if !t.zombie {
                return false;
            }
            t.pid
        } else {
            return false;
        };
        self.reparent_children_of(dead_pid);
        // A process zombie owns every unjoined thread in its group. Reclaim
        // their private stacks before releasing the shared address space.
        let members = self.thread_slots(id);
        for slot in members {
            crate::smp::trace_kernel_work(crate::smp::KernelWork::ReapThreads, slot as u32);
            if slot == id {
                continue;
            }
            if let Some(task) = self.tasks[slot].as_mut() {
                task.release_memory();
            }
            self.tasks[slot] = None;
            self.task_count -= 1;
        }
        if let Some(task) = self.tasks[id].as_mut() {
            task.release_memory();
        }
        self.tasks[id] = None;
        self.task_count -= 1;
        true
    }

    pub fn reap_thread(&mut self, id: usize) -> bool {
        if id == 0 || id >= self.tasks.len() {
            return false;
        }
        if TASK_OWNER[id].load(Ordering::Acquire) >= 0 {
            return false;
        }
        let can_reap = self.tasks[id]
            .as_ref()
            .map_or(false, |t| t.is_thread && t.thread_exited);
        if !can_reap {
            return false;
        }
        if let Some(task) = self.tasks[id].as_mut() {
            task.release_memory();
        }
        self.tasks[id] = None;
        self.task_count -= 1;
        true
    }

    pub fn list_tasks(&self) {
        println!("Running tasks:");

        for i in 0..MAX_TASKS {
            if let Some(ref task) = self.tasks[i as usize] {
                if task.running {
                    println!("ID: {} | pd_phys={:#x}", i, task.page_dir_phys);
                }
            }
        }
    }

    pub fn fmt_debug_tasks(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "tasks now_ms={}:", crate::time::uptime_ms())?;
        for (slot, task) in self.tasks.iter().enumerate() {
            let Some(task) = task.as_ref() else { continue };
            let saved = task.cpu_state_ptr as *const CPUState;
            let saved_eip = if saved.is_null() { 0 } else { unsafe { (*saved).eip } };
            writeln!(
                f,
                "  s{} pid={} tid={} run={} state={:?} owner={} deadline={} eip={:#x}",
                slot,
                task.pid,
                task.tid,
                task.running,
                task.state,
                TASK_OWNER[slot].load(Ordering::Relaxed),
                task.wake_deadline_ms,
                saved_eip,
            )?;
        }
        Ok(())
    }
}

fn idle() {
    println!("[TASK] idle");
    loop {
        unsafe {
            asm!("hlt");
        }
        // USB + PCMCIA hotplug are intentionally polled from task context.
        // OHCI transfers themselves run with interrupts temporarily disabled on
        // the Sony C1M so the PIT scheduler cannot preempt old M5237 hardware.
        crate::drivers::usb::poll_events();
        crate::drivers::pcmcia::poll_hotplug();
    }
}
