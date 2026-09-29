#![no_std]
#![no_main]

extern crate alloc;

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::min;

use libfelix::prelude::*;
mod executor;
mod line_editor;
mod parser;
use executor::{interpret, poll_background_jobs, run_builtin_argv};
use line_editor::LineEditor;
use parser::{parse_line, CommandGroup, Connector, Redir, RedirKind, RedirTarget, SimpleCmd, PROTECTED};
use libfelix::syscall::{
    self, chdir, close, getcwd, getpid, getpgrp, kill, mkdir, mount, mount_list,
    open, pipe, read, rmdir, setpgid, task_list, tcsetpgrp, umount2, unlink,
    spawn_path_env_pgid, waitpid_status, write, O_APPEND, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, SIGCONT, SIGINT,
    SIGKILL, SIGSTOP, SIGTERM, SIGTSTP, TASK_RUNNING, TASK_STOPPED, TASK_ZOMBIE, WCONTINUED,
    WNOHANG, WUNTRACED,
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
        let mut cwd_buf = [0u8; 256];
        let cwd_len = unsafe { getcwd(cwd_buf.as_mut_ptr(), cwd_buf.len()) };
        let cwd = if cwd_len > 1 && (cwd_len as isize) > 0 && cwd_len <= cwd_buf.len() {
            core::str::from_utf8(&cwd_buf[..cwd_len - 1])
                .unwrap_or("/")
                .to_string()
        } else {
            String::from("/")
        };

        let mut shell = Self {
            cwd: cwd.clone(),
            old_cwd: cwd.clone(),
            path: String::new(),
            command_cache: None,
            jobs: Vec::new(),
            next_job_id: 1,
            env: Vec::new(),
            last_status: 0,
            should_exit: false,
        };

        // Preserve the process environment supplied by terminal/telnetd or a
        // parent shell. This is especially important for TERM, HOME and PATH.
        for entry in envs() {
            if let Some((name, value)) = entry.split_once('=') {
                shell.set_var(name, value, true);
            }
        }
        if shell.get_var("PATH").is_none() {
            shell.set_var("PATH", "/bin:.", true);
        }
        if shell.get_var("HOME").is_none() {
            shell.set_var("HOME", "/home/user", true);
        }
        shell.set_var("PWD", &cwd, true);
        shell.set_var("OLDPWD", &cwd, true);
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
// Shell output
// ---------------------------------------------------------------------------

fn write_all_fd(fd: u32, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let n = unsafe { write(fd, bytes.as_ptr(), bytes.len()) };
        if n == 0 || n == usize::MAX {
            break;
        }
        bytes = &bytes[n..];
    }
}

/// Minimal stdout abstraction kept so the existing builtins need only small
/// changes. It deliberately contains no terminal emulator or WM state.
pub struct TermBuffer;

impl TermBuffer {
    fn new() -> Self {
        Self
    }

    fn push(&mut self, line: &str) {
        write_all_fd(1, line.as_bytes());
        write_all_fd(1, b"\n");
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        write_all_fd(1, bytes);
    }

    fn clear(&mut self) {
        write_all_fd(1, b"\x1b[2J\x1b[H");
    }

