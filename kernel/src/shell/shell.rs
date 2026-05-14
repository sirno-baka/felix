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
use crate::{filesystem, print, println};
use crate::filesystem::EXT2_SLAVE;

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
    fn parse_args(&self) -> Vec<String> {
        self.buffer
            .clone().trim()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect()
    }

    fn interpret(&mut self) {
        let args: Vec<String> = self.parse_args();

        if args.is_empty() {
            return;
        }

        match args[0].as_str() {
            "ping" => println!("PONG!"),
            "alloc" => {
                let mut v = alloc::vec::Vec::new();
                v.push(1);
                v.push(2);
                println!("vec allocated! len = {}", v.len());
            }

            "ls" => {
                let path = args.get(1).map(|s| s.as_str()).unwrap_or("/");
                crate::filesystem::EXT2_SLAVE.lock().as_mut().map(|fs| {
                    fs.list_directory_path(path);
                });
            },

            "cat" => {
                if let Some(filename) = args.get(1) {
                    self.cat(filename.as_str());
                } else {
                    println!("Usage: cat <file>");
                }
            },

            "ps" => unsafe { TASK_MANAGER.list_tasks(); },
            "help" => println!("{}", HELP),

            "rt" => unsafe { /* ... твой старый код ... */ },



            "mkdir" => unsafe { /* ... твой код ... */ },
            "rmdir" => unsafe { /* ... твой код ... */ },
            "write" => unsafe { /* ... твой код ... */ },
            "run" => unsafe { /* ... твой код ... */ },
            "test" => unsafe { /* ... твой код ... */ },

            _ => println!("Unknown command: {}", args[0]),
        }
    }

    // показывает содержимое файла (теперь БЕЗ unsafe и с with_ext2_slave)
    pub fn cat(&self, filename: &str) {
        let mut guard = crate::filesystem::EXT2_SLAVE.lock();
        if let Some(fs) = guard.as_mut() {
            let data = fs.read_file(filename);
            match data {
                None => println!("File not found or cannot be read: {}", filename),
                Some(data) => {
                    if let Ok(text) = alloc::string::String::from_utf8(data) {
                        println!("{}", text);
                    } else {
                        println!("<binary file>");
                    }
                }
            }
        } else {
            println!("[EXT2] Filesystem not mounted");
        }
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