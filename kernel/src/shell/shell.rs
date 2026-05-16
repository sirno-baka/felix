// SHELL
// Нормальная, высокоуровневая версия с настоящим парсингом аргументов

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use crate::multitasking::task::TASK_MANAGER;
use crate::print::PRINTER;

use crate::memory::paging::{PAGING, TABLES};

use core::arch::asm;
use crate::{filesystem, print, println};

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
                println!("[DEBUG] ls started: {}", path);
                {
                    if let Some(vfs) = crate::filesystem::VFS.lock().as_ref() {
                        vfs.list_directory(path);
                        println!("[VFS] Listing directory");
                    } else {
                        println!("[VFS] Not initialized");
                    }
                }
                println!("[DEBUG] ls command fully completed");
            },

            "cat" => {
                if let Some(filename) = args.get(1) {
                    self.cat(filename.as_str());
                } else {
                    println!("Usage: cat <file>");
                }
            },

            "write" => {
                if let Some(filename) = args.get(1) {
                    if let Some(data) = args.get(2) {
                        if let Some(vfs) = crate::filesystem::VFS.lock().as_ref() {
                            let success = vfs.write_file(filename.as_str(), data.as_bytes());
                            if success {
                                println!("Written to {}", filename);
                            }
                        }
                    } else {
                        println!("Usage: write <file> <data>");
                    }
                }
            },

            "ps" => unsafe { TASK_MANAGER.list_tasks(); },
            "help" => println!("{}", HELP),
            "mkdir" => unsafe {
                if let Some(name) = args.get(1) {

                } else {
                    println!("Usage: mkdir <name>");
                }
            },
            "test" => unsafe {
                println!("write");
                let filename = "test";
                if let Some(vfs) = crate::filesystem::VFS.lock().as_ref() {
                    let success = vfs.read_file(filename);
                    if success.is_some() {
                        println!("Written to {:?}", success.unwrap());
                    }
                }
            },

            _ => println!("Unknown command: {}", args[0]),
        }
    }

    // показывает содержимое файла (теперь БЕЗ unsafe и с with_ext2_slave)
    pub fn cat(&self, filename: &str) {
        println!("[DEBUG] cat started: {}", filename);
        {
            if let Some(vfs) = crate::filesystem::VFS.lock().as_mut() {
                let data = vfs.read_file(filename);
                match data {
                    None => println!("File not found: {}", filename),
                    Some(data) => {
                        println!("data: {:?}", data.len());
                        println!("data: {:?}", data);
                        if let Ok(text) = alloc::string::String::from_utf8(data) {
                            println!("{}", text);
                        } else {
                            println!("<binary file>");
                        }
                    }
                }
            } else {
                println!("[VFS] Not initialized");
            }
        }
        println!("[DEBUG] cat command fully completed");
    }

    // запускает исполняемый файл как задачу
    pub unsafe fn run(&self, filename: &str) {
        // let fat = FAT.lock();
        // let entry = fat.search_file(filename);   // аналогично cat
        //
        // if entry.name[0] != 0 {
        //     let slot = TASK_MANAGER.get_free_slot();
        //     let target = APP_TARGET + (slot as u32 * APP_SIZE);
        //
        //     // map table 8
        //     TABLES[8].set(target);
        //     PAGING.set_table(8, &TABLES[8]);
        //
        //     fat.read_file_to_target(&entry, target as *mut u32);
        //
        //     let signature = *(target as *mut u32);
        //     if signature == APP_SIGNATURE {
        //         TASK_MANAGER.add_task((target + 4) as u32);
        //     } else {
        //         println!("File is not a valid executable!");
        //     }
        // } else {
        //     println!("Program not found: {}", filename);
        // }
    }
}