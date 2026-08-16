//TASK MANAGER

use alloc::vec::Vec;
use core::arch::asm;
use core::u32::MAX;
use crate::filesystem::file::FileDescriptorTable;
use crate::memory::paging::{PageDirectory, PDEFlags, copy_kernel_mappings, PhysAddr, VirtAddr, KERNEL_OFFSET};
use crate::{gdt, init_network_stack, print, println};
use crate::drivers::pic::wait;

pub const STACK_SIZE: usize = 32 * 1024;
pub const HEADROOM: usize = 16 * 1024;
const MAX_TASKS: i8 = 8;

/// Отслеживает сколько выделений памяти используют каждую страницу.
/// Страница размапливается только когда счётчик достигает 0.
#[derive(Clone)]
pub struct PageRefcounts {
    entries: Vec<(u32, u32)>, // (page_addr, refcount)
}

impl PageRefcounts {
    pub const fn new() -> Self {
        PageRefcounts { entries: Vec::new() }
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

//each task has a 4KiB stack containg the cpu state in the bottom part of it
#[derive(Clone)]
pub struct Task {
    pub stack: [u8; STACK_SIZE],
    pub page_dir: PageDirectory,
    pub cpu_state_ptr: u32,
    pub running: bool,
    pub kernel_stack: u32,
    pub fd_table: FileDescriptorTable,
    pub heap_next: u32,
    pub page_refcounts: PageRefcounts,
    /// Parent task slot (-1 = none / kernel)
    pub parent: i8,
    /// Task has exited and waits to be reaped by wait()
    pub zombie: bool,
    /// Exit status (valid when zombie == true)
    pub exit_code: i32,
    /// Pending signals bitmask (bit N-1 = signal N). See `crate::signal`.
    pub pending_signals: u32,
}


#[repr(C)]
pub struct CPUState {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub eip:    u32,
    pub cs:     u32,
    pub eflags: u32,
    pub esp:    u32,
    pub ss:     u32,
}

impl Task {
    pub fn sleep(&mut self) {
        self.running = false;
    }

    pub fn wake(&mut self) {
        self.running = true;
    }

    pub fn new_idle() -> Self {
        Task {
            stack: [0; STACK_SIZE],
            page_dir: PageDirectory::new(),
            cpu_state_ptr: 0,
            running: false,
            fd_table: FileDescriptorTable::new(),
            kernel_stack: 0,
            heap_next: 0,
            page_refcounts: PageRefcounts::new(),
            parent: -1,
            zombie: false,
            exit_code: 0,
            pending_signals: 0,
        }
    }

    pub fn new_task() -> Self {
        Task {
            stack: [0; STACK_SIZE],
            page_dir: PageDirectory::new(),
            cpu_state_ptr: 0,
            running: false,
            fd_table: FileDescriptorTable::new(),
            kernel_stack: 0,
            heap_next: 0,
            page_refcounts: PageRefcounts::new(),
            parent: -1,
            zombie: false,
            exit_code: 0,
            pending_signals: 0,
        }
    }
    // ====================== Task::init ======================
    pub fn init(&mut self, entry_point: u32, user_stack_top: u32, heap_start: u32) {
        self.running = true;

        let kernel_stack_top = unsafe {
            (&self.stack as *const u8).add(STACK_SIZE) as u32
        };
        self.kernel_stack = kernel_stack_top;

        let state_ptr = (kernel_stack_top as usize - HEADROOM - core::mem::size_of::<CPUState>()) as *mut CPUState;
        self.cpu_state_ptr = state_ptr as u32;

        unsafe {
            let state = &mut *state_ptr;
            *state = CPUState {
                eax: 0, ebx: 0, ecx: 0, edx: 0,
                esi: 0, edi: 0, ebp: 0,
                eip:    entry_point,
                cs:     0x1B,
                eflags: 0x202,
                esp:    user_stack_top,
                ss:     0x23,
            };
        }

        // let recursive_pde = self.page_dir.entries[1023];
        // if (recursive_pde & PDEFlags::PRESENT) == 0 {
        //     copy_kernel_mappings(&mut self.page_dir);
        // }

        self.fd_table = FileDescriptorTable::new();
        self.heap_next = heap_start;

        println!("[TASK::init] Task ready | entry={:#x} | user_stack={:#x} | kernel_stack={:#x} | cpu_state={:#x} | page_dir={:#x}",
                 entry_point, user_stack_top, self.kernel_stack, self.cpu_state_ptr,
                 &self.page_dir as *const PageDirectory as u32);
    }

}

pub struct TaskManager {
    pub(crate) tasks: [Option<Task>; MAX_TASKS as usize],
    pub(crate) task_count: i8,
    pub(crate) current_task: i8,
    first_switch: bool,
}

pub static mut TASK_MANAGER: TaskManager = TaskManager {
    tasks: init_tasks_array(),
    task_count: 0,
    current_task: -1,
    first_switch: true,
};

const fn init_tasks_array() -> [Option<Task>; MAX_TASKS as usize] {
    [const { None }; MAX_TASKS as usize]
}

impl TaskManager {
    pub fn init(&mut self) {
        self.tasks[0] = Some(Task::new_idle());
        let task = self.tasks[0].as_mut().unwrap();

        // === НОВОЕ: инициализируем PageDirectory idle-задачи ===
        let pd_virt = &task.page_dir as *const _ as u32;
        let pd_phys = if pd_virt >= KERNEL_OFFSET {
            pd_virt - KERNEL_OFFSET
        } else {
            pd_virt
        };
        copy_kernel_mappings(&mut task.page_dir, pd_phys);
        let stack_top = unsafe {
            (&task.stack as *const u8).add(STACK_SIZE) as u32
        };

        let state_ptr = (stack_top as usize - HEADROOM - core::mem::size_of::<CPUState>()) as *mut CPUState;

        unsafe {
            let state = &mut *state_ptr;
            *state = CPUState {
                eax: 0, ebx: 0, ecx: 0, edx: 0,
                esi: 0, edi: 0, ebp: 0,
                eip: idle as u32,
                cs:     0x08,
                eflags: 0x202,
                esp:    stack_top,
                ss:     0x10,
            };
            task.cpu_state_ptr = state_ptr as u32;
            task.kernel_stack = stack_top;
            task.running = true;
        }

        unsafe {
            gdt::TSS.esp0 = task.kernel_stack;
            gdt::TSS.ss0  = 0x10;
        }

        self.task_count = 1;
        self.current_task = 0;
        self.first_switch = true;

        println!("[TASK] Idle task initialized | kernel_stack={:#x} | cpu_state={:#x} | pd_phys={:#x}",
                 task.kernel_stack, task.cpu_state_ptr, pd_phys);
    }

    //add given task to next slot
    pub fn add_task(&mut self, entry_point: u32, user_stack_top: u32, heap_start: u32) {
        let free_slot = self.get_free_slot();
        if free_slot < 0 {
            println!("[TASK] No free slot!");
            return;
        }
        let mut task = Task::new_task();
        task.init(entry_point, user_stack_top, heap_start);
        self.tasks[free_slot as usize] = Some(task);
        self.task_count += 1;
    }

    //remove task
    pub fn remove_task(&mut self, id: usize) {
        if id != 0 {
            if self.tasks[id].is_some() {
                self.tasks[id] = None;
                self.task_count -= 1;
            }
        }
    }

    pub fn remove_current_task(&mut self) {
        self.remove_task(self.current_task as usize);
    }

    //CPU SCHEDULER LOGIC
    pub fn schedule(&mut self, cpu_state: *mut CPUState) -> *mut CPUState {
        if  self.tasks[0].is_none() {
            return cpu_state
        }
        if self.first_switch {
            self.first_switch = false;

            let task = unsafe { self.tasks[0].as_ref().unwrap() };
            let new_cpustate = task.cpu_state_ptr as *mut CPUState;
            unsafe {
                gdt::TSS.esp0 = task.kernel_stack;
                task.page_dir.switch();        // ← правильно: &self, без копирования
            }
            return new_cpustate;
        }

        // Сохраняем состояние текущей задачи
        if self.current_task >= 0 {
            if let Some(ref mut task) = self.tasks[self.current_task as usize] {
                task.cpu_state_ptr = cpu_state as u32;
            }
        }

        // Выбираем следующую задачу
        self.current_task = self.get_next_task();

        if self.current_task < 0
            || self.tasks[self.current_task as usize].is_none()
            || !self.tasks[self.current_task as usize].as_ref().unwrap().running
        {
            self.current_task = 0;
        }

        let task = unsafe { self.tasks[self.current_task as usize].as_ref().unwrap() };
        // println!("[SCHEDULE] switching to task {} | pd_phys={:#x} | eip={:#x}",
        //          self.current_task, &task.page_dir as *const _ as u32, task.cpu_state_ptr);
        let new_cpustate = task.cpu_state_ptr as *mut CPUState;
        // внутри schedule, перед task.page_dir.switch()

        unsafe {
            gdt::TSS.esp0 = task.kernel_stack;
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
            task.page_dir.switch();            // ← главное исправление
        }

        new_cpustate
    }

    pub fn get_next_task(&self) -> i8 {
        if self.task_count <= 0 {
            return 0;
        }

        let mut i = (self.current_task + 1) % MAX_TASKS;
        for _ in 0..MAX_TASKS {
            if let Some(ref task) = self.tasks[i as usize] {
                if task.running {
                    return i;
                }
            }
            i = (i + 1) % MAX_TASKS;
        }
        0
    }

    pub fn get_free_slot(&self) -> i8 {
        for i in 0..MAX_TASKS {
            if self.tasks[i as usize].is_none() {
                return i as i8;
            }
        }
        -1
    }

    pub fn get_current_slot(&self) -> i8 {
        self.current_task
    }

    /// Find a zombie child of `parent` matching `want_pid` (-1 = any).
    /// Returns (slot, exit_code) without removing the task.
    pub fn find_zombie_child(&self, parent: i8, want_pid: i32) -> Option<(usize, i32)> {
        for i in 0..MAX_TASKS as usize {
            if let Some(ref t) = self.tasks[i] {
                if t.zombie && t.parent == parent {
                    if want_pid < 0 || want_pid == i as i32 {
                        return Some((i, t.exit_code));
                    }
                }
            }
        }
        None
    }

    /// Reap (free) a zombie task slot. Returns true on success.
    pub fn reap(&mut self, id: usize) -> bool {
        if id == 0 {
            return false;
        }
        if let Some(ref t) = self.tasks[id] {
            if !t.zombie {
                return false;
            }
        } else {
            return false;
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
                    println!("ID: {} | page_dir={:#x}", i, &task.page_dir as *const PageDirectory as u32);
                }
            }
        }
    }
}

fn idle() {
    let mut a = 0;
    loop {
        a += 1;
        for _ in 0..1000000000 {

        }
        if a % 10000000 == 0 {
            a += 1;
        }
        unsafe { asm!("hlt"); }
    }
}

