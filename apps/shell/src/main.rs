#![no_std]
#![no_main]

extern crate alloc;

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::min;

use libfelix::async_rt::yield_now;
use libfelix::embedded_graphics;
use libfelix::prelude::*;
mod executor;
mod line_editor;
mod parser;
mod terminal;
use executor::{interpret, poll_background_jobs};
use line_editor::LineEditor;
use parser::{parse_line, CommandGroup, Connector, Redir, RedirKind, RedirTarget, SimpleCmd, PROTECTED};
use terminal::{Terminal, CELL_H, CELL_W};
use libfelix::syscall::{
    self, chdir, close, execve_env_pgid, execve_wasm_env_pgid, getpid, getpgrp, kill, mkdir, mount, mount_list,
    open, openpty, pipe, read, rmdir, set_nonblock, setpgid, task_list, tty_setfg, umount2, unlink,
    waitpid_status, write, O_APPEND, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, SIGCONT, SIGINT,
    SIGKILL, SIGSTOP, SIGTERM, SIGTSTP, TASK_RUNNING, TASK_STOPPED, TASK_ZOMBIE, WNOHANG,
};

// ---------------------------------------------------------------------------
// Shell state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Stopped,
}

struct BackgroundJob {
    id: u32,
    pgid: i32,
    pids: Vec<i32>,
    last_pid: i32,
    command: String,
    state: JobState,
    capture_fd: Option<u32>,
    stdin_w: Option<u32>,
    last_status: i32,
}

struct EnvVar {
    name: String,
    value: String,
    exported: bool,
}

struct Shell {
    cwd: String,
    old_cwd: String,
    path: String,
    command_cache: Option<Vec<String>>,
    jobs: Vec<BackgroundJob>,
    next_job_id: u32,
    env: Vec<EnvVar>,
    last_status: i32,
    should_exit: bool,
}

fn list_dir(path: &str) -> Vec<String> {
    let mut path_buf = String::from(path);
    if !path_buf.ends_with('/') && !path_buf.is_empty() {
        path_buf.push('/');
    }
    path_buf.push('\0');
    let mut buf = [0u8; 4096];
    let n = unsafe { syscall::ls(path_buf.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    if n == 0 {
        return Vec::new();
    }
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    let mut result = Vec::new();
    for entry in text.lines() {
        let e = entry.trim();
        if !e.is_empty() {
            result.push(e.to_string());
        }
    }
    result
}

fn longest_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let mut prefix = strings[0].clone();
    while !prefix.is_empty() && strings.iter().any(|s| !s.starts_with(&prefix)) {
        prefix.pop();
    }
    prefix
}

enum CompletionResult {
    None,
    Replace(String),
    Listed,
}

fn input_token(input: &str) -> (&str, &str) {
    let start = input
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(i, ch)| i + ch.len_utf8())
        .unwrap_or(0);
    (&input[..start], &input[start..])
}

fn command_position(before: &str) -> bool {
    before
        .rsplit('|')
        .next()
        .unwrap_or(before)
        .trim()
        .is_empty()
}

fn path_completion_matches(shell: &Shell, token: &str) -> Vec<String> {
    let (dir_part, leaf) = match token.rfind('/') {
        Some(i) => (&token[..=i], &token[i + 1..]),
        None => ("", token),
    };

    let list_path = if dir_part.is_empty() {
        shell.cwd.clone()
    } else if dir_part.starts_with('/') {
        normalize_path(dir_part)
    } else {
        shell.resolve(dir_part)
    };

    let mut matches = Vec::new();
    for entry in list_dir(&list_path) {
        if entry.starts_with(leaf) {
            let mut candidate = String::from(dir_part);
            candidate.push_str(&entry);
            matches.push(candidate);
        }
    }
    matches.sort();
    matches.dedup();
    matches
}

fn handle_tab_completion(shell: &mut Shell, input: &str, term: &mut TermBuffer) -> CompletionResult {
    let (before, token) = input_token(input);
    let is_command = command_position(before);

    let mut matches = if is_command && !token.contains('/') {
        shell
            .get_commands()
            .iter()
            .filter(|cmd| cmd.starts_with(token))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        path_completion_matches(shell, token)
    };

    if matches.is_empty() {
        return CompletionResult::None;
    }
    matches.sort();
    matches.dedup();

    if matches.len() == 1 {
        let mut replacement = matches[0].clone();
        if !replacement.ends_with('/') {
            replacement.push(' ');
        }
        return CompletionResult::Replace(format!("{}{}", before, replacement));
    }

    let common = longest_common_prefix(&matches);
    if common.len() > token.len() {
        return CompletionResult::Replace(format!("{}{}", before, common));
    }

    // Put candidates on their own line and restore the current prompt/input.
    term.write_bytes(b"\r\n");
    let mut msg = String::new();
    for (i, item) in matches.iter().enumerate() {
        if i > 0 {
            msg.push_str("  ");
        }
        msg.push_str(item);
    }
    term.push(&msg);
    term.prompt_line(&shell.prompt());
    term.write_bytes(input.as_bytes());
    CompletionResult::Listed
}

impl Shell {
    fn new() -> Self {
        let mut shell = Self {
            cwd: String::from("/"),
            old_cwd: String::from("/"),
            // Root contains the system apps today; `.` follows cwd.
            path: String::from("/:."),
            command_cache: None,
            jobs: Vec::new(),
            next_job_id: 1,
            env: Vec::new(),
            last_status: 0,
            should_exit: false,
        };
        shell.set_var("PATH", "/:.", true);
        shell.set_var("HOME", "/", true);
        shell.set_var("PWD", "/", true);
        shell.set_var("OLDPWD", "/", true);
        shell
    }

