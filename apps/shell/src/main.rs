#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libfelix::prelude::*;
use libfelix::syscall::{self, write, read, open, close, mkdir, rmdir, unlink, execve, wait};

/// Shell state: current working directory + PATH.
struct Shell {
    cwd: String,
    /// Colon-separated search path for executables (like bash $PATH).
    path: String,
}

impl Shell {
    fn new() -> Self {
        Self {
            cwd: String::from("/"),
            // Root is on the PATH so `hello` works without `./`
            path: String::from("/"),
        }
    }

    fn prompt(&self) -> String {
        let mut s = String::from("felix:");
        s.push_str(&self.cwd);
        s.push_str("$ ");
        s
    }

    /// Resolve a user path against cwd.
    /// Absolute paths are cleaned as-is; relative are joined with cwd.
    fn resolve(&self, path: &str) -> String {
        let joined = if path.starts_with('/') {
            path.to_string()
        } else if self.cwd == "/" {
            let mut s = String::from("/");
            s.push_str(path);
            s
        } else {
            let mut s = self.cwd.clone();
            s.push('/');
            s.push_str(path);
            s
        };
        normalize_path(&joined)
    }

    /// Try to find an executable for `name`.
    /// - if name contains '/', resolve relative to cwd (./hello, /bin/foo)
    /// - else search each directory in PATH
    fn find_executable(&self, name: &str) -> Option<String> {
        if name.contains('/') {
            let full = self.resolve(name);
            if file_exists(&full) {
                return Some(full);
            }
            return None;
        }

        for dir in self.path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = if dir == "/" {
                let mut s = String::from("/");
                s.push_str(name);
                s
            } else {
                let mut s = String::from(dir);
                s.push('/');
                s.push_str(name);
                normalize_path(&s)
            };
            if file_exists(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

/// Collapse `.` / `..` and duplicate slashes. Always returns an absolute path.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return String::from("/");
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

fn file_exists(path: &str) -> bool {
    File::open(path).is_ok()
}

/// True if `path` looks like a directory (open fails as file, but ls works).
fn is_directory(path: &str) -> bool {
    let mut p = String::from(path);
    p.push('\0');
    let mut buf = [0u8; 64];
    let n = unsafe { syscall::ls(p.as_ptr() as *const u8, buf.as_mut_ptr(), buf.len()) };
    n > 0 || path == "/"
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    println!("\n=== Felix User Shell ===");
    println!("Type 'help' for commands\n");

    let mut shell = Shell::new();

    loop {
        print!("{}", shell.prompt());

        let line = read_line();
        if line.trim().is_empty() {
            continue;
        }
        interpret(&mut shell, line);
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
            // Ctrl+C at the prompt: cancel the current line
            0x03 => {
                print!("^C\n");
                buf.clear();
                break;
            }
            0x08 | 0x7f => {
                if !buf.is_empty() {
                    buf.pop();
                    print!("\x08 \x08");
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

fn interpret(shell: &mut Shell, line: String) {
    let args: Vec<String> = line
        .trim()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    if args.is_empty() {
        return;
    }

    let cmd = args[0].as_str();

    match cmd {
        "help" => print_help(),
        "exit" | "quit" => unsafe { syscall::exit() },

        "pwd" => {
            println!("{}", shell.cwd);
        }

        "cd" => {
            let target = args.get(1).map(|s| s.as_str()).unwrap_or("/");
            let new_cwd = shell.resolve(target);
            if is_directory(&new_cwd) {
                shell.cwd = new_cwd;
            } else {
                println!("cd: {}: No such directory", target);
            }
        }

        "ls" => {
            let path = args
                .get(1)
                .map(|s| shell.resolve(s))
                .unwrap_or_else(|| shell.cwd.clone());
            ls(&path);
        }

        "cat" => {
            if let Some(file) = args.get(1) {
                cat(&shell.resolve(file));
            } else {
                println!("Usage: cat <file>");
            }
        }

        "mkdir" => {
            if let Some(dir) = args.get(1) {
                let full = shell.resolve(dir);
                let mut path = full;
                path.push('\0');
                unsafe {
                    mkdir(path.as_ptr() as *const u8);
                }
            } else {
                println!("Usage: mkdir <name>");
            }
        }

        "rmdir" => {
            if let Some(dir) = args.get(1) {
                let mut path = shell.resolve(dir);
                path.push('\0');
                unsafe {
                    rmdir(path.as_ptr() as *const u8);
                }
            } else {
                println!("Usage: rmdir <name>");
            }
        }

        "rm" => {
            if let Some(file) = args.get(1) {
                let mut path = shell.resolve(file);
                path.push('\0');
                unsafe {
                    unlink(path.as_ptr() as *const u8);
                }
            } else {
                println!("Usage: rm <file>");
            }
        }

        "path" => {
            if let Some(new_path) = args.get(1) {
                shell.path = new_path.clone();
                println!("PATH={}", shell.path);
            } else {
                println!("{}", shell.path);
            }
        }

        "ps" => println!("ps: not implemented yet"),

        // External command: resolve via PATH or explicit path, then exec + wait
        _ => {
            match shell.find_executable(cmd) {
                Some(full_path) => exec_program(&full_path),
                None => println!("{}: command not found", cmd),
            }
        }
    }
}

fn exec_program(path: &str) {
    match File::open(path) {
        Ok(mut f) => match f.read_to_end() {
            Ok(data) => {
                // ELF magic check — avoid execve-ing random text files
                if data.len() < 4 || &data[0..4] != b"\x7fELF" {
                    println!("{}: not an executable f4b: {:?}", path, &data[0..4]);
                    return;
                }
                unsafe {
                    let pid = execve(data.as_ptr(), data.len());
                    if pid == usize::MAX {
                        println!("execve failed: {}", path);
                    } else {
                        let _ = wait(pid as i32);
                    }
                }
            }
            Err(e) => println!("read error: {:?}", e),
        },
        Err(IoError::NotFound) => println!("{}: No such file", path),
        Err(e) => println!("open error: {:?}", e),
    }
}

fn print_help() {
    println!(
        r#"Builtins:
  ls [path]        - list directory (relative to cwd)
  cat <file>       - display file content
  cd [dir]         - change directory
  pwd              - print working directory
  path [dirs]      - show or set PATH (colon-separated)
  mkdir <name>     - create directory
  rmdir <name>     - remove directory
  rm <file>        - remove file
  help             - this help
  exit             - exit shell

External programs:
  ./hello          - run binary in current directory
  /hello           - absolute path
  hello            - search PATH (default: /)
"#
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
        println!("ls: cannot read directory: {}", path);
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

    let mut buf = [0u8; 512];
    loop {
        let n = unsafe { read(fd as u32, buf.as_mut_ptr(), buf.len()) };
        if n == 0 {
            break;
        }
        unsafe {
            write(1, buf.as_ptr(), n);
        }
    }

    unsafe {
        close(fd as u32);
    }
}
