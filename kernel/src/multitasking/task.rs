//TASK MANAGER
use core::arch::asm;
use crate::filesystem::file::FileDescriptorTable;
use crate::println;

const STACK_SIZE: usize = 32 * 1024;
const MAX_TASKS: i8 = 8;

//each task has a 4KiB stack containg the cpu state in the bottom part of it
#[derive(Copy, Debug, Clone)]
pub struct Task {
    pub stack: [u8; STACK_SIZE],
    pub cpu_state_ptr: u32, //pub cpu_state: *mut CPUState,
    pub running: bool,

    pub fd_table: FileDescriptorTable,
}


#[repr(C, packed)]
pub struct CPUState {
    // Регистры, которые пушит naked timer handler (в обратном порядке)
    pub ebp: u32,
    pub edi: u32,
    pub esi: u32,
    pub edx: u32,
    pub ecx: u32,
    pub ebx: u32,
    pub eax: u32,

    // То, что CPU автоматически пушит при входе в прерывание (из user mode)
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

        let stack_top = unsafe {
            (&self.stack as *const u8).add(STACK_SIZE) as usize
        };

        let state_ptr = (stack_top - core::mem::size_of::<CPUState>()) as *mut CPUState;
        self.cpu_state_ptr = state_ptr as u32;

        unsafe {
            let state = &mut *state_ptr;
            *state = CPUState {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
                esi: 0,
                edi: 0,
                ebp: 0,

                // === USER MODE ===
                eip:    entry_point,
                cs:     0x1B,      // User Code (RPL=3)
                eflags: 0x202,
                esp:    user_stack_top,
                ss:     0x23,      // User Data (RPL=3)
            };
        }

        self.fd_table = FileDescriptorTable::new();
        println!("[TASK::init] Task ready | entry = {:#x} | user_stack = {:#x} | cpu_state_ptr = {:#x}",
                 entry_point, user_stack_top, self.cpu_state_ptr);
    }

}

pub struct TaskManager {
    pub(crate) tasks: [Task; MAX_TASKS as usize], //arry of tasks
    task_count: i8,                    //how many tasks are in the queue
    current_task: i8,                  //current running task
}

//init null task manager
pub static mut TASK_MANAGER: TaskManager = TaskManager {
    tasks: [NULL_TASK; MAX_TASKS as usize],
    task_count: 0,
    current_task: -1,
};

impl TaskManager {
    pub fn init(&mut self) {
        // self.add_task(idle as u32);
    }

    //add given task to next slot
    pub fn add_task(&mut self, entry_point: u32, user_stack_top: u32) {
        let free_slot = self.get_free_slot();
        if free_slot < 0 {
            println!("[TASK] No free slot!");
            return;
        }
        self.tasks[free_slot as usize].init(entry_point, user_stack_top);
        self.task_count += 1;
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
        if self.task_count <= 0 {
            return cpu_state;
        }

        // Сохраняем состояние текущего таска
        if self.current_task >= 0 {
            self.tasks[self.current_task as usize].cpu_state_ptr = cpu_state as u32;
        }

        // Выбираем следующий runnable таск
        self.current_task = self.get_next_task();

        // Защита: если почему-то вернули плохой индекс — берём idle (0)
        if self.current_task < 0 || !self.tasks[self.current_task as usize].running {
            self.current_task = 0;
        }

        self.tasks[self.current_task as usize].cpu_state_ptr as *mut CPUState
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
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

