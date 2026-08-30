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
mod terminal;
use terminal::{Terminal, CELL_H, CELL_W};
use libfelix::syscall::{
    self, close, execve, execve_wasm, kill, mkdir, open, pipe, read, rmdir, set_nonblock, unlink,
    wait, wait_options, write, O_APPEND, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, SIGINT, WNOHANG,
};

// ---------------------------------------------------------------------------
// Shell state
// ---------------------------------------------------------------------------

struct Shell {
    cwd: String,
    path: String,
    command_cache: Option<Vec<String>>, // кэш всех команд
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
    let first = &strings[0];
    let mut prefix = String::new();
    for (i, ch) in first.char_indices() {
        for s in &strings[1..] {
            if let Some(c) = s.chars().nth(i) {
                if c != ch {
                    return prefix;
                }
            } else {
                return prefix;
            }
        }
        prefix.push(ch);
    }
    prefix
}

fn handle_tab_completion(shell: &mut Shell, input: &str, term: &mut TermBuffer) -> (String, bool) {
    // Если ввод содержит пробелы — не дополняем команду (можно расширить для путей)
    // if input.contains(' ') {
    //     return (input.to_string(), false);
    // }
    let (tr_input, prefix) = match input.rsplit_once(' ') {
        None => {
            ("", input)
        }
        Some((tr_input, prefix)) => {
            (tr_input, prefix)

        }
    };
    println!("{}", prefix);
    let all_cmds = shell.get_commands().clone();
    let mut matches: Vec<String> = all_cmds
        .into_iter()
        .filter(|cmd| cmd.starts_with(prefix))
        .collect();

    if matches.is_empty() {
        return (input.to_string(), false);
    }

    matches.sort();

    if matches.len() == 1 {
        // Единственное совпадение — заменяем ввод и добавляем пробел
        if tr_input.is_empty() {
            let new_input = format!("{} ", matches[0]);
            return (new_input, true);
        }
        let new_input = format!("{} {}", tr_input, matches[0]);
        (new_input, true)
    } else {
        // Несколько совпадений — находим общий префикс
        let common = longest_common_prefix(&matches);
        if common.len() > prefix.len() {
            // Общий префикс длиннее текущего — заменяем на него (без пробела)
            if tr_input.is_empty() {
                let new_input = format!("{} ", common);
                return (new_input, true);
            }
            let new_input = format!("{} {}", tr_input, common);
            (new_input, true)

        } else {
            // Нет общего префикса — выводим список вариантов в терминал
            let mut msg = String::from("");
            for (i, m) in matches.iter().enumerate() {
                if i > 0 {
                    msg.push_str("  ");
                }
                msg.push_str(m);
            }
            term.push(&msg);
            return (input.to_string(), true); // dirty, чтобы перерисовать терминал с подсказкой
        }
    }
}

impl Shell {
    fn new() -> Self {
        Self {
            cwd: String::from("/"),
            path: String::from("/"),
            command_cache: None,
        }
    }