    fn get_var(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|v| v.name == name)
            .map(|v| v.value.as_str())
    }

    fn set_var(&mut self, name: &str, value: &str, exported: bool) {
        if let Some(v) = self.env.iter_mut().find(|v| v.name == name) {
            v.value.clear();
            v.value.push_str(value);
            v.exported |= exported;
        } else {
            self.env.push(EnvVar {
                name: name.to_string(),
                value: value.to_string(),
                exported,
            });
        }
        if name == "PATH" {
            self.path = value.to_string();
            self.invalidate_cache();
        }
    }

    fn export_name(&mut self, name: &str) -> bool {
        if let Some(v) = self.env.iter_mut().find(|v| v.name == name) {
            v.exported = true;
            true
        } else {
            false
        }
    }

    fn unset_var(&mut self, name: &str) {
        self.env.retain(|v| v.name != name);
        if name == "PATH" {
            self.path.clear();
            self.invalidate_cache();
        }
    }

    fn exported_env(&self) -> Vec<String> {
        self.env
            .iter()
            .filter(|v| v.exported)
            .map(|v| format!("{}={}\0", v.name, v.value))
            .collect()
    }

    fn alloc_job_id(&mut self) -> u32 {
        let id = self.next_job_id;
        self.next_job_id = self.next_job_id.wrapping_add(1).max(1);
        id
    }

    fn get_commands(&mut self) -> &Vec<String> {
        if self.command_cache.is_none() {
            let mut cmds = Vec::new();
            // Встроенные команды
            for b in BUILTINS {
                cmds.push(b.to_string());
            }
            // External commands from PATH. Relative PATH entries are resolved
            // against cwd, so `.` follows `cd` as users expect.
            for dir in self.path.split(':') {
                if dir.is_empty() {
                    continue;
                }
                let resolved = if dir.starts_with('/') {
                    normalize_path(dir)
                } else {
                    self.resolve(dir)
                };
                for f in list_dir(&resolved) {
                    // `ls` marks directories with a trailing slash; they are
                    // valid path completions but not executable command names.
                    if !f.ends_with('/') {
                        cmds.push(f);
                    }
                }
            }
            cmds.sort();
            cmds.dedup();
            self.command_cache = Some(cmds);
        }
        self.command_cache.as_ref().unwrap()
    }

    // Инвалидация кэша (вызывать при изменении PATH)
    fn invalidate_cache(&mut self) {
        self.command_cache = None;
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
            return file_exists(&full).then_some(full);
        }
        for dir in self.path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let base = if dir.starts_with('/') {
                normalize_path(dir)
            } else {
                self.resolve(dir)
            };
            let candidate = if base == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", base, name)
            };
            let candidate = normalize_path(&candidate);
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

fn expand_word(shell: &Shell, word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;

    // Leading ~ expansion (suppressed when parser marked it protected).
    if chars.first() == Some(&'~') && (chars.len() == 1 || chars.get(1) == Some(&'/')) {
        out.push_str(shell.get_var("HOME").unwrap_or("/"));
        i = 1;
    }

    while i < chars.len() {
        if chars[i] == PROTECTED {
            i += 1;
            if let Some(ch) = chars.get(i) {
                out.push(*ch);
                i += 1;
            }
            continue;
        }
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        i += 1;
        if i >= chars.len() {
            out.push('$');
            break;
        }
        if chars[i] == '?' {
            out.push_str(&shell.last_status.to_string());
            i += 1;
            continue;
        }

        let mut name = String::new();
        if chars[i] == '{' {
            i += 1;
            while i < chars.len() && chars[i] != '}' {
                name.push(chars[i]);
                i += 1;
            }
            if i < chars.len() && chars[i] == '}' {
                i += 1;
            }
        } else {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                name.push(chars[i]);
                i += 1;
            }
        }
        if name.is_empty() {
            out.push('$');
        } else if let Some(value) = shell.get_var(&name) {
            out.push_str(value);
        }
    }
    out
}

fn expand_cmd(shell: &Shell, cmd: &SimpleCmd) -> SimpleCmd {
    let args = cmd.args.iter().map(|a| expand_word(shell, a)).collect();
    let redirs = cmd
        .redirs
        .iter()
        .map(|r| Redir {
            fd: r.fd,
            kind: r.kind,
            target: match &r.target {
                RedirTarget::Path(p) => RedirTarget::Path(expand_word(shell, p)),
                RedirTarget::Fd(fd) => RedirTarget::Fd(*fd),
            },
        })
        .collect();
    SimpleCmd { args, redirs }
}

fn file_exists(path: &str) -> bool {
    File::open(path).is_ok()
}

