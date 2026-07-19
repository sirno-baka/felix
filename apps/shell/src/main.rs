#![no_std]
#![no_main]
extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libfelix::{print, println};
use libfelix::syscall::{self, write, read, open, close, mkdir, rmdir, unlink, execve};

const PROMPT: &str = "felix> ";

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    println!("\n=== Felix User Shell ===");
    println!("Type 'help' for commands\n");

    loop {
        print!("{}", PROMPT);

        let line = read_line();
        if line.trim().is_empty() {
            continue;
        }
        interpret(line);
    }
}

fn read_line() -> String {
    let mut buf = String::new();
    let mut byte_buf = [0u8; 1];

    loop {
        let n = unsafe { read(0, byte_buf.as_mut_ptr(), 1) };
        if n == 0 {
            break;
        }

        let c = byte_buf[0];

        match c {
            b'\n' | b'\r' => {
                print!("\n");
                break;
            }
            0x08 | 0x7f => { // backspace
                if !buf.is_empty() {
                    buf.pop();
                    print!("\x08\x08");
                }
            }
            c if c.is_ascii_graphic() || c == b' ' => {
                buf.push(c as char);
                print!("{}", c as char);
            }
            _ => {}
        }
    }

    buf
}

fn interpret(line: String) {
    let args: Vec<String> = line
        .trim()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    if args.is_empty() {
        return;
    }

    match args[0].as_str() {
        "help" => print_help(),
        "exit" | "quit" => unsafe { syscall::exit() },

        "ls" => {
            let path = args.get(1).map(|s| s.as_str()).unwrap_or("/");
            ls(path);
        }
        "cat" => {
            if let Some(file) = args.get(1) {
                cat(file);
            } else {
                println!("Usage: cat <file>");
            }
        }
        "run" => {
            if let Some(app) = args.get(1) {
                let mut path = app.clone();
                path.push('\0');
                unsafe {
                    execve(path.as_ptr() as *const u8);
                }
            } else {
                println!("Usage: run <application>");
            }
        }
        "mkdir" => {
            if let Some(dir) = args.get(1) {
                let mut path = dir.clone();
                path.push('\0');
                unsafe { mkdir(path.as_ptr() as *const u8); }
            } else {
                println!("Usage: mkdir <name>");
            }
        }
        "rmdir" => {
            if let Some(dir) = args.get(1) {
                let mut path = dir.clone();
                path.push('\0');
                unsafe { rmdir(path.as_ptr() as *const u8); }
            } else {
                println!("Usage: rmdir <name>");
            }
        }
        "rm" => {
            if let Some(file) = args.get(1) {
                let mut path = file.clone();
                path.push('\0');
                unsafe { unlink(path.as_ptr() as *const u8); }
            } else {
                println!("Usage: rm <file>");
            }
        }
        "ps" => println!("ps: not implemented yet"),
        _ => println!("Unknown command: {}", args[0]),
    }
}

fn print_help() {
    println!(
        r#"Available commands:
  ls [path]           - list directory
  cat <file>          - display file content
  run <app>           - execute binary (e.g. run /hello.bin)
  mkdir <name>        - create directory
  rmdir <name>        - remove directory
  rm <file>           - remove file
  help                - show this help
  exit                - exit shell"#
    );
}

fn ls(path: &str) {
    let mut path_buf = String::from(path);
    if path_buf.is_empty() {
        path_buf.push('/');
    }
    path_buf.push('\0');

    let mut buf = [0u8; 4096];
    let n = unsafe { syscall::ls(path_buf.as_ptr() as *const u8, buf.as_mut_ptr(), buf.len()) };
    if n == 0 {
        println!("ls: cannot read directory");
        return;
    }

    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    for entry in text.lines() {
        if !entry.is_empty() {
            println!("{}", entry);
        }
    }
}

fn cat(filename: &str) {
    let mut path = String::from(filename);
    path.push('\0');

    let fd = unsafe { open(path.as_ptr() as *const u8, 0) };
    if fd == usize::MAX {
        println!("File not found: {}", filename);
        return;
    }

    println!("fd: {:?}", fd);
    let mut buf = [0u8; 512];
    loop {
        let n = unsafe { read(fd as u32, buf.as_mut_ptr(), buf.len()) };
        println!("n: {:?}", n);
        if n == 0 {
            break;
        }
        unsafe { write(1, buf.as_ptr(), n); }
    }

    unsafe { close(fd as u32); }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}