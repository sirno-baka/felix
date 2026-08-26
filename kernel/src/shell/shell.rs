use crate::print::PRINTER;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::filesystem::VFS;
use crate::filesystem::vfs::Vfs;
use crate::multitasking::task::TASK_MANAGER;
use crate::{print, println};
use core::arch::asm;
use interrupt_sync::SpinMutex;

const HELP: &'static str = "Available commands:
ls                  - lists root directory entries
cat <file>          - displays content of a file
test <a|b|c>        - runs a dummy task
help                - shows this help";

pub struct Shell {
    buffer: SpinMutex<String>,
}

impl Shell {
    pub fn new() -> Self {
        Shell {
            buffer: SpinMutex::new(String::new()),
        }
    }

    pub unsafe fn run(&self) -> ! {
        self.init();
        loop {
            self.process_input()
        }
    }

    pub unsafe fn process_input(&self) {
        asm!("hlt");

        loop {
            if KEYBOARD_BUFFER.lock().as_ref().unwrap().is_empty() {
                unsafe {
                    crate::wrappers::hlt!();
                }
            } else {
                let event = KEYBOARD_BUFFER.lock().as_mut().unwrap().pop();
                match event {
                    b'\n' => {
                        self.enter();
                        return;
                    }
                    0x08 => {
                        // backspace
                        self.backspace();
                    }
                    c if c.is_ascii_graphic() || c == b' ' => {
                        self.add(c as char);
                    }
                    _ => {}
                }
            }
        }

        // while let Some(byte) = crate::drivers::keyboard_buffer::KEYBOARD_BUFFER.lock() {
        //
        // }
    }

    pub fn add(&self, c: char) {
        self.buffer.lock().push(c);
        print!("{}", c)
    }

    pub fn backspace(&self) {
        if self.buffer.lock().pop().is_some() {
            let mut printer = PRINTER.lock();
            printer.delete();
        }
    }

    pub fn enter(&self) {
        {
            let mut p = PRINTER.lock();
            p.new_line();
        }

        self.interpret();
        self.init();
    }

    pub fn init(&self) {
        self.buffer.lock().clear();

        let mut p = PRINTER.lock();
        p.set_colors(0xc, 0);
        p.prints("felix> ");
        p.reset_colors();
    }

    fn parse_args(&self) -> Vec<String> {
        self.buffer
            .lock()
            .clone()
            .trim()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect()
    }

    fn interpret(&self) {
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
                if let Some(entries) = VFS.get().list_directory_entries(path) {
                    println!("[EXT2] Directory listing for: {}", path);
                    for e in entries {
                        let typ = if e.file_type == 2 { "dir" } else { "file" };
                        println!("  [{:4}] {:<20} {:>8} {}", e.inode, e.name, e.size, typ);
                    }
                } else {
                    println!("[EXT2] Directory not found or error: {}", path);
                }

                println!("[DEBUG] ls done");
            }
            "cat" => {
                if let Some(filename) = args.get(1) {
                    self.cat(filename.as_str());
                } else {
                    println!("Usage: cat <file>");
                }
            }

            "write" => {
                if let Some(filename) = args.get(1) {
                    if let Some(data) = args.get(2) {
                        let success = VFS.get().write_file(filename.as_str(), data.as_bytes());
                        if success {
                            println!("Written to {}", filename);
                        }
                    } else {
                        println!("Usage: write <file> <data>");
                    }
                }
            }
            "run" => {
                if let Some(app) = args.get(1) {
                    // Можно вызвать через syscall или напрямую
                    let result = unsafe {
                        let mut path = app.clone();
                        path.push('\0'); // <-- добавляем нуль-терминатор
                        // Прямой вызов для отладки (позже сделаем через int 0x80)
                        // crate::syscalls::handler::sys_execve(path.as_ptr() as *const u8)
                    };
                    // if result != 0 {
                    //     println!("Failed to run: {}", app);
                    // }
                } else {
                    println!("Usage: run <application>");
                }
            }

            "mkdir" => {
                if let Some(name) = args.get(1) {
                    let success = VFS.get().mkdir(name);
                    println!("mkdir {}: {}", name, success);
                } else {
                    println!("Usage: mkdir <name>");
                }
            }

            "rmdir" => {
                if let Some(name) = args.get(1) {
                    let success = VFS.get().rmdir(name);
                    println!("rmdir {}: {}", name, success);
                } else {
                    println!("Usage: rmdir <name>");
                }
            }

            "rm" => {
                if let Some(filename) = args.get(1) {
                    let success = VFS.get().remove_file(filename.as_str());
                    if success {
                        println!("remove {}", filename);
                    }
                }
            }

            "ps" => unsafe {
                TASK_MANAGER.list_tasks();
            },
            "help" => println!("{}", HELP),
            _ => println!("Unknown command: {}", args[0]),
        }
    }

    pub fn cat(&self, filename: &str) {
        println!("[DEBUG] cat: {}", filename);
        let data = VFS.get().read_file(filename);
        match data {
            None => println!("File not found: {}", filename),
            Some(data) => {
                if let Ok(text) = alloc::string::String::from_utf8(data) {
                    println!("{}", text);
                } else {
                    println!("<binary>");
                }
            }
        }
    }
}
