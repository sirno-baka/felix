use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::print::PRINTER;

use crate::multitasking::task::TASK_MANAGER;
use core::arch::asm;
use interrupt_sync::SpinMutex;
use crate::{print, println};

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
        while let Some(byte) = crate::drivers::keyboard_buffer::KEYBOARD_BUFFER.pop() {
            match byte {
                b'\n' => {
                    self.enter();
                    return;
                }
                0x08 => {  // backspace
                    self.backspace();
                }
                c if c.is_ascii_graphic() || c == b' ' => {
                    self.add(c as char);
                }
                _ => {}
            }
        }
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
            .lock().clone().trim()
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
                if let Some(vfs) = crate::filesystem::VFS.lock().as_ref() {
                    vfs.list_directory(path);
                }
                println!("[DEBUG] ls done");
            }
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
                        if let Some(vfs) = crate::filesystem::VFS.lock().as_mut() {
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

            "rm" => {
                if let Some(filename) = args.get(1) {
                    if let Some(vfs) = crate::filesystem::VFS.lock().as_mut() {
                        let success = vfs.remove_file(filename.as_str());
                        if success {
                            println!("remove {}", filename);
                        }
                    }
                }
            },

            "ps" => unsafe { TASK_MANAGER.list_tasks(); },
            "help" => println!("{}", HELP),
            _ => println!("Unknown command: {}", args[0]),
        }
    }

    pub fn cat(&self, filename: &str) {
        println!("[DEBUG] cat: {}", filename);
        if let Some(vfs) = crate::filesystem::VFS.lock().as_mut() {
            let data = vfs.read_file(filename);
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
}