fn is_directory(path: &str) -> bool {
    let mut p = String::from(path);
    p.push('\0');
    let mut buf = [0u8; 64];
    let n = unsafe { syscall::ls(p.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    n > 0 || path == "/"
}

// ---------------------------------------------------------------------------
// Redirection helpers (parsing itself lives in parser.rs)
// ---------------------------------------------------------------------------

struct RedirFds {
    stdin: i32,
    stdout: i32,
    stderr: i32,
    stderr_to_stdout: bool,
}

impl RedirFds {
    fn new() -> Self {
        Self { stdin: -1, stdout: -1, stderr: -1, stderr_to_stdout: false }
    }
}

fn close_if_open(fd: &mut i32) {
    if *fd >= 0 {
        unsafe { close(*fd as u32); }
        *fd = -1;
    }
}

fn open_redir_path(shell: &Shell, r: &Redir, path: &str) -> Result<i32, String> {
    let full = shell.resolve(path);
    let mut cpath = full;
    cpath.push('\0');
    let flags = match r.kind {
        RedirKind::In => O_RDONLY,
        RedirKind::Out => O_WRONLY | O_CREAT | O_TRUNC,
        RedirKind::Append => O_WRONLY | O_CREAT | O_APPEND,
    };
    let fd = unsafe { open(cpath.as_ptr(), flags) };
    if fd == usize::MAX {
        Err(format!("{}: cannot open", path))
    } else {
        Ok(fd as i32)
    }
}

fn prepare_redirs(shell: &Shell, redirs: &[Redir]) -> Result<RedirFds, String> {
    let mut fds = RedirFds::new();
    for r in redirs {
        match &r.target {
            RedirTarget::Path(path) => {
                let fd = open_redir_path(shell, r, path)?;
                match r.fd {
                    0 => { close_if_open(&mut fds.stdin); fds.stdin = fd; }
                    1 => { close_if_open(&mut fds.stdout); fds.stdout = fd; }
                    2 => { close_if_open(&mut fds.stderr); fds.stderr = fd; fds.stderr_to_stdout = false; }
                    _ => {
                        unsafe { close(fd as u32); }
                        return Err(format!("redirection: fd {} is not supported", r.fd));
                    }
                }
            }
            RedirTarget::Fd(target) => {
                if r.fd == 2 && *target == 1 {
                    close_if_open(&mut fds.stderr);
                    fds.stderr_to_stdout = true;
                } else {
                    return Err(format!("redirection: {}>&{} is not supported", r.fd, target));
                }
            }
        }
    }
    Ok(fds)
}

/// Compatibility helper for builtins. Builtins currently write one output
/// stream, so fd 1 is enough; fd 0/2 are still parsed and opened/closed safely.
fn open_redirs(shell: &Shell, redirs: &[Redir]) -> Result<(i32, i32), String> {
    let mut fds = prepare_redirs(shell, redirs)?;
    // Current builtins do not consume redirected stdin/stderr themselves.
    close_if_open(&mut fds.stdin);
    close_if_open(&mut fds.stderr);
    Ok((-1, fds.stdout))
}

// ---------------------------------------------------------------------------
// Terminal buffer
// ---------------------------------------------------------------------------

const MAX_HISTORY: usize = 64;
const TERM_PAD: i32 = 8;

pub struct TermBuffer {
    inner: Terminal,
    /// Last rendered row strings (label path / tab completion).
    cache: Vec<String>,
}

impl TermBuffer {
    fn fit(win: &Window) -> (usize, usize) {
        let w = win.client_width() as i32;
        let h = win.client_height() as i32;
        let cols = ((w - TERM_PAD * 2) / CELL_W).max(1) as usize;
        let rows = ((h - TERM_PAD * 2) / CELL_H).max(1) as usize;
        (cols, rows)
    }

    fn resize_to(&mut self, win: &Window) {
        let (cols, rows) = Self::fit(win);
        self.inner.resize(cols, rows);
    }

    fn new(win: &Window) -> Self {
        let (cols, rows) = Self::fit(win);
        Self {
            inner: Terminal::new(cols, rows),
            cache: Vec::new(),
        }
    }

    fn push(&mut self, line: &str) {
        // Keep `\n` as line break; VTE handles wrap + CSI.
        self.inner.write_str(line);
        if !line.ends_with('\n') {
            self.inner.process(b"\r\n");
        }
        self.cache = self.inner.visible_lines();
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.inner.process(bytes);
        self.cache = self.inner.visible_lines();
    }

    fn clear(&mut self) {
        self.inner.clear();
        self.cache = self.inner.visible_lines();
    }

    fn visible_history(&self) -> impl Iterator<Item = &str> {
        self.cache.iter().map(|s| s.as_str())
    }

    fn draw(&self, win: &mut Window) {
        self.inner
            .draw(win, embedded_graphics::prelude::Point::new(TERM_PAD, TERM_PAD));
    }

    fn prompt_line(&mut self, prompt: &str) {
        self.inner.write_str(prompt);
        self.cache = self.inner.visible_lines();
    }

    fn rubout_n(&mut self, n: usize) {
        for _ in 0..n {
            self.write_bytes(b"\x08 \x08");
        }
    }

    fn scroll(&mut self, delta: i32) {
        self.inner.scroll(delta);
    }
}

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

fn try_builtin(shell: &mut Shell, cmd: &SimpleCmd, out: &mut TermBuffer) -> bool {
    let name = cmd.args[0].as_str();
    match name {
        "help" | "exit" | "quit" | "pwd" | "cd" | "ls" | "cat" | "mkdir" | "rmdir" | "rm"
        | "path" | "ps" | "jobs" | "fg" | "bg" | "kill" | "wait" | "export" | "unset" | "env"
        | "set" | "clear" | "echo" | "head" | "lspci" | "ifconfig" | "mount" | "mounts" | "umount" => {}
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
                    out.write_bytes(line.as_bytes());
                    out.write_bytes(b"\n");
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
            shell.should_exit = true;
            shell.last_status = cmd.args.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        },
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
            let target_owned = match cmd.args.get(1).map(|s| s.as_str()) {
                None => shell.get_var("HOME").unwrap_or("/").to_string(),
                Some("-") => shell.old_cwd.clone(),
                Some(v) => v.to_string(),
            };
            let new_cwd = shell.resolve(&target_owned);
            if is_directory(&new_cwd) {
                let mut cpath = new_cwd.clone();
                cpath.push('\0');
                if unsafe { chdir(cpath.as_ptr()) } == 0 {
                    let previous = shell.cwd.clone();
                    shell.old_cwd = previous.clone();
                    shell.cwd = new_cwd;
                    shell.set_var("OLDPWD", &previous, true);
                    let now = shell.cwd.clone();
                    shell.set_var("PWD", &now, true);
                    shell.invalidate_cache();
                    if cmd.args.get(1).map(|s| s.as_str()) == Some("-") {
                        out.push(&shell.cwd);
                    }
                } else {
                    out.push(&format!("cd: {}: chdir failed", target_owned));
                    shell.last_status = 1;
                }
            } else {
                out.push(&format!("cd: {}: No such directory", target_owned));
                shell.last_status = 1;
            }
            if file_fd >= 0 {
                unsafe { close(file_fd as u32); }
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
        "head" => {
            if let Some(file) = cmd.args.get(1) {
                head_to(&shell.resolve(file), file_fd, 20, out);
            } else {
                out.push("Usage: head <file>");
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
                    mkdir(path.as_ptr());
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
                    rmdir(path.as_ptr());
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
                    unlink(path.as_ptr());
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
                shell.set_var("PATH", new_path, true);
                out.push(&format!("PATH={}", shell.path));
            } else {
                out.push(&shell.path);
            }
            if file_fd >= 0 { unsafe { close(file_fd as u32); } }
        }
        "ps" => {
            ps_to(file_fd, out);
            if file_fd >= 0 { unsafe { close(file_fd as u32); } }
        }
        "mount" => {
            if cmd.args.len() == 1 {
                mounts_to(file_fd, out);
                shell.last_status = 0;
            } else if cmd.args.len() == 3 {
                let source = cmd.args[1].clone();
                let target = shell.resolve(&cmd.args[2]);
                let mut csource = source.clone();
                csource.push('\0');
                let mut ctarget = target.clone();
                ctarget.push('\0');
                let rc = unsafe {
                    mount(
                        csource.as_ptr(),
                        ctarget.as_ptr(),
                        core::ptr::null(),
                        0,
                        core::ptr::null(),
                    )
                };
                if rc != 0 {
                    out.push(&format!("mount: {} on {} failed", source, target));
                    shell.last_status = 1;
                } else {
                    out.push(&format!("{} mounted on {}", source, target));
                    shell.last_status = 0;
                }
            } else {
                out.push("Usage: mount [<device> <mountpoint>]");
                shell.last_status = 1;
            }
            if file_fd >= 0 { unsafe { close(file_fd as u32); } }
        }
        "mounts" => {
            mounts_to(file_fd, out);
            if file_fd >= 0 { unsafe { close(file_fd as u32); } }
        }
        "umount" => {
            if let Some(path) = cmd.args.get(1) {
                let full = shell.resolve(path);
                let mut cpath = full.clone();
                cpath.push('\0');
                let rc = unsafe { umount2(cpath.as_ptr(), 0) };
                if rc != 0 {
                    out.push(&format!("umount: {} failed", full));
                    shell.last_status = 1;
                } else {
                    shell.last_status = 0;
                }
            } else {
                out.push("Usage: umount <mountpoint>");
                shell.last_status = 1;
            }
            if file_fd >= 0 { unsafe { close(file_fd as u32); } }
        }
        "jobs" => {
            if shell.jobs.is_empty() {
                if file_fd >= 0 {
                    let msg = b"No background jobs\n";
                    unsafe { write(file_fd as u32, msg.as_ptr(), msg.len()); }
                } else {
                    out.push("No background jobs");
                }
            } else {
                for job in &shell.jobs {
                    let state = if job.state == JobState::Stopped { "Stopped" } else { "Running" };
                    let mut line = format!("[{}] {}  {}", job.id, state, job.command);
                    if file_fd >= 0 {
                        line.push('\n');
                        unsafe { write(file_fd as u32, line.as_bytes().as_ptr(), line.len()); }
                    } else {
                        out.push(&line);
                    }
                }
            }
            if file_fd >= 0 {
                unsafe { close(file_fd as u32); }
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
        "lspci" => {
            lspci_to(file_fd, out);
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        "ifconfig" => {
            ifconfig_to(&cmd.args, file_fd, out);
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

fn mounts_to(file_fd: i32, out: &mut TermBuffer) {
    let total = unsafe { mount_list(core::ptr::null_mut(), 0) };
    let mut items = alloc::vec![syscall::MountInfo::default(); total];
    let n = if items.is_empty() { 0 } else { unsafe { mount_list(items.as_mut_ptr(), items.len()) } };
    for item in items.iter().take(n) {
        let end = item.path.iter().position(|b| *b == 0).unwrap_or(item.path.len());
        let path = core::str::from_utf8(&item.path[..end]).unwrap_or("?");
        if file_fd >= 0 {
            let mut line = path.to_string();
            line.push('\n');
            unsafe { let _ = write(file_fd as u32, line.as_ptr(), line.len()); }
        } else {
            out.push(path);
        }
    }
}

fn ps_to(file_fd: i32, out: &mut TermBuffer) {
    let total = unsafe { task_list(core::ptr::null_mut(), 0) };
    let mut items = alloc::vec![syscall::TaskInfo::default(); total];
    let n = if items.is_empty() {
        0
    } else {
        unsafe { task_list(items.as_mut_ptr(), items.len()) }
    };

    let emit = |line: &str, out: &mut TermBuffer| {
        if file_fd >= 0 {
            let mut text = line.to_string();
            text.push('\n');
            unsafe { write(file_fd as u32, text.as_ptr(), text.len()); }
        } else {
            out.push(line);
        }
    };

    emit("PID  PPID  PGID  SID   TTY  STATE    EXIT  NAME             CWD", out);
    for item in items.iter().take(n) {
        let state = match item.state {
            TASK_RUNNING => "RUN",
            TASK_STOPPED => "STOP",
            TASK_ZOMBIE => "ZOMBIE",
            _ => "?",
        };
        let end = item.name.iter().position(|b| *b == 0).unwrap_or(item.name.len());
        let name = core::str::from_utf8(&item.name[..end]).unwrap_or("?");
        let cwd_end = item.cwd.iter().position(|b| *b == 0).unwrap_or(item.cwd.len());
        let cwd = core::str::from_utf8(&item.cwd[..cwd_end]).unwrap_or("?");
        let tty = if item.tty_id < 0 { "-".to_string() } else { item.tty_id.to_string() };
        emit(
            &format!(
                "{:<4} {:<5} {:<5} {:<5} {:<4} {:<8} {:<5} {:<16} {}",
                item.pid, item.ppid, item.pgid, item.sid, tty, state, item.exit_code, name, cwd
            ),
            out,
        );
    }
}

fn parse_ipv4(s: &str) -> Option<u32> {
    let mut parts = s.split('.');
    let a: u8 = parts.next()?.parse().ok()?;
    let b: u8 = parts.next()?.parse().ok()?;
    let c: u8 = parts.next()?.parse().ok()?;
    let d: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(u32::from_be_bytes([a, b, c, d]))
}

fn fmt_ipv4(v: u32) -> String {
    let o = v.to_be_bytes();
    format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3])
}

fn ifconfig_push(file_fd: i32, out: &mut TermBuffer, line: &str) {
    if file_fd >= 0 {
        let mut b = String::from(line);
        b.push('\n');
        unsafe {
            write(file_fd as u32, b.as_bytes().as_ptr(), b.len());
        }
    } else {
        out.push(line);
    }
}

fn ifconfig_show(file_fd: i32, out: &mut TermBuffer) {
    use syscall::{IfConfig, IFCFG_GET, IF_MODE_DHCP, IF_MODE_STATIC, IF_STATE_CONFIGURING, IF_STATE_UP};
    let mut cfg = IfConfig::default();
    let rc = unsafe { syscall::ifconfig(IFCFG_GET, &mut cfg as *mut _) };
    if rc == usize::MAX {
        ifconfig_push(file_fd, out, "ifconfig: no nic");
        return;
    }
    let mode = match cfg.mode {
        IF_MODE_STATIC => "static",
        IF_MODE_DHCP => "dhcp",
        _ => "none",
    };
    let state = match cfg.state {
        IF_STATE_UP => "up",
        IF_STATE_CONFIGURING => "configuring",
        _ => "down",
    };

    ifconfig_push(file_fd, out, &format!(" eth0  {}", state));
    ifconfig_push(file_fd, out, &format!(" inet {}/{}", fmt_ipv4(cfg.ip), cfg.prefix));
    ifconfig_push(file_fd, out, &format!(" gw {}", fmt_ipv4(cfg.gateway)));
    ifconfig_push(file_fd, out, &format!(" dns {}", fmt_ipv4(cfg.dns)));
    ifconfig_push(file_fd, out, &format!(" ether {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", cfg.mac[0], cfg.mac[1], cfg.mac[2], cfg.mac[3], cfg.mac[4], cfg.mac[5]));
    ifconfig_push(file_fd, out, &format!(" mode {}", mode));
}

fn ifconfig_to(args: &[String], file_fd: i32, out: &mut TermBuffer) {
    use syscall::{IfConfig, IFCFG_DHCP, IFCFG_GET, IFCFG_STATIC, IF_STATE_UP};
    if args.len() == 1 {
        ifconfig_show(file_fd, out);
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("dhcp") {
        let rc = unsafe { syscall::ifconfig(IFCFG_DHCP, core::ptr::null_mut()) };
        if rc == usize::MAX {
            ifconfig_push(file_fd, out, "ifconfig: dhcp failed");
            return;
        }
        ifconfig_push(file_fd, out, "dhcp started");
        for _ in 0..40 {
            unsafe {
                syscall::sys_sleep(100);
            }
            let mut cfg = IfConfig::default();
            let _ = unsafe { syscall::ifconfig(IFCFG_GET, &mut cfg as *mut _) };
            if cfg.state == IF_STATE_UP {
                ifconfig_show(file_fd, out);
                return;
            }
        }
        ifconfig_push(file_fd, out, "dhcp: still configuring (check ifconfig)");
        return;
    }
    let spec = args[1].as_str();
    let (ip_s, pfx_s) = spec.split_once('/').unwrap_or((spec, "24"));
    let Some(ip) = parse_ipv4(ip_s) else {
        ifconfig_push(file_fd, out, "Usage: ifconfig | ifconfig dhcp | ifconfig IP/PREFIX [GW]");
        return;
    };
    let Ok(prefix) = pfx_s.parse::<u32>() else {
        ifconfig_push(file_fd, out, "bad prefix");
        return;
    };
    let gw = args.get(2).and_then(|s| parse_ipv4(s)).unwrap_or(0);
    let mut cfg = IfConfig {
        ip,
        prefix,
        gateway: gw,
        ..IfConfig::default()
    };
    let rc = unsafe { syscall::ifconfig(IFCFG_STATIC, &mut cfg as *mut _) };
    if rc == usize::MAX {
        ifconfig_push(file_fd, out, "ifconfig: static failed");
        return;
    }
    ifconfig_show(file_fd, out);
}

/// List PCI devices; names from the [pci-ids](https://docs.rs/pci-ids) database.
fn lspci_to(file_fd: i32, out: &mut TermBuffer) {
    use pci_ids::{Device, FromId, Subclass, Vendor};

    let total = unsafe { syscall::pci_list(core::ptr::null_mut(), 0) };
    if total == 0 {
        out.push("lspci: no PCI devices found");
        return;
    }
    let mut buf = alloc::vec![syscall::PciInfo::default(); total];
    let n = unsafe { syscall::pci_list(buf.as_mut_ptr(), buf.len()) };
    out.push(&format!("=== PCI Devices ({} found) ===", n));

    for d in buf.iter().take(n) {
        let vendor = Vendor::from_id(d.vendor_id)
            .map(|v| v.name())
            .unwrap_or("Unknown vendor");
        let device = Device::from_vid_pid(d.vendor_id, d.device_id)
            .map(|dev| dev.name())
            .unwrap_or("Unknown device");
        let class = Subclass::from_cid_sid(d.class_code, d.subclass)
            .map(|s| s.name())
            .unwrap_or("Unknown class");

        let line = format!(
            "{:02x}:{:02x}.{}  [{:04x}:{:04x}]  {} | {} | {} ",
            d.bus, d.device, d.function, d.vendor_id, d.device_id, vendor, device, class,
        );
        if file_fd >= 0 {
            let mut b = line.clone();
            b.push('\n');
            unsafe {
                write(file_fd as u32, b.as_bytes().as_ptr(), b.len());
            }
        } else {
            out.push(&line);
        }
    }
    out.push("==============================");
}

fn ls_to(path: &str, file_fd: i32, out: &mut TermBuffer) {
    let mut path_buf = String::from(path);
    if path_buf.is_empty() {
        path_buf.push('/');
    }
    path_buf.push('\0');
    let mut buf = [0u8; 4096];
    let n = unsafe { syscall::ls(path_buf.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    if n == 0 {
        out.push(&format!("ls: cannot read directory: {}", path));
        return;
    }
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    if file_fd < 0 {
        let mut lines = String::new();
        for entry in text.lines() {
            lines.push_str(entry);
            lines.push_str(" ");
        }
        out.push(lines.as_str());
        return;
    }
    for entry in text.lines() {
        if entry.is_empty() {
            continue;
        }
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

fn head_to(filename: &str, file_fd: i32, count: u32, out: &mut TermBuffer) {
    let mut path = String::from(filename);
    path.push('\0');
    let fd = unsafe { open(path.as_ptr(), O_RDONLY) };
    if fd == usize::MAX {
        out.push(&format!("File not found: {}", filename));
        return;
    }
    let mut remain = count as usize;
    let mut buf = [0u8; 4096];
    while remain > 0 {
        let n = unsafe { read(fd as u32, buf.as_mut_ptr(), min(remain, 4096)) };
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
        remain = remain.saturating_sub(n);
    }
    unsafe {
        close(fd as u32);
    }
}

fn cat_to(filename: &str, file_fd: i32, out: &mut TermBuffer) {
    let mut path = String::from(filename);
    path.push('\0');
    let fd = unsafe { open(path.as_ptr(), O_RDONLY) };
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

fn help_text() -> String {
    String::from(
        "Builtins:\n\
  ls [path]        - list directory\n\
  cat <file>       - display file content\n\
  cd [dir]         - change directory\n\
  pwd              - print working directory\n\
  path [dirs]      - show or set PATH\n\
  mkdir / rmdir / rm\n\
  lspci            - list PCI devices (pci-ids names)\n\
  ifconfig         - show iface\n\
  ifconfig dhcp    - DHCP\n\
  ifconfig IP/PFX [GW]  - static\n\
  jobs / fg / bg / wait / kill - job control\n\
  mount            - list VFS mount points\n\
  mount /dev/X /mnt/Y - mount ext2/FAT block device\n\
  mounts           - list VFS mount points\n\
  umount <path>    - unmount removable filesystem\n\
  clear            - clear terminal\n\
  help / exit\n\n\
Tab completes commands and paths relative to cwd.\n\
cmd &              - run in background\n\
cmd > file         - redirect stdout and detach (Felix convenience)\n\
cmd >> file        - append stdout and detach\n\
Pipes run in foreground.\n\
Ctrl+C interrupts a foreground program (userspace).\n",
    )
}

#[cfg(any())]
mod legacy_execution {
use super::*;
// ---------------------------------------------------------------------------
// Legacy execution path kept out of the build while the new executor.rs owns
// pipelines/job control. It remains here temporarily to make the transition
// easy to inspect and can be deleted after testing.
// ---------------------------------------------------------------------------

enum UiTick {
    None,
    Interrupt,
    /// Bytes to push into the child's stdin pipe.
    Data([u8; 8], usize),
}

const SCAN_ESC: u8 = 0x01;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;

fn map_child_key(scancode: u8, ch: u8, mods: u8) -> ([u8; 8], usize) {
    let ctrl = (mods & 2) != 0;
    if ch == 0x03 || (scancode == 0x2e && ctrl) {
        return ([0x03, 0, 0, 0, 0, 0, 0, 0], 1);
    }
    if ctrl && ch >= b'a' && ch <= b'z' {
        return ([ch - b'a' + 1, 0, 0, 0, 0, 0, 0, 0], 1);
    }
    match scancode {
        SCAN_ENTER => ([b'\r', 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_BACKSPACE => ([0x7f, 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_TAB => ([b'\t', 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_ESC => ([0x1b, 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_UP => ([0x1b, b'[', b'A', 0, 0, 0, 0, 0], 3),
        SCAN_DOWN => ([0x1b, b'[', b'B', 0, 0, 0, 0, 0], 3),
        SCAN_RIGHT => ([0x1b, b'[', b'C', 0, 0, 0, 0, 0], 3),
        SCAN_LEFT => ([0x1b, b'[', b'D', 0, 0, 0, 0, 0], 3),
        _ if ch >= 0x20 && ch < 0x7f => ([ch, 0, 0, 0, 0, 0, 0, 0], 1),
        _ => ([0; 8], 0),
    }
}

fn spawn(
    path: &str,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    args: &[String],
) -> Option<i32> {
    let mut f = File::open(path).ok()?;
    let data = f.read_to_end().ok()?;
    let mut c_strings: Vec<String> = Vec::new();
    if args.is_empty() {
        let mut s = String::from(path);
        s.push('\0');
        c_strings.push(s);
    } else {
        for a in args {
            let mut s = a.clone();
            s.push('\0');
            c_strings.push(s);
        }
    }
    let ptrs: Vec<*const u8> = c_strings.iter().map(|s| s.as_ptr()).collect();
    if data.len() < 4 {
        println!("Not executable file");
        return None;
    }
    unsafe {
        let pid = match &data[0..4] {
            &[0x0, 0x61, 0x73, 0x6d] => execve_wasm(
                data.as_ptr(),
                data.len(),
                stdin_fd,
                stdout_fd,
                stderr_fd,
                &ptrs,
            ),
            b"\x7fELF" => execve(
                data.as_ptr(),
                data.len(),
                stdin_fd,
                stdout_fd,
                stderr_fd,
                &ptrs,
            ),
            _ => {
                println!("Not executable file");
                usize::MAX
            }
        };
        if pid == usize::MAX {
            None
        } else {
            Some(pid as i32)
        }
    }
}

/// Non-blocking pipe drain into the VT screen. Returns true if any data was consumed.
fn drain_pipe_once(
    fd: u32,
    out: &mut TermBuffer,
    _partial: &mut String,
    _live_idx: &mut Option<usize>,
) -> bool {
    let mut buf = [0u8; 512];
    let n = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
    if n == 0 || n == usize::MAX {
        return false;
    }
    out.write_bytes(&buf[..n]);
    true
}

/// UI bridge: one mutable owner of the window so tick + redraw don't conflict.
struct UiBridge<'a> {
    win: &'a mut Window,
    prompt: String,
}

impl UiBridge<'_> {
    fn poll_keys(&mut self) -> UiTick {
        let mut evbuf = [WmEvent::default(); 32];
        let n = self.win.poll_events(&mut evbuf);
        for e in &evbuf[..n] {
            if e.kind == EV_RESIZE {
                continue;
            }
            if e.kind != EV_KEY_DOWN {
                continue;
            }
            let ch = e.b as u8;
            let sc = e.a as u8;
            let mods = e.c as u8;
            if ch == 0x03 || (sc == 0x2e && (mods & 2) != 0) {
                return UiTick::Interrupt;
            }
            let (buf, n) = map_child_key(sc, ch, mods);
            if n > 0 {
                return UiTick::Data(buf, n);
            }
        }
        UiTick::None
    }

    fn redraw(&mut self, term: &TermBuffer) {
        refresh_terminal(self.win, term);
        let _ = self.win.flip();
    }
}

/// Ctrl+C in this window → kill(child, SIGINT).
fn supervise_child(
    pid: i32,
    capture_fd: Option<u32>,
    stdin_w: Option<u32>,
    out: &mut TermBuffer,
    ui: &mut UiBridge<'_>,
) {
    if let Some(fd) = capture_fd {
        let _ = unsafe { set_nonblock(fd) };
    }

    let mut partial = String::new();
    let mut live_idx: Option<usize> = None;
    let mut done = false;
    let mut sent_sigint = false;

    while !done {
        if let Some(fd) = capture_fd {
            let mut any = false;
            while drain_pipe_once(fd, out, &mut partial, &mut live_idx) {
                any = true;
            }
            if any {
                ui.redraw(out);
            }
        }

        match ui.poll_keys() {
            UiTick::Interrupt if !sent_sigint => {
                unsafe {
                    let _ = kill(pid, SIGINT);
                }
                if let Some(w) = stdin_w {
                    let b = [0x03u8];
                    unsafe {
                        let _ = write(w, b.as_ptr(), 1);
                    }
                }
                out.push("^C");
                ui.redraw(out);
                sent_sigint = true;
            }
            UiTick::Data(buf, n) => {
                if let Some(w) = stdin_w {
                    unsafe {
                        let _ = write(w, buf.as_ptr(), n);
                    }
                }
            }
            _ => {}
        }

        let w = unsafe { wait_options(pid, WNOHANG) };
        if w == pid as usize || w == usize::MAX {
            done = true;
        }

        if !done {
            block_on_yield();
        }
    }

    if let Some(fd) = capture_fd {
        loop {
            if !drain_pipe_once(fd, out, &mut partial, &mut live_idx) {
                break;
            }
            ui.redraw(out);
        }
        if !partial.is_empty() {
            if let Some(i) = live_idx {
                if i < out.cache.len() {
                    let _ = i;
                }
            } else {
                out.push(&partial);
            }
            ui.redraw(out);
        }
        unsafe {
            close(fd);
        }
    }
    if let Some(w) = stdin_w {
        unsafe {
            close(w);
        }
    }
}

/// Tiny yield without pulling Executor into every call site.
fn block_on_yield() {
    // One Pending cycle via our runtime helper.
    libfelix::async_rt::block_on(async {
        yield_now().await;
    });
}

fn run_external(
    shell: &Shell,
    cmd: &SimpleCmd,
    forced_in: i32,
    forced_out: i32,
    background: bool,
    out: &mut TermBuffer,
    ui: &mut UiBridge<'_>,
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
            unsafe { close(sin as u32); }
        }
        sin = forced_in;
    }
    if forced_out >= 0 {
        if sout >= 0 {
            unsafe { close(sout as u32); }
        }
        sout = forced_out;
    }

    // A child without explicit stdin gets a private pipe. Foreground jobs are
    // fed keyboard data by supervise_child(); background jobs get EOF when the
    // shell closes its writer, so they cannot steal terminal input.
    let mut stdin_r: i32 = -1;
    let mut stdin_w: i32 = -1;
    if sin < 0 {
        let mut fds = [0u32; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } == 0 {
            stdin_r = fds[0] as i32;
            stdin_w = fds[1] as i32;
            sin = stdin_r;
        }
    }

    let mut capture_r: i32 = -1;
    let mut capture_w: i32 = -1;
    let mut serr: i32 = -1;

    if !background {
        // Foreground stdout/stderr are bridged into this GUI terminal. If
        // stdout is redirected to a file, only stderr is captured.
        let mut fds = [0u32; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } == 0 {
            capture_r = fds[0] as i32;
            capture_w = fds[1] as i32;
            if sout < 0 {
                sout = capture_w;
            }
            serr = capture_w;
        }
    }
    // Background output must never go to an undrained anonymous pipe: a noisy
    // task would block as soon as PIPE_BUF_SIZE fills. Explicit stdout
    // redirection is kept; otherwise stdout/stderr use their default console.

    let pid = spawn(&full, sin, sout, serr, &cmd.args);

    if capture_w >= 0 {
        unsafe { close(capture_w as u32); }
    }

    // Close only descriptors owned by this invocation. Pipeline fds supplied
    // through forced_in/forced_out are owned by run_pipeline().
    if stdin_r >= 0 {
        unsafe { close(stdin_r as u32); }
    } else if sin >= 0 && forced_in < 0 {
        unsafe { close(sin as u32); }
    }
    if sout >= 0 && forced_out < 0 && sout != capture_w {
        unsafe { close(sout as u32); }
    }

    let Some(p) = pid else {
        if capture_r >= 0 {
            unsafe { close(capture_r as u32); }
        }
        if stdin_w >= 0 {
            unsafe { close(stdin_w as u32); }
        }
        return None;
    };

    if background {
        if capture_r >= 0 {
            unsafe { close(capture_r as u32); }
        }
        if stdin_w >= 0 {
            unsafe { close(stdin_w as u32); }
        }
        return Some(p);
    }

    let cap = if capture_r >= 0 {
        Some(capture_r as u32)
    } else {
        None
    };
    let kin = if stdin_w >= 0 {
        Some(stdin_w as u32)
    } else {
        None
    };
    supervise_child(p, cap, kin, out, ui);
    None
}

fn split_background(line: &str) -> (&str, bool) {
    let trimmed = line.trim_end();
    if let Some(body) = trimmed.strip_suffix('&') {
        (body.trim_end(), true)
    } else {
        (trimmed, false)
    }
}

fn has_stdout_redirection(cmd: &SimpleCmd) -> bool {
    cmd.redirs
        .iter()
        .any(|r| r.kind == RedirKind::Out || r.kind == RedirKind::Append)
}

fn reap_background_jobs(shell: &mut Shell) {
    let mut i = 0;
    while i < shell.jobs.len() {
        let pid = shell.jobs[i].pid;
        let done = unsafe { wait_options(pid, WNOHANG) } == pid as usize;
        if done {
            shell.jobs.remove(i);
        } else {
            i += 1;
        }
    }
}

fn run_pipeline(shell: &Shell, stages: &[String], out: &mut TermBuffer, ui: &mut UiBridge<'_>) {
    let n = stages.len();
    if n == 0 {
        return;
    }

    let mut pipes: Vec<(u32, u32)> = Vec::new();
    for _ in 0..n.saturating_sub(1) {
        let mut fds = [0u32; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
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
        let in_fd: i32 = if i == 0 { -1 } else { pipes[i - 1].0 as i32 };
        let out_fd: i32 = if i + 1 == n { -1 } else { pipes[i].1 as i32 };
        let is_last = i + 1 == n;

        // Only the last stage gets live UI supervision + capture.
        if is_last {
            let _ = run_external(shell, &cmd, in_fd, -1, false, out, ui);
        } else {
            // Intermediate stages: fire-and-forget wait after all spawned.
            let name = cmd.args[0].as_str();
            if let Some(full) = shell.find_executable(name) {
                if let Some(p) = spawn(&full, in_fd, out_fd, -1, &cmd.args) {
                    pids.push(p);
                }
            }
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

fn interpret(shell: &mut Shell, line: &str, out: &mut TermBuffer, ui: &mut UiBridge<'_>) {
    let (line, background_requested) = split_background(line);
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

        // Felix shell convenience: an external command with stdout redirected
        // to a file is detached automatically. `cmd > file &` is also accepted.
        let background = background_requested || has_stdout_redirection(&cmd);
        if let Some(pid) = run_external(shell, &cmd, -1, -1, background, out, ui) {
            shell.jobs.push(BackgroundJob {
                pid,
                command: line.to_string(),
            });
            out.push(&format!("[{}] started", pid));
        }
        return;
    }

    if background_requested {
        out.push("background pipelines are not supported yet");
        return;
    }
    run_pipeline(shell, &stages, out, ui);
}

}

// ---------------------------------------------------------------------------
// GUI terminal
// ---------------------------------------------------------------------------

const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_A: u8 = 0x1E;
const SCAN_D: u8 = 0x20;
const SCAN_E: u8 = 0x12;
const SCAN_U: u8 = 0x16;
const SCAN_W: u8 = 0x11;
const SCAN_K: u8 = 0x25;
const SCAN_C: u8 = 0x2E;
const SCAN_Z: u8 = 0x2C;
const SCAN_HOME: u8 = 0x47;
const SCAN_UP: u8 = 0x48;
const SCAN_PGUP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PGDN: u8 = 0x51;
const SCAN_DELETE: u8 = 0x53;
const MAX_INPUT: usize = 1024;
const CMD_HISTORY_MAX: usize = 64;
const LINE_MAX_CHARS: usize = 69; // unused for wrap; VT reflows by window cols
const HIST_PATH: &str = "/shell_hist";

fn load_cmd_history() -> Vec<String> {
    let mut f = match File::open_ro(HIST_PATH) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let data = f.read_to_end().unwrap_or_default();
    let text = core::str::from_utf8(&data).unwrap_or("");
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
    }
    if out.len() > CMD_HISTORY_MAX {
        out.drain(0..out.len() - CMD_HISTORY_MAX);
    }
    out
}

fn save_cmd_history(hist: &[String]) {
    let mut f = match File::create(HIST_PATH) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut buf = String::new();
    for line in hist {
        buf.push_str(line);
        buf.push('\n');
    }
    let _ = f.write(buf.as_bytes());
}

/// Keep the **start** of the line (bus addr / prompt), not the tail.
fn truncate_line(s: &str) -> &str {
    s
}

fn refresh_terminal(win: &mut Window, term: &TermBuffer) {
    term.draw(win);
}

fn redraw_editor(term: &mut TermBuffer, shell: &Shell, editor: &LineEditor) {
    term.write_bytes(b"\r\x1b[2K");
    term.write_bytes(shell.prompt().as_bytes());
    term.write_bytes(editor.text().as_bytes());
    let back = editor.chars_after_cursor();
    if back > 0 {
        term.write_bytes(format!("\x1b[{}D", back).as_bytes());
    }
}

fn block_on_yield() {
    libfelix::async_rt::block_on(async { yield_now().await; });
}

const BUILTINS: &[&str] = &[
    "help", "exit", "quit", "pwd", "cd", "ls", "cat", "mkdir", "rmdir", "rm", "path", "ps",
    "jobs", "fg", "bg", "kill", "wait", "export", "unset", "env", "set", "clear", "echo",
    "head", "lspci", "ifconfig", "mount", "mounts", "umount",
];

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let shell_pid = unsafe { getpid() };
    println!("shell: userspace main started pid={}", shell_pid);

    let mut win = Window::create(30, 30, 640, 400, "Felix Shell").unwrap_or_else(|| {
        Window::create(40, 40, 480, 320, "Felix Shell").expect("wm_create failed")
    });

    let mut shell = Shell::new();
    // Become our own process group so foreground jobs can be handed the GUI tty
    // and the shell can reclaim it afterwards.
    let _ = unsafe { setpgid(0, shell_pid) };
    let shell_pgid = unsafe { getpgrp() };
    let _ = unsafe { tty_setfg(shell_pgid) };
    let mut term = TermBuffer::new(&win);
    let mut editor = LineEditor::new();
    let mut cmd_hist: Vec<String> = load_cmd_history();
    let mut hist_pos: Option<usize> = None;
    let mut draft = String::new();

    term.push("=== Felix User Shell ===");
    term.push("Tab paths · arrows edit/history · Ctrl+C/Z · Ctrl+A/E/U/K/W · help");
    term.push("");
    redraw_editor(&mut term, &shell, &editor);
    refresh_terminal(&mut win, &term);
    let _ = win.flip();

    loop {
        let mut dirty = false;

        // Background pipelines are reaped continuously, not only when the next
        // command is entered. This is important with Felix's small task table.
        if poll_background_jobs(&mut shell, &mut term) {
            redraw_editor(&mut term, &shell, &editor);
            dirty = true;
        }

        let mut evbuf = [WmEvent::default(); 64];
        let n = win.poll_events(&mut evbuf);
        for e in &evbuf[..n] {
            if e.kind == EV_RESIZE {
                term.resize_to(&win);
                dirty = true;
                continue;
            }
            if e.kind != EV_KEY_DOWN {
                continue;
            }

            let scancode = e.a as u8;
            let ch = e.b as u8;
            let mods = e.c as u8;
            let ctrl = (mods & 2) != 0;

            // readline-like editing controls.
            let edited = if ctrl && scancode == SCAN_A {
                editor.home()
            } else if ctrl && scancode == SCAN_E {
                editor.end()
            } else if ctrl && scancode == SCAN_U {
                editor.kill_before()
            } else if ctrl && scancode == SCAN_K {
                editor.kill_after()
            } else if ctrl && scancode == SCAN_W {
                editor.delete_prev_word()
            } else {
                false
            };
            if edited {
                hist_pos = None;
                redraw_editor(&mut term, &shell, &editor);
                dirty = true;
                continue;
            }

            if ctrl && scancode == SCAN_C {
                if !editor.text().is_empty() {
                    editor.clear();
                    term.write_bytes(b"^C\r\n");
                    redraw_editor(&mut term, &shell, &editor);
                    hist_pos = None;
                    draft.clear();
                    dirty = true;
                }
                continue;
            }
            if ctrl && scancode == SCAN_D && editor.text().is_empty() {
                shell.last_status = 0;
                shell.should_exit = true;
            }
            if shell.should_exit {
                break;
            }

            match scancode {
                SCAN_ENTER => {
                    let cmd = editor.take();
                    let trimmed = cmd.trim();
                    if !trimmed.is_empty() && cmd_hist.last().map(|s| s.as_str()) != Some(trimmed) {
                        cmd_hist.push(trimmed.to_string());
                        if cmd_hist.len() > CMD_HISTORY_MAX {
                            cmd_hist.remove(0);
                        }
                        save_cmd_history(&cmd_hist);
                    }
                    hist_pos = None;
                    draft.clear();
                    term.write_bytes(b"\r\n");
                    refresh_terminal(&mut win, &term);
                    let _ = win.flip();
                    if !trimmed.is_empty() {
                        interpret(&mut shell, &cmd, &mut term, &mut win);
                    }
                    if shell.should_exit {
                        break;
                    }
                    redraw_editor(&mut term, &shell, &editor);
                    dirty = true;
                }
                SCAN_PGUP => {
                    term.scroll(8);
                    dirty = true;
                }
                SCAN_PGDN => {
                    term.scroll(-8);
                    dirty = true;
                }
                SCAN_LEFT => {
                    if editor.left() {
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_RIGHT => {
                    if editor.right() {
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_HOME => {
                    if editor.home() {
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_END => {
                    if editor.end() {
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_DELETE => {
                    if editor.delete() {
                        hist_pos = None;
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_BACKSPACE => {
                    if editor.backspace() {
                        hist_pos = None;
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_UP => {
                    if !cmd_hist.is_empty() {
                        let next = match hist_pos {
                            None => {
                                draft = editor.text().to_string();
                                cmd_hist.len() - 1
                            }
                            Some(0) => 0,
                            Some(i) => i - 1,
                        };
                        hist_pos = Some(next);
                        editor.set(cmd_hist[next].clone());
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_DOWN => {
                    if let Some(i) = hist_pos {
                        if i + 1 < cmd_hist.len() {
                            hist_pos = Some(i + 1);
                            editor.set(cmd_hist[i + 1].clone());
                        } else {
                            hist_pos = None;
                            editor.set(draft.clone());
                        }
                        redraw_editor(&mut term, &shell, &editor);
                        dirty = true;
                    }
                }
                SCAN_TAB => {
                    let cursor = editor.cursor();
                    let prefix = editor.text()[..cursor].to_string();
                    let suffix = editor.text()[cursor..].to_string();
                    match handle_tab_completion(&mut shell, &prefix, &mut term) {
                        CompletionResult::None => {}
                        CompletionResult::Replace(new_prefix) => {
                            let new_cursor = new_prefix.len();
                            let mut full = new_prefix;
                            full.push_str(&suffix);
                            editor.set_with_cursor(full, new_cursor);
                            redraw_editor(&mut term, &shell, &editor);
                            dirty = true;
                        }
                        CompletionResult::Listed => {
                            redraw_editor(&mut term, &shell, &editor);
                            dirty = true;
                        }
                    }
                }
                _ if ch >= 0x20 && ch < 0x7f && editor.text().len() < MAX_INPUT => {
                    editor.insert(ch as char);
                    hist_pos = None;
                    redraw_editor(&mut term, &shell, &editor);
                    dirty = true;
                }
                _ => {}
            }
        }

        if shell.should_exit {
            break;
        }
        if dirty {
            refresh_terminal(&mut win, &term);
            let _ = win.flip();
        } else {
            block_on_yield();
        }
    }

    shell.last_status
}