    fn get_commands(&mut self) -> &Vec<String> {
        if self.command_cache.is_none() {
            let mut cmds = Vec::new();
            // Встроенные команды
            for b in BUILTINS {
                cmds.push(b.to_string());
            }
            // Внешние команды из PATH
            for dir in self.path.split(':') {
                if dir.is_empty() {
                    continue;
                }
                let files = list_dir(dir);
                for f in files {
                    cmds.push(f);
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
    let n = unsafe { syscall::ls(p.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    n > 0 || path == "/"
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum RedirKind {
    In,
    Out,
    Append,
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
                let fd = unsafe { open(path.as_ptr(), O_RDONLY) };
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
                let fd = unsafe { open(path.as_ptr(), O_WRONLY | O_CREAT | O_TRUNC) };
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
                let fd = unsafe { open(path.as_ptr(), O_WRONLY | O_CREAT | O_APPEND) };
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
        | "path" | "ps" | "clear" | "echo" | "head" | "lspci" | "ifconfig" => {}
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
        "exit" | "quit" => out.push("Goodbye."),
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
                shell.path = new_path.clone();
                shell.invalidate_cache(); // <-- добавить
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
  clear            - clear terminal\n\
  help / exit\n\n\
Redirection / pipes as usual.\n\
Ctrl+C interrupts a running program (userspace).\n",
    )
}

// ---------------------------------------------------------------------------
// External process supervision (cooperative: UI + stdout + wait)
// ---------------------------------------------------------------------------

enum UiTick {
    None,
    Interrupt,
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
            if e.kind != EV_KEY_DOWN {
                continue;
            }
            let ch = e.b as u8;
            let sc = e.a as u8;
            let mods = e.c as u8;
            if ch == 0x03 || (sc == 0x2e && (mods & 2) != 0) {
                return UiTick::Interrupt;
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
fn supervise_child(pid: i32, capture_fd: Option<u32>, out: &mut TermBuffer, ui: &mut UiBridge<'_>) {
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

        if matches!(ui.poll_keys(), UiTick::Interrupt) && !sent_sigint {
            unsafe {
                let _ = kill(pid, SIGINT);
            }
            out.push("^C");
            ui.redraw(out);
            sent_sigint = true;
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

    let mut capture_r: i32 = -1;
    let mut capture_w: i32 = -1;
    let mut serr: i32 = -1;
    if sout < 0 {
        let mut fds = [0u32; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } == 0 {
            capture_r = fds[0] as i32;
            capture_w = fds[1] as i32;
            sout = capture_w;
            serr = capture_w;
        }
    } else {
        let mut fds = [0u32; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } == 0 {
            capture_r = fds[0] as i32;
            capture_w = fds[1] as i32;
            serr = capture_w;
        }
    }

    let pid = spawn(&full, sin, sout, serr, &cmd.args);

    if capture_w >= 0 {
        unsafe {
            close(capture_w as u32);
        }
    }
    if sin >= 0 && forced_in < 0 {
        unsafe {
            close(sin as u32);
        }
    }
    if sout >= 0 && forced_out < 0 && sout != capture_w {
        unsafe {
            close(sout as u32);
        }
    }

    if let Some(p) = pid {
        let cap = if capture_r >= 0 {
            Some(capture_r as u32)
        } else {
            None
        };
        supervise_child(p, cap, out, ui);
        None
    } else {
        if capture_r >= 0 {
            unsafe {
                close(capture_r as u32);
            }
        }
        None
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
            let _ = run_external(shell, &cmd, in_fd, -1, out, ui);
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
        let _ = run_external(shell, &cmd, -1, -1, out, ui);
        return;
    }
    run_pipeline(shell, &stages, out, ui);
}

// ---------------------------------------------------------------------------
// GUI terminal
// ---------------------------------------------------------------------------

const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_TAB: u8 = 0x0F;
const MAX_INPUT: usize = 96;
const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PGUP: u8 = 0x49;
const SCAN_PGDN: u8 = 0x51;
const CMD_HISTORY_MAX: usize = 64;
const LINE_MAX_CHARS: usize = 69;
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
    if s.len() > LINE_MAX_CHARS {
        &s[..LINE_MAX_CHARS]
    } else {
        s
    }
}

fn refresh_terminal(win: &mut Window, term: &TermBuffer) {
    term.draw(win);
}

const BUILTINS: &[&str] = &[
    "help", "exit", "quit", "pwd", "cd", "ls", "cat", "mkdir", "rmdir", "rm", "path", "ps",
    "clear", "echo", "head", "lspci", "ifconfig",
];

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut win = Window::create(30, 30, 640, 400, "Felix Shell").unwrap_or_else(|| {
        Window::create(40, 40, 480, 320, "Felix Shell").expect("wm_create failed")
    });
    let mut ui = Ui::new();
    let _ = ui;

    let mut shell = Shell::new();
    let mut term = TermBuffer::new(&win);
    let mut input = String::new();
    let mut cmd_hist: Vec<String> = load_cmd_history();
    let mut hist_pos: Option<usize> = None; // None = текущая строка
    let mut draft = String::new();          // то, что набирали до ↑
    term.push("=== Felix User Shell ===");
    term.push("\x1b[32mVT\x1b[0m · \x1b[31mESC\x1b[0m · help / clear / Ctrl+C");
    term.push("");

    term.prompt_line(&shell.prompt());
    refresh_terminal(&mut win, &term);
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

            if ch == 0x03 || (scancode == 0x2e && (e.c as u8 & 2) != 0) {
                continue;
            }

            if scancode == SCAN_ENTER {
                let cmd = input.clone();
                let t = cmd.trim();
                if !t.is_empty() && cmd_hist.last().map(|s| s.as_str()) != Some(t) {
                    cmd_hist.push(t.to_string());
                    if cmd_hist.len() > CMD_HISTORY_MAX {
                        cmd_hist.remove(0);
                    }
                    save_cmd_history(&cmd_hist);
                }
                hist_pos = None;
                draft.clear();

                term.write_bytes(b"\r\n");
                input.clear();

                {
                    let mut bridge = UiBridge {
                        win: &mut win,
                        prompt: shell.prompt(),
                    };
                    bridge.redraw(&term);
                    if !cmd.trim().is_empty() {
                        interpret(&mut shell, &cmd, &mut term, &mut bridge);
                    }
                }
                term.prompt_line(&shell.prompt());
                dirty = true;
            } else if scancode == SCAN_PGUP {
                term.scroll(8);
                dirty = true;
            } else if scancode == SCAN_PGDN {
                term.scroll(-8);
                dirty = true;
            } else if scancode == SCAN_UP {
                if !cmd_hist.is_empty() {
                    let next = match hist_pos {
                        None => {
                            draft = input.clone();
                            cmd_hist.len() - 1
                        }
                        Some(0) => 0,
                        Some(i) => i - 1,
                    };
                    hist_pos = Some(next);
                    term.rubout_n(input.len());
                    input = cmd_hist[next].clone();
                    term.write_bytes(input.as_bytes());
                    dirty = true;
                }
            } else if scancode == SCAN_DOWN {
                if let Some(i) = hist_pos {
                    term.rubout_n(input.len());
                    if i + 1 < cmd_hist.len() {
                        hist_pos = Some(i + 1);
                        input = cmd_hist[i + 1].clone();
                    } else {
                        hist_pos = None;
                        input = draft.clone();
                    }
                    term.write_bytes(input.as_bytes());
                    dirty = true;
                }
            } else if scancode == SCAN_BACKSPACE {
                if input.pop().is_some() {
                    term.write_bytes(b"\x08 \x08");
                    dirty = true;
                }
            } else if scancode == SCAN_TAB {
                let (new_input, completed) = handle_tab_completion(&mut shell, &input, &mut term);
                if completed {
                    term.rubout_n(input.len());
                    input = new_input;
                    term.write_bytes(input.as_bytes());
                    dirty = true;
                    // После вывода подсказок обновляем UI
                    let prompt = shell.prompt();
                    refresh_terminal(&mut win, &term);
                    let _ = win.flip();
                }
            } else if ch >= 0x20 && ch < 0x7f && input.len() < MAX_INPUT {
                input.push(ch as char);
                term.write_bytes(&[ch]);
                dirty = true;
            }
        }

        if dirty {
            let prompt = shell.prompt();
            refresh_terminal(&mut win, &term);
            let _ = win.flip();
        }
    }
}
