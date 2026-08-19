#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libfelix::prelude::*;
use libfelix::syscall::{
    self, write, read, open, close, mkdir, rmdir, unlink, execve, wait, pipe, O_RDONLY, O_WRONLY,
    O_CREAT, O_TRUNC, O_APPEND,
};

// ---------------------------------------------------------------------------
// Shell state
// ---------------------------------------------------------------------------

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
    In,     // <
    Out,    // >
    Append, // >>
}

struct Redir {
    kind: RedirKind,
    path: String,
}

struct SimpleCmd {
    args: Vec<String>,
    redirs: Vec<Redir>,
}

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
                    return Err(format!("{}: No such file", r.path));
                }
                if stdin_fd >= 0 {
                    unsafe {
                        close(stdin_fd as u32);
                    }
                }
                stdin_fd = fd as i32;
            }
            RedirKind::Out => {
                let flags = O_WRONLY | O_CREAT | O_TRUNC;
                let fd = unsafe { open(path.as_ptr() as *const u8, flags) };
                if fd == usize::MAX {
                    return Err(format!("{}: cannot create", r.path));
                }
                if stdout_fd >= 0 {
                    unsafe {
                        close(stdout_fd as u32);
                    }
                }
                stdout_fd = fd as i32;
            }
            RedirKind::Append => {
                let flags = O_WRONLY | O_CREAT | O_APPEND;
                let fd = unsafe { open(path.as_ptr() as *const u8, flags) };
                if fd == usize::MAX {
                    return Err(format!("{}: cannot open", r.path));
                }
                if stdout_fd >= 0 {
                    unsafe {
                        close(stdout_fd as u32);
                    }
                }
                stdout_fd = fd as i32;
            }
        }
    }
    Ok((stdin_fd, stdout_fd))
}

// ---------------------------------------------------------------------------
// Terminal buffer (history of completed lines)
// ---------------------------------------------------------------------------

const MAX_HISTORY: usize = 64;
/// Total label rows. Last row is always the live prompt+input line.
const VISIBLE_LINES: usize = 22;
const HISTORY_ROWS: usize = VISIBLE_LINES - 1;

struct TermBuffer {
    lines: Vec<String>,
}

impl TermBuffer {
    fn new() -> Self {
        Self { lines: Vec::new() }
    }

    fn push(&mut self, line: &str) {
        for part in line.split('\n') {
            self.lines.push(String::from(part));
            if self.lines.len() > MAX_HISTORY {
                self.lines.remove(0);
            }
        }
    }

    fn clear(&mut self) {
        self.lines.clear();
    }

