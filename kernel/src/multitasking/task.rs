//TASK MANAGER

use core::arch::asm;
use core::u32::MAX;
use crate::filesystem::file::FileDescriptorTable;
use crate::{gdt, print, println};
use crate::drivers::pic::wait;

const STACK_SIZE: usize = 32 * 1024;
const MAX_TASKS: i8 = 8;
const HEADROOM: usize = 16384;

//each task has a 4KiB stack containg the cpu state in the bottom part of it
#[derive(Copy, Debug, Clone)]
pub struct Task {
    pub stack: [u8; STACK_SIZE],
    pub cpu_state_ptr: u32, //pub cpu_state: *mut CPUState,
    pub running: bool,
    pub kernel_stack: u32,             // ← НОВОЕ: отдельный kernel stack                                                                                                                                                                                              bootloader/src/gdt.rs           +1 -1
    pub fd_table: FileDescriptorTable,
}


#[repr(C)]
pub struct CPUState {
    // Порядок точно соответствует тому, на что указывает esp из naked handler
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    // То, что CPU автоматически пушит при user→kernel
    pub eip:    u32,
    pub cs:     u32,
    pub eflags: u32,
    pub esp:    u32,   // user stack pointer
    pub ss:     u32,
}

static NULL_TASK: Task = Task {
    stack: [0; STACK_SIZE],
    cpu_state_ptr: 0 as u32, //cpu_state: 0 as *mut CPUState,
    running: false,
    fd_table: FileDescriptorTable::new(),
    kernel_stack: 0 as u32,
};

impl Task {
    pub fn sleep(&mut self) {
        self.running = false;
    }

    pub fn wake(&mut self) {
        self.running = true;
    }
    //setup task stack, zeroing its cpu state and setting entry point
        // Изменённая функция
    // ====================== Task::init ======================
    pub fn init(&mut self, entry_point: u32, user_stack_top: u32) {
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
                cs:     0x1B,                    // ← USER CODE (RPL=3)
                eflags: 0x202,
                esp:    user_stack_top,          // ← user stack
                ss:     0x23,                    // ← USER DATA (RPL=3)
            };
        }

        self.fd_table = FileDescriptorTable::new();

        println!("[TASK::init] Task ready | entry={:#x} | user_stack={:#x} | kernel_stack={:#x} | cpu_state={:#x}",
                 entry_point, user_stack_top, self.kernel_stack, self.cpu_state_ptr);
    }

}

pub struct TaskManager {
    pub(crate) tasks: [Task; MAX_TASKS as usize], //arry of tasks
    task_count: i8,                    //how many tasks are in the queue
    pub(crate) current_task: i8,                  //current running task
    first_switch: bool,          // ← НОВОЕ ПОЛЕ
}

//init null task manager
pub static mut TASK_MANAGER: TaskManager = TaskManager {
    tasks: [NULL_TASK; MAX_TASKS as usize],
    task_count: 0,
    current_task: -1,
    first_switch: true,          // ← true
};

impl TaskManager {
    pub fn init(&mut self) {
        // Создаём idle task напрямую в массиве (чтобы stack не был локальной переменной)
        self.tasks[0] = Task {
            stack: [0; STACK_SIZE],
            cpu_state_ptr: 0,
            running: false,
            fd_table: FileDescriptorTable::new(),
            kernel_stack: 0,
        };

        let stack_top = unsafe {
            (&self.tasks[0].stack as *const u8).add(STACK_SIZE) as u32
        };

        let state_ptr = (stack_top as usize - HEADROOM - core::mem::size_of::<CPUState>()) as *mut CPUState;

        unsafe {
            let state = &mut *state_ptr;
            *state = CPUState {
                eax: 0, ebx: 0, ecx: 0, edx: 0,
                esi: 0, edi: 0, ebp: 0,
                eip: idle as u32,
                cs:     0x08,           // ← kernel code
                eflags: 0x202,
                esp:    stack_top,      // kernel stack
                ss:     0x10,           // ← kernel data
            };
            self.tasks[0].cpu_state_ptr = state_ptr as u32;
            self.tasks[0].kernel_stack = stack_top;
            self.tasks[0].running = true;
        }

        // <<< КРИТИЧНО: инициализируем TSS до первого таймера >>>
        unsafe {
            gdt::TSS.esp0 = self.tasks[0].kernel_stack;
            gdt::TSS.ss0  = 0x10;           // kernel data segment
        }

        self.task_count = 1;
        self.current_task = 0;
        self.first_switch = true;
        println!("[TASK] Idle task initialized | kernel_stack={:#x} | cpu_state={:#x}",
                 self.tasks[0].kernel_stack, self.tasks[0].cpu_state_ptr);
    }