    fn prompt_line(&mut self, prompt: &str) {
        write_all_fd(1, prompt.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

fn try_builtin(shell: &mut Shell, cmd: &SimpleCmd, out: &mut TermBuffer) -> bool {
    let name = cmd.args[0].as_str();
    match name {
        "help" | "exit" | "quit" | "pwd" | "cd" | "ls" | "cat" | "mkdir" | "rmdir" | "rm" | "mv"
        | "path" | "ps" | "jobs" | "fg" | "bg" | "kill" | "wait" | "export" | "unset" | "env"
        | "set" | "clear" | "reboot" | "echo" | "head" | "lspci" | "ifconfig" | "mount" | "mounts" | "umount"
        | "audiotest" => {}
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
                cat_stdin(file_fd, out);
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
                head_stdin(file_fd, 20, out);
            }
            if file_fd >= 0 {
                unsafe {
                    close(file_fd as u32);
                }
            }
        }
        "audiotest" => {
            audio_test(out);
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
        "mv" => {
            if let (Some(old), Some(new)) = (cmd.args.get(1), cmd.args.get(2)) {
                let mut old_path = shell.resolve(old);
                let mut new_path = shell.resolve(new);
                old_path.push('\0');
                new_path.push('\0');
                let rc = unsafe { syscall::rename(old_path.as_ptr(), new_path.as_ptr()) };
                if (rc as isize) < 0 {
                    out.push(&format!("mv: cannot rename {} to {} (errno {})", old, new, -(rc as isize)));
                    shell.last_status = 1;
                } else {
                    shell.last_status = 0;
                }
            } else {
                out.push("Usage: mv <old> <new>");
                shell.last_status = 1;
            }
            if file_fd >= 0 { unsafe { close(file_fd as u32); } }
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
        "reboot" => unsafe {
            syscall::reboot();
        },
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

fn write_builtin_bytes(file_fd: i32, bytes: &[u8], out: &mut TermBuffer) {
    if file_fd >= 0 {
        let mut off = 0usize;
        while off < bytes.len() {
            let n = unsafe { write(file_fd as u32, bytes[off..].as_ptr(), bytes.len() - off) };
            if n == 0 || n == usize::MAX {
                break;
            }
            off += n;
        }
    } else {
        out.write_bytes(bytes);
    }
}

fn cat_stdin(file_fd: i32, out: &mut TermBuffer) {
    let mut buf = [0u8; 512];
    loop {
        let n = unsafe { read(0, buf.as_mut_ptr(), buf.len()) };
        if n == 0 || n == usize::MAX {
            break;
        }
        write_builtin_bytes(file_fd, &buf[..n], out);
    }
}

fn head_stdin(file_fd: i32, count: u32, out: &mut TermBuffer) {
    let mut remain = count as usize;
    let mut buf = [0u8; 512];
    while remain > 0 {
        let want = core::cmp::min(remain, buf.len());
        let n = unsafe { read(0, buf.as_mut_ptr(), want) };
        if n == 0 || n == usize::MAX {
            break;
        }
        write_builtin_bytes(file_fd, &buf[..n], out);
        remain = remain.saturating_sub(n);
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
        write_builtin_bytes(file_fd, &buf[..n], out);
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
        write_builtin_bytes(file_fd, &buf[..n], out);
    }
    unsafe {
        close(fd as u32);
    }
}

fn audio_test(out: &mut TermBuffer) {
    let path = b"/dev/audio\0";
    let fd = unsafe { open(path.as_ptr(), O_WRONLY) };
    if fd == usize::MAX {
        out.push("audiotest: cannot open /dev/audio");
        return;
    }

    // 1 second, ~440 Hz triangle wave. /dev/audio wants raw S16LE,
    // stereo, 48 kHz. Keep the buffer small so this also exercises the
    // kernel's blocking refill path instead of requiring a large allocation.
    const RATE: usize = 48_000;
    const PERIOD: usize = 109; // 48000 / 109 ~= 440.37 Hz
    const FRAMES: usize = 1024;
    let mut pcm = [0u8; FRAMES * 4];
    let mut frame = 0usize;

    while frame < RATE {
        let count = core::cmp::min(FRAMES, RATE - frame);
        for i in 0..count {
            let p = (frame + i) % PERIOD;
            let half = PERIOD / 2;
            let sample = if p < half {
                -12_000i32 + (24_000i32 * p as i32) / half as i32
            } else {
                12_000i32 - (24_000i32 * (p - half) as i32) / (PERIOD - half) as i32
            } as i16;
            let b = sample.to_le_bytes();
            let o = i * 4;
            pcm[o] = b[0];
            pcm[o + 1] = b[1];
            pcm[o + 2] = b[0];
            pcm[o + 3] = b[1];
        }

        let bytes = count * 4;
        let mut off = 0usize;
        while off < bytes {
            let n = unsafe { write(fd as u32, pcm[off..bytes].as_ptr(), bytes - off) };
            if n == 0 || n == usize::MAX {
                unsafe { close(fd as u32); }
                out.push("audiotest: write /dev/audio failed");
                return;
            }
            off += n;
        }
        frame += count;
    }

    unsafe { close(fd as u32); }
    out.push("audiotest: wrote 1s 440 Hz / 48k stereo S16LE");
}

fn help_text() -> String {
    String::from(
        "Builtins:\n\
  ls [path]        - list directory\n\
  cat [file]       - display file content or stdin\n\
  head [file]      - display first 20 bytes from file or stdin\n\
  cd [dir]         - change directory\n\
  pwd              - print working directory\n\
  path [dirs]      - show or set PATH\n\
  mkdir / rmdir / rm\n\
  mv <old> <new>   - rename/move inside one filesystem\n\
  lspci            - list PCI devices (pci-ids names)\n\
  ifconfig         - show iface\n\
  ifconfig dhcp    - DHCP\n\
  ifconfig IP/PFX [GW]  - static\n\
  jobs / fg / bg / wait / kill - job control\n\
  mount            - list VFS mount points\n\
  mount /dev/X /mnt/Y - mount ext2/FAT block device\n\
  mounts           - list VFS mount points\n\
  umount <path>    - unmount removable filesystem\n\
  audiotest        - play 1s 440 Hz test tone via /dev/audio\n\
  clear            - clear terminal\n\
  reboot           - reboot the machine\n\
  help / exit\n\n\
Tab completes commands and paths relative to cwd.\n\
cmd &              - run in background\n\
cmd > file         - redirect stdout and detach (Felix convenience)\n\
cmd >> file        - append stdout and detach\n\
Pipes run in foreground.\n\
Files ending in .rhai are run through /bin/rhai.\n\
Ctrl+C interrupts the foreground process group through the controlling TTY.\n",
    )
}

// ---------------------------------------------------------------------------
// Interactive CLI
// ---------------------------------------------------------------------------

const MAX_INPUT: usize = 1024;
const CMD_HISTORY_MAX: usize = 64;
const HIST_PATH: &str = "/home/user/.shell_history";

const BUILTINS: &[&str] = &[
    "help", "exit", "quit", "pwd", "cd", "ls", "cat", "mkdir", "rmdir", "rm", "mv", "path", "ps",
    "jobs", "fg", "bg", "kill", "wait", "export", "unset", "env", "set", "clear", "reboot", "echo",
    "head", "lspci", "ifconfig", "mount", "mounts", "umount", "audiotest",
];

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

fn redraw_editor(term: &mut TermBuffer, shell: &Shell, editor: &LineEditor) {
    term.write_bytes(b"\r\x1b[2K");
    term.write_bytes(shell.prompt().as_bytes());
    term.write_bytes(editor.text().as_bytes());
    let back = editor.chars_after_cursor();
    if back > 0 {
        term.write_bytes(format!("\x1b[{}D", back).as_bytes());
    }
}

fn editor_termios(original: syscall::Termios) -> syscall::Termios {
    let mut t = original;
    t.c_lflag &= !(syscall::LFLAG_ICANON | syscall::LFLAG_ECHO | syscall::LFLAG_ISIG);
    t.c_cc[syscall::VMIN] = 1;
    t.c_cc[syscall::VTIME] = 0;
    t
}

#[derive(Clone, Copy)]
enum InputKey {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Tab,
    Ctrl(u8),
    PageUp,
    PageDown,
}

struct InputDecoder {
    esc: [u8; 8],
    esc_len: usize,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

impl InputDecoder {
    fn new() -> Self {
        Self {
            esc: [0; 8],
            esc_len: 0,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    fn feed(&mut self, b: u8) -> Option<InputKey> {
        if self.utf8_need != 0 {
            if self.utf8_len >= self.utf8.len() || (b & 0xc0) != 0x80 {
                self.utf8_len = 0;
                self.utf8_need = 0;
                return None;
            }
            self.utf8[self.utf8_len] = b;
            self.utf8_len += 1;
            if self.utf8_len == self.utf8_need {
                let result = core::str::from_utf8(&self.utf8[..self.utf8_len])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .map(InputKey::Char);
                self.utf8_len = 0;
                self.utf8_need = 0;
                return result;
            }
            return None;
        }

        if self.esc_len != 0 {
            if self.esc_len < self.esc.len() {
                self.esc[self.esc_len] = b;
                self.esc_len += 1;
            } else {
                self.esc_len = 0;
                return None;
            }

            let seq = &self.esc[..self.esc_len];
            let done = b.is_ascii_alphabetic() || b == b'~';
            if !done {
                return None;
            }
            let key = match seq {
                [0x1b, b'[', b'A'] | [0x1b, b'O', b'A'] => Some(InputKey::Up),
                [0x1b, b'[', b'B'] | [0x1b, b'O', b'B'] => Some(InputKey::Down),
                [0x1b, b'[', b'C'] | [0x1b, b'O', b'C'] => Some(InputKey::Right),
                [0x1b, b'[', b'D'] | [0x1b, b'O', b'D'] => Some(InputKey::Left),
                [0x1b, b'[', b'H'] | [0x1b, b'O', b'H'] | [0x1b, b'[', b'1', b'~'] => Some(InputKey::Home),
                [0x1b, b'[', b'F'] | [0x1b, b'O', b'F'] | [0x1b, b'[', b'4', b'~'] => Some(InputKey::End),
                [0x1b, b'[', b'3', b'~'] => Some(InputKey::Delete),
                [0x1b, b'[', b'5', b'~'] => Some(InputKey::PageUp),
                [0x1b, b'[', b'6', b'~'] => Some(InputKey::PageDown),
                _ => None,
            };
            self.esc_len = 0;
            return key;
        }

        match b {
            0x1b => {
                self.esc[0] = 0x1b;
                self.esc_len = 1;
                None
            }
            b'\r' | b'\n' => Some(InputKey::Enter),
            0x08 | 0x7f => Some(InputKey::Backspace),
            b'\t' => Some(InputKey::Tab),
            0x01..=0x1a => Some(InputKey::Ctrl(b)),
            0x20..=0x7e => Some(InputKey::Char(b as char)),
            0xc2..=0xdf => {
                self.utf8[0] = b;
                self.utf8_len = 1;
                self.utf8_need = 2;
                None
            }
            0xe0..=0xef => {
                self.utf8[0] = b;
                self.utf8_len = 1;
                self.utf8_need = 3;
                None
            }
            0xf0..=0xf4 => {
                self.utf8[0] = b;
                self.utf8_len = 1;
                self.utf8_need = 4;
                None
            }
            _ => None,
        }
    }
}

fn handle_key(
    key: InputKey,
    shell: &mut Shell,
    editor: &mut LineEditor,
    term: &mut TermBuffer,
    cmd_hist: &mut Vec<String>,
    hist_pos: &mut Option<usize>,
    draft: &mut String,
    cooked: Option<syscall::Termios>,
) {
    let edited = match key {
        InputKey::Ctrl(0x01) => editor.home(),
        InputKey::Ctrl(0x05) => editor.end(),
        InputKey::Ctrl(0x15) => editor.kill_before(),
        InputKey::Ctrl(0x0b) => editor.kill_after(),
        InputKey::Ctrl(0x17) => editor.delete_prev_word(),
        _ => false,
    };
    if edited {
        *hist_pos = None;
        redraw_editor(term, shell, editor);
        return;
    }

    match key {
        InputKey::Ctrl(0x03) => {
            editor.clear();
            term.write_bytes(b"^C\n");
            *hist_pos = None;
            draft.clear();
            shell.last_status = 130;
            redraw_editor(term, shell, editor);
        }
        InputKey::Ctrl(0x04) if editor.text().is_empty() => {
            shell.should_exit = true;
        }
        InputKey::Ctrl(0x04) => {
            if editor.delete() {
                *hist_pos = None;
                redraw_editor(term, shell, editor);
            }
        }
        InputKey::Enter => {
            let cmd = editor.take();
            let trimmed = cmd.trim();
            if !trimmed.is_empty() && cmd_hist.last().map(|s| s.as_str()) != Some(trimmed) {
                cmd_hist.push(trimmed.to_string());
                if cmd_hist.len() > CMD_HISTORY_MAX {
                    cmd_hist.remove(0);
                }
                save_cmd_history(cmd_hist);
            }
            *hist_pos = None;
            draft.clear();
            term.write_bytes(b"\n");
            if !trimmed.is_empty() {
                if let Some(t) = cooked.as_ref() {
                    let _ = unsafe { syscall::tcsetattr(0, t as *const _) };
                }
                interpret(shell, &cmd, term);
                if let Some(t) = cooked {
                    let editor_mode = editor_termios(t);
                    let _ = unsafe { syscall::tcsetattr(0, &editor_mode as *const _) };
                }
            }
            if !shell.should_exit {
                redraw_editor(term, shell, editor);
            }
        }
        InputKey::Left => {
            if editor.left() { redraw_editor(term, shell, editor); }
        }
        InputKey::Right => {
            if editor.right() { redraw_editor(term, shell, editor); }
        }
        InputKey::Home => {
            if editor.home() { redraw_editor(term, shell, editor); }
        }
        InputKey::End => {
            if editor.end() { redraw_editor(term, shell, editor); }
        }
        InputKey::Delete => {
            if editor.delete() {
                *hist_pos = None;
                redraw_editor(term, shell, editor);
            }
        }
        InputKey::Backspace => {
            if editor.backspace() {
                *hist_pos = None;
                redraw_editor(term, shell, editor);
            }
        }
        InputKey::Up => {
            if !cmd_hist.is_empty() {
                let next = match *hist_pos {
                    None => {
                        *draft = editor.text().to_string();
                        cmd_hist.len() - 1
                    }
                    Some(0) => 0,
                    Some(i) => i - 1,
                };
                *hist_pos = Some(next);
                editor.set(cmd_hist[next].clone());
                redraw_editor(term, shell, editor);
            }
        }
        InputKey::Down => {
            if let Some(i) = *hist_pos {
                if i + 1 < cmd_hist.len() {
                    *hist_pos = Some(i + 1);
                    editor.set(cmd_hist[i + 1].clone());
                } else {
                    *hist_pos = None;
                    editor.set(draft.clone());
                }
                redraw_editor(term, shell, editor);
            }
        }
        InputKey::Tab => {
            let cursor = editor.cursor();
            let prefix = editor.text()[..cursor].to_string();
            let suffix = editor.text()[cursor..].to_string();
            match handle_tab_completion(shell, &prefix, term) {
                CompletionResult::None => {}
                CompletionResult::Replace(new_prefix) => {
                    let new_cursor = new_prefix.len();
                    let mut full = new_prefix;
                    full.push_str(&suffix);
                    editor.set_with_cursor(full, new_cursor);
                    redraw_editor(term, shell, editor);
                }
                CompletionResult::Listed => redraw_editor(term, shell, editor),
            }
        }
        InputKey::Char(ch) if editor.text().len() < MAX_INPUT => {
            editor.insert(ch);
            *hist_pos = None;
            redraw_editor(term, shell, editor);
        }
        InputKey::PageUp | InputKey::PageDown | InputKey::Ctrl(_) | InputKey::Char(_) => {}
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = args().map(|arg| arg.to_string()).collect();
    if argv.get(1).map(|arg| arg.as_str()) == Some("--builtin") {
        let mut shell = Shell::new();
        let mut term = TermBuffer::new();
        let builtin_args = argv.into_iter().skip(2).collect();
        return run_builtin_argv(&mut shell, builtin_args, &mut term);
    }
    if argv.get(1).map(|arg| arg.as_str()) == Some("-c") {
        let command = argv[2..].join(" ");
        let mut shell = Shell::new();
        let mut term = TermBuffer::new();
        interpret(&mut shell, &command, &mut term);
        return shell.last_status;
    }

    let shell_pid = unsafe { getpid() };
    let _ = unsafe { setpgid(0, shell_pid) };

    // An interactive shell that was started in the background must not touch
    // the shared terminal modes until its process group becomes foreground.
    // The parent shell will later tcsetpgrp()+SIGCONT it from `fg`.
    loop {
        let shell_pgid = unsafe { getpgrp() };
        let foreground_pgid = unsafe { syscall::tcgetpgrp(0) };
        if foreground_pgid < 0 || foreground_pgid == shell_pgid {
            break;
        }
        unsafe {
            let _ = kill(-shell_pgid, syscall::SIGTTIN);
        }
    }

    println!("shell: userspace main started pid={}", shell_pid);
    let mut shell = Shell::new();
    let mut term = TermBuffer::new();
    let mut editor = LineEditor::new();
    let mut decoder = InputDecoder::new();
    let mut cmd_hist = load_cmd_history();
    let mut hist_pos = None;
    let mut draft = String::new();

    let mut cooked = syscall::Termios::default();
    let has_tty = unsafe { syscall::tcgetattr(0, &mut cooked as *mut _) } != usize::MAX;
    let cooked = has_tty.then_some(cooked);
    if let Some(t) = cooked {
        let editor_mode = editor_termios(t);
        let _ = unsafe { syscall::tcsetattr(0, &editor_mode as *const _) };
    }

    term.push("=== Felix User Shell ===");
    term.push("Tab paths · arrows edit/history · Ctrl+C/Z · Ctrl+A/E/U/K/W · help");
    term.push("");
    redraw_editor(&mut term, &shell, &editor);

    let mut input = [0u8; 128];
    while !shell.should_exit {
        if poll_background_jobs(&mut shell, &mut term) {
            redraw_editor(&mut term, &shell, &editor);
        }

        let mut pfd = syscall::PollFd {
            fd: 0,
            events: syscall::POLLIN,
            revents: 0,
        };
        let ready = unsafe { syscall::poll(&mut pfd as *mut _, 1, 20) };
        if ready == 0 || ready == usize::MAX || (pfd.revents & syscall::POLLIN) == 0 {
            continue;
        }

        let n = unsafe { read(0, input.as_mut_ptr(), input.len()) };
        if n == 0 {
            shell.should_exit = true;
            break;
        }
        if n == usize::MAX {
            continue;
        }
        for &b in &input[..n] {
            if let Some(key) = decoder.feed(b) {
                handle_key(
                    key,
                    &mut shell,
                    &mut editor,
                    &mut term,
                    &mut cmd_hist,
                    &mut hist_pos,
                    &mut draft,
                    cooked,
                );
                if shell.should_exit {
                    break;
                }
            }
        }
    }

    if let Some(t) = cooked {
        let _ = unsafe { syscall::tcsetattr(0, &t as *const _) };
    }
    term.write_bytes(b"\n");
    shell.last_status
}