    /// Last HISTORY_ROWS of completed output (oldest first).
    fn visible_history(&self) -> impl Iterator<Item = &str> {
        let start = self.lines.len().saturating_sub(HISTORY_ROWS);
        self.lines[start..].iter().map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Builtins / interpreter
// ---------------------------------------------------------------------------

fn try_builtin(shell: &mut Shell, cmd: &SimpleCmd, out: &mut TermBuffer) -> bool {
    let name = cmd.args[0].as_str();
    match name {
        "help" | "exit" | "quit" | "pwd" | "cd" | "ls" | "cat" | "mkdir" | "rmdir" | "rm"
        | "path" | "ps" | "clear" | "echo" => {}
        _ => return false,
    }

    let file_fd = match open_redirs(shell, &cmd.redirs) {
        Ok((_in, out_fd)) => out_fd,
        Err(e) => {
            out.push(&e);
            return true;
        }
    };

    match name {
        "help" => {
            let msg = help_text();
            if file_fd >= 0 {
                unsafe {
                    write(file_fd as u32, msg.as_bytes().as_ptr(), msg.len());
                    close(file_fd as u32);
                }
            } else {
                for line in msg.lines() {
                    out.push(line);
                }
            }
        }

        "echo" => {
            let mut msg = String::new();
            for (i, arg) in cmd.args.iter().enumerate().skip(1) {
                if i > 1 {
                    msg.push(' ');
                }
                msg.push_str(arg);
            }

            if file_fd >= 0 {
                msg.push('\n');
                unsafe {
                    write(file_fd as u32, msg.as_bytes().as_ptr(), msg.len());
                    close(file_fd as u32);
                }
            } else {
                out.push(&msg);
            }
        }
        
        "exit" | "quit" => {
            out.push("Goodbye.");
        }
        "pwd" => {
            let s = shell.cwd.clone();
            if file_fd >= 0 {
                let mut b = s.clone();
                b.push('\n');
                unsafe {
                    write(file_fd as u32, b.as_bytes().as_ptr(), b.len());
                    close(file_fd as u32);
                }
            } else {
                out.push(&s);
            }
        }
        "cd" => {
            let target = cmd.args.get(1).map(|s| s.as_str()).unwrap_or("/");
            let new_cwd = shell.resolve(target);
            if is_directory(&new_cwd) {
                shell.cwd = new_cwd;
            } else {
                out.push(&format!("cd: {}: No such directory", target));
            }
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        "ls" => {
            let path = cmd
                .args
                .get(1)
                .map(|s| shell.resolve(s))
                .unwrap_or_else(|| shell.cwd.clone());
            ls_to(&path, file_fd, out);
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        "cat" => {
            if let Some(file) = cmd.args.get(1) {
                cat_to(&shell.resolve(file), file_fd, out);
            } else {
                out.push("Usage: cat <file>");
            }
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
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
                out.push("Usage: mkdir <name>");
            }
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
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
                out.push("Usage: rmdir <name>");
            }
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
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
                out.push("Usage: rm <file>");
            }
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        "path" => {
            if let Some(new_path) = cmd.args.get(1) {
                shell.path = new_path.clone();
                out.push(&format!("PATH={}", shell.path));
            } else {
                out.push(&shell.path);
            }
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        "ps" => {
            out.push("ps: not implemented yet");
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        "clear" => {
            out.clear();
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        _ => {}
    }
    true
}

fn ls_to(path: &str, file_fd: i32, out: &mut TermBuffer) {
    let mut path_buf = String::from(path);
    if path_buf.is_empty() {
        path_buf.push('/');
    }
    path_buf.push('\0');

    let mut buf = [0u8; 4096];
    let n = unsafe { syscall::ls(path_buf.as_ptr() as *const u8, buf.as_mut_ptr(), buf.len()) };
    if n == 0 {
        out.push(&format!("ls: cannot read directory: {}", path));
        return;
    }

    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    for entry in text.lines() {
        if !entry.is_empty() {
            if file_fd >= 0 {
                let mut line = String::from(entry);
                line.push('\n');
                unsafe {
                    write(file_fd as u32, line.as_bytes().as_ptr(), line.len());
                }
            } else {
                out.push(entry);
            }
        }
    }
}

fn cat_to(filename: &str, file_fd: i32, out: &mut TermBuffer) {
    let mut path = String::from(filename);
    path.push('\0');

    let fd = unsafe { open(path.as_ptr() as *const u8, O_RDONLY) };
    if fd == usize::MAX {
        out.push(&format!("File not found: {}", filename));
        return;
    }

    let mut buf = [0u8; 512];
    loop {
        let n = unsafe { read(fd as u32, buf.as_mut_ptr(), buf.len()) };
        if n == 0 {
            break;
        }
        if file_fd >= 0 {
            unsafe {
                write(file_fd as u32, buf.as_ptr(), n);
            }
        } else if let Ok(s) = core::str::from_utf8(&buf[..n]) {
            for line in s.split('\n') {
                out.push(line);
            }
        }
    }

    unsafe {
        close(fd as u32);
    }
}

fn run_external(
    shell: &Shell,
    cmd: &SimpleCmd,
    forced_in: i32,
    forced_out: i32,
    out: &mut TermBuffer,
) -> Option<i32> {
    let name = cmd.args[0].as_str();
    let full = match shell.find_executable(name) {
        Some(p) => p,
        None => {
            out.push(&format!("{}: command not found", name));
            return None;
        }
    };

    let (mut sin, mut sout) = match open_redirs(shell, &cmd.redirs) {
        Ok(v) => v,
        Err(e) => {
            out.push(&e);
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

    out.push(&format!("[running {} ...]", full));
    let pid = spawn_elf(&full, sin, sout, -1, &cmd.args);
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
                    return None;
                }
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
                let ptrs: Vec<*const u8> = c_strings.iter().map(|s| s.as_ptr()).collect();

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
                        None
                    } else {
                        Some(pid as i32)
                    }
                }
            }
            Err(_) => None,
        },
        Err(_) => None,
    }
}

fn run_pipeline(shell: &Shell, stages: &[String], out: &mut TermBuffer) {
    let n = stages.len();
    if n == 0 {
        return;
    }

    let mut pipes: Vec<(u32, u32)> = Vec::new();
    for _ in 0..n - 1 {
        let mut fds = [0u32; 2];
        let r = unsafe { pipe(fds.as_mut_ptr()) };
        if r != 0 {
            out.push("pipe failed");
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
            -1
        } else {
            pipes[i - 1].0 as i32
        };
        let out_fd: i32 = if i == n - 1 {
            -1
        } else {
            pipes[i].1 as i32
        };

        let pid = if i == 0 && i == n - 1 {
            run_external(shell, &cmd, -1, -1, out)
        } else if i == 0 {
            run_external(shell, &cmd, -1, out_fd, out)
        } else if i == n - 1 {
            run_external(shell, &cmd, in_fd, -1, out)
        } else {
            run_external(shell, &cmd, in_fd, out_fd, out)
        };

        if let Some(p) = pid {
            pids.push(p);
        }
    }

    for (r, w) in pipes {
        unsafe {
            close(r);
            close(w);
        }
    }

    for pid in pids {
        unsafe {
            let _ = wait(pid);
        }
    }
}

fn interpret(shell: &mut Shell, line: &str, out: &mut TermBuffer) {
    let stages = split_pipeline(line.trim());
    if stages.is_empty() {
        return;
    }

    if stages.len() == 1 {
        let cmd = parse_simple(&stages[0]);
        if cmd.args.is_empty() {
            return;
        }
        if try_builtin(shell, &cmd, out) {
            return;
        }
        if let Some(pid) = run_external(shell, &cmd, -1, -1, out) {
            unsafe {
                let _ = wait(pid);
            }
        }
        return;
    }

    run_pipeline(shell, &stages, out);
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
  clear            - clear terminal\n\
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

// ---------------------------------------------------------------------------
// Classic terminal GUI (no TextInput / no buttons)
// ---------------------------------------------------------------------------

const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;
const MAX_INPUT: usize = 96;
const LINE_MAX_CHARS: usize = 68;

fn truncate_line(s: &str) -> &str {
    if s.len() > LINE_MAX_CHARS {
        &s[s.len() - LINE_MAX_CHARS..]
    } else {
        s
    }
}

/// Redraw all labels: history rows + live prompt line at the bottom.
fn refresh_terminal(
    ui: &mut Ui,
    shell: &Shell,
    term: &TermBuffer,
    input: &str,
    line_ids: &[WidgetId],
) {
    // History fills the first HISTORY_ROWS labels (pad with empty from top).
    let mut hist: Vec<&str> = term.visible_history().collect();
    while hist.len() < HISTORY_ROWS {
        hist.insert(0, "");
    }

    for i in 0..HISTORY_ROWS {
        let text = hist.get(i).copied().unwrap_or("");
        ui.set_label(line_ids[i], truncate_line(text));
    }

    // Last label = prompt + current input (+ caret)
    let mut live = shell.prompt();
    live.push_str(input);
    live.push('_');
    ui.set_label(line_ids[HISTORY_ROWS], truncate_line(&live));
}

fn run_line(shell: &mut Shell, term: &mut TermBuffer, input: &str) {
    let cmd = input.trim();
    if cmd.is_empty() {
        return;
    }
    // Echo the completed line into history
    term.push(&format!("{}{}", shell.prompt(), cmd));
    interpret(shell, cmd, term);
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut win = Window::create(30, 30, 640, 400, "Felix Shell").unwrap_or_else(|| {
        Window::create(40, 40, 480, 320, "Felix Shell").expect("wm_create failed")
    });

    let mut ui = Ui::new();

    // Only labels — one per visible row (last row is the live input line)
    let mut line_ids: Vec<WidgetId> = Vec::new();
    let line_y0 = 8;
    let line_h = 16;
    for i in 0..VISIBLE_LINES {
        let y = line_y0 + (i as i32) * line_h;
        line_ids.push(ui.add_label(Label::new(10, y, "")));
    }

    let mut shell = Shell::new();
    let mut term = TermBuffer::new();
    let mut input = String::new();

    term.push("=== Felix User Shell ===");
    term.push("Type commands and press Enter.  help — builtins, clear — wipe view.");
    term.push("");

    refresh_terminal(&mut ui, &shell, &term, &input, &line_ids);
    ui.draw(&mut win);
    let _ = win.flip();

    loop {
        let mut dirty = false;

        let mut evbuf = [WmEvent::default(); 64];
        let n = win.poll_events(&mut evbuf);

        for e in &evbuf[..n] {
            if e.kind != EV_KEY_DOWN {
                continue;
            }
            let scancode = e.a as u8;
            let ch = e.b as u8;

            if scancode == SCAN_ENTER {
                run_line(&mut shell, &mut term, &input);
                input.clear();
                dirty = true;
            } else if scancode == SCAN_BACKSPACE {
                if input.pop().is_some() {
                    dirty = true;
                }
            } else if ch >= 0x20 && ch < 0x7f && input.len() < MAX_INPUT {
                input.push(ch as char);
                dirty = true;
            }
        }

        if dirty {
            refresh_terminal(&mut ui, &shell, &term, &input, &line_ids);
            ui.draw(&mut win);
            let _ = win.flip();
        }
    }
}
