#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use kolibri_embedded_gui::button::Button;
use kolibri_embedded_gui::label::Label;
use libfelix::embedded_graphics::*;
use libfelix::embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10};
use libfelix::embedded_graphics::mono_font::MonoTextStyle;
use libfelix::embedded_graphics::pixelcolor::Rgb888;
use libfelix::embedded_graphics::prelude::{DrawTarget, Point, Primitive, RgbColor, Size};
use libfelix::embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use libfelix::embedded_graphics::text::Text;
use libfelix::prelude::*;
use libfelix::syscall::{self, write, read, open, close, mkdir, rmdir, unlink, execve, wait, pipe, O_RDONLY, O_WRONLY, O_CREAT, O_TRUNC, O_APPEND};

struct Shell {
    cwd: String,
    path: String,
}

impl Shell {
    fn new() -> Self {
        Self {
            cwd: String::from("/"),
            path: String::from("/"),
        }
    }

    fn prompt(&self) -> String {
        let mut s = String::from("felix:");
        s.push_str(&self.cwd);
        s.push_str("$ ");
        s
    }

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

fn is_directory(path: &str) -> bool {
    let mut p = String::from(path);
    p.push('\0');
    let mut buf = [0u8; 64];
    let n = unsafe { syscall::ls(p.as_ptr() as *const u8, buf.as_mut_ptr(), buf.len()) };
    n > 0 || path == "/"
}

// ====================== Parsing ======================

#[derive(Clone, Copy, PartialEq)]
enum RedirKind {
    In,      // <
    Out,     // >
    Append,  // >>
}

struct Redir {
    kind: RedirKind,
    path: String,
}

struct SimpleCmd {
    args: Vec<String>,
    redirs: Vec<Redir>,
}

/// Split a line into pipeline stages: `a | b | c`
fn split_pipeline(line: &str) -> Vec<String> {
    let mut stages = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        if ch == '|' {
            let t = cur.trim().to_string();
            if !t.is_empty() {
                stages.push(t);
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    let t = cur.trim().to_string();
    if !t.is_empty() {
        stages.push(t);
    }
    stages
}

/// Parse one stage into argv + redirections.
fn parse_simple(stage: &str) -> SimpleCmd {
    let mut args = Vec::new();
    let mut redirs = Vec::new();
    let tokens: Vec<&str> = stage.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t == "<" || t == ">" || t == ">>" {
            let kind = match t {
                "<" => RedirKind::In,
                ">>" => RedirKind::Append,
                _ => RedirKind::Out,
            };
            i += 1;
            if i < tokens.len() {
                redirs.push(Redir {
                    kind,
                    path: tokens[i].to_string(),
                });
            }
        } else if t.starts_with(">>") && t.len() > 2 {
            redirs.push(Redir {
                kind: RedirKind::Append,
                path: t[2..].to_string(),
            });
        } else if t.starts_with('>') && t.len() > 1 {
            redirs.push(Redir {
                kind: RedirKind::Out,
                path: t[1..].to_string(),
            });
        } else if t.starts_with('<') && t.len() > 1 {
            redirs.push(Redir {
                kind: RedirKind::In,
                path: t[1..].to_string(),
            });
        } else {
            args.push(t.to_string());
        }
        i += 1;
    }
    SimpleCmd { args, redirs }
}

/// Open redirection files for a command. Returns (stdin_fd, stdout_fd) as i32 (-1 = default).
/// Caller must close any >= 0 fds after spawn.
fn open_redirs(shell: &Shell, redirs: &[Redir]) -> Result<(i32, i32), String> {
    let mut stdin_fd: i32 = -1;
    let mut stdout_fd: i32 = -1;

    for r in redirs {
        let full = shell.resolve(&r.path);
        let mut path = full.clone();
        path.push('\0');
        match r.kind {
            RedirKind::In => {
                let fd = unsafe { open(path.as_ptr() as *const u8, O_RDONLY) };
                if fd == usize::MAX {
                    return Err(alloc::format!("{}: No such file", r.path));
                }
                if stdin_fd >= 0 {
                    unsafe { close(stdin_fd as u32); }
                }
                stdin_fd = fd as i32;
            }
            RedirKind::Out => {
                let flags = O_WRONLY | O_CREAT | O_TRUNC;
                let fd = unsafe { open(path.as_ptr() as *const u8, flags) };
                if fd == usize::MAX {
                    return Err(alloc::format!("{}: cannot create", r.path));
                }
                if stdout_fd >= 0 {
                    unsafe { close(stdout_fd as u32); }
                }
                stdout_fd = fd as i32;
            }
            RedirKind::Append => {
                let flags = O_WRONLY | O_CREAT | O_APPEND;
                let fd = unsafe { open(path.as_ptr() as *const u8, flags) };
                if fd == usize::MAX {
                    return Err(alloc::format!("{}: cannot open", r.path));
                }
                if stdout_fd >= 0 {
                    unsafe { close(stdout_fd as u32); }
                }
                stdout_fd = fd as i32;
            }
        }
    }
    Ok((stdin_fd, stdout_fd))
}
use kolibri_embedded_gui::ui::Ui;
use libfelix::wm::medsize_rgb888_style;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Shell window via high-level WM API (userspace owns the client buffer).
    let mut win = Window::create(40, 40, 320, 240, "demo").unwrap();
    let mut ui = Ui::new_fullscreen(&mut win, medsize_rgb888_style());
    ui.clear_background();
    ui.add(Label::new("Basic Example").with_font(FONT_10X20));

    ui.add(Label::new("Basic Counter (7LOC)"));

    ui.add_horizontal(Button::new("-"));
    ui.add_horizontal(Label::new(format!("Clicked times").as_ref()));
    ui.add_horizontal(Button::new("+"));

    win.flip();

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
            0x03 => {
                print!("^C\n");
                buf.clear();
                break;
            }
            0x08 | 0x7f => {
                if !buf.is_empty() {
                    buf.pop();
                    // Kernel console now handles BS: moves cursor left + erases glyph
                    print!("\x08");
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
    let stages = split_pipeline(line.trim());
    if stages.is_empty() {
        return;
    }

    // Single stage — may be builtin
    if stages.len() == 1 {
        let cmd = parse_simple(&stages[0]);
        if cmd.args.is_empty() {
            return;
        }
        if try_builtin(shell, &cmd) {
            return;
        }
        if let Some(pid) = run_external(shell, &cmd, -1, -1) {
            unsafe {
                let _ = wait(pid);
            }
        }
        return;
    }

    // Pipeline: only externals supported for now
    run_pipeline(shell, &stages);
}

fn try_builtin(shell: &mut Shell, cmd: &SimpleCmd) -> bool {
    let name = cmd.args[0].as_str();
    match name {
        "help" | "exit" | "quit" | "pwd" | "cd" | "ls" | "cat" | "mkdir" | "rmdir" | "rm"
        | "path" | "ps" => {}
        _ => return false,
    }

    // Builtins with output redirection: capture via temporary approach
    // For cat/ls we write to redirected fd if present.
    let out_fd = match open_redirs(shell, &cmd.redirs) {
        Ok((_in, out)) => out,
        Err(e) => {
            println!("{}", e);
            return true;
        }
    };
    // input redirect for builtins (cat reads files by name, ignore < for now unless no args)

    match name {
        "help" => {
            let msg = help_text();
            write_out(out_fd, msg.as_bytes());
        }
        "exit" | "quit" => unsafe { syscall::exit() },
        "pwd" => {
            let mut s = shell.cwd.clone();
            s.push('\n');
            write_out(out_fd, s.as_bytes());
        }
        "cd" => {
            let target = cmd.args.get(1).map(|s| s.as_str()).unwrap_or("/");
            let new_cwd = shell.resolve(target);
            if is_directory(&new_cwd) {
                shell.cwd = new_cwd;
            } else {
                println!("cd: {}: No such directory", target);
            }
        }
        "ls" => {
            let path = cmd
                .args
                .get(1)
                .map(|s| shell.resolve(s))
                .unwrap_or_else(|| shell.cwd.clone());
            ls_to(&path, out_fd);
        }
        "cat" => {
            if let Some(file) = cmd.args.get(1) {
                cat_to(&shell.resolve(file), out_fd);
            } else {
                println!("Usage: cat <file>");
            }
        }
        "mkdir" => {
            if let Some(dir) = cmd.args.get(1) {
                let mut path = shell.resolve(dir);
                path.push('\0');
                unsafe {
                    mkdir(path.as_ptr() as *const u8);
                }
            } else {
                println!("Usage: mkdir <name>");
            }
        }
        "rmdir" => {
            if let Some(dir) = cmd.args.get(1) {
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
            if let Some(file) = cmd.args.get(1) {
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
            if let Some(new_path) = cmd.args.get(1) {
                shell.path = new_path.clone();
                println!("PATH={}", shell.path);
            } else {
                let mut s = shell.path.clone();
                s.push('\n');
                write_out(out_fd, s.as_bytes());
            }
        }
        "ps" => println!("ps: not implemented yet"),
        _ => {}
    }

    if out_fd >= 0 {
        unsafe {
            close(out_fd as u32);
        }
    }
    true
}

fn write_out(fd: i32, data: &[u8]) {
    if fd < 0 {
        unsafe {
            write(1, data.as_ptr(), data.len());
        }
    } else {
        unsafe {
            write(fd as u32, data.as_ptr(), data.len());
        }
    }
}

fn run_external(shell: &Shell, cmd: &SimpleCmd, forced_in: i32, forced_out: i32) -> Option<i32> {
    let name = cmd.args[0].as_str();
    let full = match shell.find_executable(name) {
        Some(p) => p,
        None => {
            println!("{}: command not found", name);
            return None;
        }
    };

    let (mut sin, mut sout) = match open_redirs(shell, &cmd.redirs) {
        Ok(v) => v,
        Err(e) => {
            println!("{}", e);
            return None;
        }
    };
    if forced_in >= 0 {
        if sin >= 0 {
            unsafe {
                close(sin as u32);
            }
        }
        sin = forced_in;
    }
    if forced_out >= 0 {
        if sout >= 0 {
            unsafe {
                close(sout as u32);
            }
        }
        sout = forced_out;
    }

    let pid = spawn_elf(&full, sin, sout, -1, &cmd.args);
    // Parent closes its copies of redirect fds (child has its own refs)
    if sin >= 0 && forced_in < 0 {
        unsafe {
            close(sin as u32);
        }
    }
    if sout >= 0 && forced_out < 0 {
        unsafe {
            close(sout as u32);
        }
    }
    pid
}

fn spawn_elf(
    path: &str,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    args: &[String],
) -> Option<i32> {
    match File::open(path) {
        Ok(mut f) => match f.read_to_end() {
            Ok(data) => {
                if data.len() < 4 || &data[0..4] != b"\x7fELF" {
                    println!("{}: not an executable", path);
                    return None;
                }
                // Build C-string argv (argv[0] = path or args[0])
                let mut c_strings: Vec<String> = Vec::new();
                if args.is_empty() {
                    c_strings.push({
                        let mut s = String::from(path);
                        s.push('\0');
                        s
                    });
                } else {
                    for a in args {
                        let mut s = a.clone();
                        s.push('\0');
                        c_strings.push(s);
                    }
                }
                let ptrs: Vec<*const u8> =
                    c_strings.iter().map(|s| s.as_ptr()).collect();

                unsafe {
                    let pid = execve(
                        data.as_ptr(),
                        data.len(),
                        stdin_fd,
                        stdout_fd,
                        stderr_fd,
                        &ptrs,
                    );
                    if pid == usize::MAX {
                        println!("execve failed: {}", path);
                        None
                    } else {
                        Some(pid as i32)
                    }
                }
            }
            Err(e) => {
                println!("read error: {:?}", e);
                None
            }
        },
        Err(_) => {
            println!("{}: No such file", path);
            None
        }
    }
}

fn run_pipeline(shell: &Shell, stages: &[String]) {
    let n = stages.len();
    if n == 0 {
        return;
    }

    // Create n-1 pipes
    let mut pipes: Vec<(u32, u32)> = Vec::new();
    for _ in 0..n - 1 {
        let mut fds = [0u32; 2];
        let r = unsafe { pipe(fds.as_mut_ptr()) };
        if r != 0 {
            println!("pipe failed");
            return;
        }
        pipes.push((fds[0], fds[1]));
    }

    let mut pids: Vec<i32> = Vec::new();

    for (i, stage) in stages.iter().enumerate() {
        let cmd = parse_simple(stage);
        if cmd.args.is_empty() {
            continue;
        }

        let in_fd: i32 = if i == 0 {
            -1 // may be overridden by <
        } else {
            pipes[i - 1].0 as i32
        };
        let out_fd: i32 = if i == n - 1 {
            -1 // may be overridden by >
        } else {
            pipes[i].1 as i32
        };

        // open_redirs inside run_external may override; for middle stages force pipe ends
        let pid = if i == 0 && i == n - 1 {
            run_external(shell, &cmd, -1, -1)
        } else if i == 0 {
            run_external(shell, &cmd, -1, out_fd)
        } else if i == n - 1 {
            run_external(shell, &cmd, in_fd, -1)
        } else {
            run_external(shell, &cmd, in_fd, out_fd)
        };

        if let Some(p) = pid {
            pids.push(p);
        }
    }

    // Parent must close all pipe ends so children get EOF
    for (r, w) in pipes {
        unsafe {
            close(r);
            close(w);
        }
    }

    // Wait for all children (foreground = last for Ctrl+C)
    for (i, pid) in pids.iter().enumerate() {
        unsafe {
            let _ = wait(*pid);
        }
        let _ = i;
    }
}

fn help_text() -> String {
    String::from(
        "Builtins:\n\
  ls [path]        - list directory\n\
  cat <file>       - display file content\n\
  cd [dir]         - change directory\n\
  pwd              - print working directory\n\
  path [dirs]      - show or set PATH\n\
  mkdir / rmdir / rm\n\
  help / exit\n\n\
Redirection:\n\
  cmd > file       - stdout to file (truncate)\n\
  cmd >> file      - stdout append\n\
  cmd < file       - stdin from file\n\n\
Pipes:\n\
  cmd1 | cmd2      - pipeline\n\n\
External:\n\
  ./hello  /hello  hello (PATH)\n",
    )
}

fn ls_to(path: &str, out_fd: i32) {
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
            let mut line = String::from(entry);
            line.push('\n');
            write_out(out_fd, line.as_bytes());
        }
    }
}

fn cat_to(filename: &str, out_fd: i32) {
    let mut path = String::from(filename);
    path.push('\0');

    let fd = unsafe { open(path.as_ptr() as *const u8, O_RDONLY) };
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
        write_out(out_fd, &buf[..n]);
    }

    unsafe {
        close(fd as u32);
    }
}