    //add given task to next slot
    pub fn add_task(&mut self, entry_point: u32, user_stack_top: u32) {
        let free_slot = self.get_free_slot();
        if free_slot < 0 {
            println!("[TASK] No free slot!");
            return;
        }
        // unsafe { asm!("cli") };
        self.tasks[free_slot as usize].init(entry_point, user_stack_top);
        self.task_count += 1;
        // unsafe { asm!("sti") };
    }

    //remove task
    pub fn remove_task(&mut self, id: usize) {
        if id != 0 {
            self.tasks[id] = NULL_TASK;
            self.task_count -= 1;
        }
    }

    pub fn remove_current_task(&mut self) {
        self.remove_task(self.current_task as usize);
    }

    //CPU SCHEDULER LOGIC
    //triggers scheduler with round robin scheduling algorithm, returns new cpu state
    pub fn schedule(&mut self, cpu_state: *mut CPUState) -> *mut CPUState {
        // println!("[SCHEDULE] ENTER: task={}, incoming_esp={:#x}, cs={:#x}, eip={:#x}",
        //          self.current_task,
        //          cpu_state as u32,
        //          unsafe { (*cpu_state).cs },
        //          unsafe { (*cpu_state).eip });

        // === СПЕЦИАЛЬНАЯ ОБРАБОТКА ПЕРВОГО ПЕРЕКЛЮЧЕНИЯ ===
        // Первый таймер приходит из kernel mode (boot stack) — не сохраняем его состояние
        // === СПЕЦИАЛЬНАЯ ОБРАБОТКА ТОЛЬКО ПЕРВОГО ПЕРЕКЛЮЧЕНИЯ ИЗ BOOT ===
        if self.first_switch {
            // println!("[SCHEDULE] FIRST SWITCH from boot → idle (skip save)");
            self.first_switch = false;

            let new_cpustate = self.tasks[0].cpu_state_ptr as *mut CPUState;
            unsafe {
                gdt::TSS.esp0 = self.tasks[0].kernel_stack;
            }
            // println!("[SCHEDULE] SWITCH TO idle, new_cpustate={:#x} (eip={:#x})",
            //          new_cpustate as u32, unsafe { (*new_cpustate).eip });
            return new_cpustate;
        }

        // Обычная логика для всех последующих переключений
        if self.current_task >= 0 {
            self.tasks[self.current_task as usize].cpu_state_ptr = cpu_state as u32;
        }

        self.current_task = self.get_next_task();

        if self.current_task < 0 || !self.tasks[self.current_task as usize].running {
            self.current_task = 0;
        }

        let new_cpustate = self.tasks[self.current_task as usize].cpu_state_ptr as *mut CPUState;

        // ←←← НОВАЯ ОТЛАДКА
        // unsafe {
        //     let s = &*new_cpustate;
        //     println!("[DEBUG] SWITCH TO task {} → eip={:#x} cs={:#x} ss={:#x} user_esp={:#x} eflags={:#x} | cpu_state_ptr={:#x}",
        //              self.current_task,
        //              s.eip, s.cs, s.ss, s.esp, s.eflags,
        //              new_cpustate as u32);
        // }

        unsafe {
            gdt::TSS.esp0 = self.tasks[self.current_task as usize].kernel_stack;
        }

        // println!("[SCHEDULE] SWITCH TO task {}, new_cpustate={:#x} (eip={:#x})",
        //          self.current_task, new_cpustate as u32, unsafe { (*new_cpustate).eip });

        new_cpustate
    }

    pub fn get_next_task(&self) -> i8 {
        if self.task_count <= 0 {
            return 0; // хотя бы idle
        }

        let mut i = (self.current_task + 1) % MAX_TASKS;
        for _ in 0..MAX_TASKS {
            if self.tasks[i as usize].running {
                return i;
            }
            i = (i + 1) % MAX_TASKS;
        }
        0 // fallback на idle
    }

    pub fn get_free_slot(&self) -> i8 {
        let mut slot: i8 = -1;

        for i in 0..MAX_TASKS {
            let running = self.tasks[i as usize].running;
            if running == false {
                slot = i as i8;
                return slot;
            }
        }

        slot
    }

    pub fn get_current_slot(&self) -> i8 {
        self.current_task
    }

    pub fn list_tasks(&self) {
        println!("Running tasks:");

        for i in 0..MAX_TASKS {
            let running = self.tasks[i as usize].running;
            if running {
                println!("ID: {}", i);
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
    }
}

