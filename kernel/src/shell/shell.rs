// SHELL
// Нормальная, высокоуровневая версия с настоящим парсингом аргументов

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use crate::filesystem::fat::FAT;
use crate::multitasking::task::TASK_MANAGER;
use crate::print::PRINTER;

use crate::memory::paging::{PAGING, TABLES};

use core::arch::asm;
use crate::{print, println};

const APP_TARGET: u32 = 0x00a0_0000;
const APP_SIZE: u32 = 0x0001_0000;
const APP_SIGNATURE: u32 = 0xB16B00B5;

const HELP: &'static str = "Available commands:
ls                  - lists root directory entries
cat <file>          - displays content of a file
test <a|b|c>        - runs a dummy task
run <file>          - loads and runs executable
ps                  - lists running tasks
rt <id>             - removes specified task
mkdir <name>        - create directory
rmdir <name>        - remove directory
write <file>        - write test data to file
help                - shows this help";

const PROMPT: &str = "felix> ";

// Warning! Mutable static here
// TODO: заменить на spin::Mutex или lock-free
pub static mut SHELL: Shell = Shell {
    buffer: String::new(),
};

pub struct Shell {
    buffer: String,
}

impl Shell {
    // init shell
    pub fn init(&mut self) {
        self.buffer.clear();

        unsafe {
            PRINTER.set_colors(0xc, 0);
            print!("{}", PROMPT);
            PRINTER.reset_colors();
        }
    }

    // добавляет символ (теперь char — удобнее)
    pub fn add(&mut self, c: char) {
        self.buffer.push(c);
        print!("{}", c);
    }

    // backspace
    pub fn backspace(&mut self) {
        if self.buffer.pop().is_some() {
            unsafe {
                PRINTER.delete();
            }
        }
    }

    // enter
    pub fn enter(&mut self) {
        // e9 port hack + real new line
        unsafe {
            asm!("out dx, al", in("dx") 0xe9 as u16, in("al") '\n' as u8);
            PRINTER.new_line();
        }

        self.interpret();
        self.init();
    }

    // ─────────────────────────────────────────────────────────────
    // НОВЫЙ ПАРСЕР АРГУМЕНТОВ
    // ─────────────────────────────────────────────────────────────
    fn parse_args(&self) -> Vec<&str> {
        self.buffer
            .trim()                    // убираем пробелы в начале/конце
            .split_whitespace()        // разбиваем по любому количеству пробелов
            .collect()                 // Vec<&str> — именно то, что ты хотел
    }

    // command interpreter
    fn interpret(&mut self) {
        let args: Vec<&str> = self.parse_args();

        if args.is_empty() {
            return;
        }

        match args[0] {
            // простые команды без аргументов
            "ping" => println!("PONG!"),
            "alloc" => {
                let mut v = alloc::vec::Vec::new();
                v.push(1);
                v.push(2);
                println!("vec allocated! len = {}", v.len());
            }
            "ls" => unsafe {
                if let Some(id_str) = args.get(1) {
                    FAT.lock(|fat| {
                        fat.list_entries(id_str);
                    });
                } else {

                    FAT.lock(|fat| {
                        fat.list_entries("/");
                    });
                }

            },
            "ps" => unsafe {
                TASK_MANAGER.list_tasks();
            },
            "help" => println!("{}", HELP),

            // команды с аргументами
            "rt" => unsafe {
                if let Some(id_str) = args.get(1) {
                    match id_str.parse::<usize>() {
                        Ok(id) => TASK_MANAGER.remove_task(id),
                        Err(_) => println!("Invalid task id!"),
                    }
                } else {
                    println!("Usage: rt <id>");
                }
            },

            "cat" => unsafe {
                if let Some(filename) = args.get(1) {
                    self.cat(filename);
                } else {
                    println!("Usage: cat <file>");
                }
            },

            "mkdir" => unsafe {
                if let Some(name) = args.get(1) {
                    let f = FAT.acquire_mut();
                    f.mkdir(name);
                    FAT.free();
                } else {
                    println!("Usage: mkdir <name>");
                }
            },

            "rmdir" => unsafe {
                if let Some(name) = args.get(1) {
                    let f = FAT.acquire_mut();
                    f.rmdir(name);
                    FAT.free();
                } else {
                    println!("Usage: rmdir <name>");
                }
            },

            "write" => unsafe {
                if let Some(filename) = args.get(1) {
                    if let Some(data) = args.get(2) {
                        let f = FAT.acquire_mut();
                        f.write_path(filename, data.to_string().as_bytes());
                        FAT.free();
                    }
                } else {
                    println!("Usage: write <file>");
                }
            },

            "run" => unsafe {
                if let Some(filename) = args.get(1) {
                    self.run(filename);
                } else {
                    println!("Usage: run <file>");
                }
            },

            "test" => unsafe {
                if let Some(param) = args.get(1) {
                    match *param {
                        "a" => TASK_MANAGER.add_dummy_task_a(),
                        "b" => TASK_MANAGER.add_dummy_task_b(),
                        "c" => TASK_MANAGER.add_dummy_task_c(),
                        _ => println!("Specify test a, b, or c!"),
                    }
                } else {
                    println!("Usage: test <a|b|c>");
                }
            },

            // неизвестная команда
            _ => println!("Unknown command: {}", args[0]),
        }
    }

    // показывает содержимое файла
    pub unsafe fn cat(&self, filename: &str) {
        let fat = FAT.acquire();
        let entry = fat.search_file(filename);   // если у тебя search_file принимает &str — ок
        // если нет — замени на свой метод

        if entry.name[0] != 0 {
            println!("buf: {:?}", entry);
            let mut buf = Vec::with_capacity(entry.size as usize);
            let target = buf.as_mut_ptr();
            fat.read_file_to_ptr(entry, target);
            let size = entry.size as usize;

            unsafe { buf.set_len(size ); }
            for (i, val) in buf.iter().enumerate() {
                if i > size  {
                    break
                }
                print!("{}", *val as char);
            }

            println!();
        } else {
            println!("File not found: {}", filename);
        }
        FAT.free();
    }

    // запускает исполняемый файл как задачу
    pub unsafe fn run(&self, filename: &str) {
        let fat = FAT.acquire();
        let entry = fat.search_file(filename);   // аналогично cat

        if entry.name[0] != 0 {
            let slot = TASK_MANAGER.get_free_slot();
            let target = APP_TARGET + (slot as u32 * APP_SIZE);

            // map table 8
            TABLES[8].set(target);
            PAGING.set_table(8, &TABLES[8]);

            fat.read_file_to_target(&entry, target as *mut u32);

            let signature = *(target as *mut u32);
            if signature == APP_SIGNATURE {
                TASK_MANAGER.add_task((target + 4) as u32);
            } else {
                println!("File is not a valid executable!");
            }
        } else {
            println!("Program not found: {}", filename);
        }
        FAT.free();
    }
}