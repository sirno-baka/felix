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
    // Порядок соответствует push'ам в naked handler + то, что пушит CPU
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
    esi: u32,
    edi: u32,
    ebp: u32,

    // То, что автоматически пушит процессор при прерывании
    eip: u32,
    cs: u32,
    eflags: u32,
    esp: u32,      // esp задачи (восстанавливается через iretd)
    ss: u32,
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
    pub fn init(&mut self, entry_point: u32, user_stack_top: u32) {
        self.running = true;

        let stack_top = unsafe {
            (&self.stack as *const u8).add(STACK_SIZE) as usize
        };

        let state_ptr = (stack_top - core::mem::size_of::<CPUState>()) as *mut CPUState;
        self.cpu_state_ptr = state_ptr as u32;

        unsafe {
            let state = &mut *state_ptr;

            state.eax = 0;
            state.ebx = 0;
            state.ecx = 0;
            state.edx = 0;
            state.esi = 0;
            state.edi = 0;
            state.ebp = 0;

            // === USER MODE ===
            state.eip    = entry_point;
            state.cs     = 0x1B;      // User Code
            state.eflags = 0x202;
            state.esp    = user_stack_top;   // ← теперь user stack!
            state.ss     = 0x23;      // User Data
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

