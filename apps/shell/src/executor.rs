//! Process execution, pipelines and job control for the Felix shell.

use super::*;

struct RunningGroup {
    pgid: i32,
    pids: Vec<i32>,
    last_pid: i32,
    capture_fd: Option<u32>,
    stdin_w: Option<u32>,
    last_status: i32,
}

enum SuperviseResult {
    Exited(i32),
    Stopped(RunningGroup),
}

enum UiTick {
    None,
    Interrupt,
    Suspend,
    Data([u8; 8], usize),
}

struct UiBridge<'a> {
    win: &'a mut Window,
}

impl UiBridge<'_> {
    fn poll_keys(&mut self) -> UiTick {
        let mut evbuf = [WmEvent::default(); 32];
        let n = self.win.poll_events(&mut evbuf);
        for e in &evbuf[..n] {
            if e.kind != EV_KEY_DOWN {
                continue;
            }
            let sc = e.a as u8;
            let ch = e.b as u8;
            let mods = e.c as u8;
            let ctrl = (mods & 2) != 0;
            if ch == 0x03 || (sc == SCAN_C && ctrl) {
                return UiTick::Interrupt;
            }
            if ch == 0x1a || (sc == SCAN_Z && ctrl) {
                return UiTick::Suspend;
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

fn map_child_key(scancode: u8, ch: u8, mods: u8) -> ([u8; 8], usize) {
    let ctrl = (mods & 2) != 0;
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
    shell: &Shell,
    path: &str,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    args: &[String],
    pgid: i32,
    foreground: bool,
) -> Option<i32> {
    let mut f = File::open(path).ok()?;
    let data = f.read_to_end().ok()?;
    if data.len() < 4 {
        return None;
    }

    let mut argv_store = Vec::new();
    if args.is_empty() {
        let mut s = path.to_string();
        s.push('\0');
        argv_store.push(s);
    } else {
        for arg in args {
            let mut s = arg.clone();
            s.push('\0');
            argv_store.push(s);
        }
    }
    let argv: Vec<*const u8> = argv_store.iter().map(|s| s.as_ptr()).collect();

    let env_store = shell.exported_env();
    let envp: Vec<*const u8> = env_store.iter().map(|s| s.as_ptr()).collect();

    let pid = unsafe {
        match &data[..4] {
            b"\0asm" => execve_wasm_env_pgid(
                data.as_ptr(), data.len(), stdin_fd, stdout_fd, stderr_fd, &argv, &envp, pgid, foreground,
            ),
            b"\x7fELF" => execve_env_pgid(
                data.as_ptr(), data.len(), stdin_fd, stdout_fd, stderr_fd, &argv, &envp, pgid, foreground,
            ),
            _ => usize::MAX,
        }
    };
    (pid != usize::MAX).then_some(pid as i32)
}

fn close_unique(fds: &[i32]) {
    for (i, fd) in fds.iter().enumerate() {
        if *fd < 0 || fds[..i].contains(fd) {
            continue;
        }
        unsafe { close(*fd as u32); }
    }
}

fn spawn_stage(
    shell: &Shell,
    cmd: &SimpleCmd,
    base_in: i32,
    base_out: i32,
    base_err: i32,
    pgid: i32,
    foreground: bool,
) -> Result<i32, String> {
    let full = shell
        .find_executable(&cmd.args[0])
        .ok_or_else(|| format!("{}: command not found", cmd.args[0]))?;

    // Apply redirections strictly left-to-right. This preserves the important
    // distinction between `>file 2>&1` and `2>&1 >file`.
    let mut sin = base_in;
    let mut sout = base_out;
    let mut serr = base_err;
    let mut opened: Vec<i32> = Vec::new();
    for redir in &cmd.redirs {
        match &redir.target {
            RedirTarget::Path(path) => {
                let fd = match open_redir_path(shell, redir, path) {
                    Ok(fd) => fd,
                    Err(e) => {
                        close_unique(&opened);
                        return Err(e);
                    }
                };
                opened.push(fd);
                match redir.fd {
                    0 => sin = fd,
                    1 => sout = fd,
                    2 => serr = fd,
                    other => {
                        close_unique(&opened);
                        return Err(format!("redirection: fd {} is not supported", other));
                    }
                }
            }
            RedirTarget::Fd(target) => match (redir.fd, *target) {
                (0, 0) => {}
                (1, 1) => {}
                (2, 2) => {}
                (1, 2) => sout = serr,
                (2, 1) => serr = sout,
                (0, 1) => sin = sout,
                (0, 2) => sin = serr,
                (1, 0) => sout = sin,
                (2, 0) => serr = sin,
                (from, to) => {
                    close_unique(&opened);
                    return Err(format!("redirection: {}>&{} is not supported", from, to));
                }
            },
        }
    }

    let pid = spawn(shell, &full, sin, sout, serr, &cmd.args, pgid, foreground)
        .ok_or_else(|| format!("{}: exec failed", cmd.args[0]));
    close_unique(&opened);
    pid
}

fn make_pipe() -> Result<(u32, u32), String> {
    let mut fds = [0u32; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
        Err(String::from("pipe failed"))
    } else {
        Ok((fds[0], fds[1]))
    }
}

fn has_stdin_redir(cmd: &SimpleCmd) -> bool {
    cmd.redirs.iter().any(|r| r.fd == 0)
}

fn has_stdout_redir(cmd: &SimpleCmd) -> bool {
    cmd.redirs
        .iter()
        .any(|r| r.fd == 1 && matches!(&r.target, RedirTarget::Path(_)))
}

fn spawn_pipeline(shell: &Shell, commands: &[SimpleCmd], foreground: bool) -> Result<RunningGroup, String> {
    if commands.is_empty() {
        return Err(String::from("empty pipeline"));
    }

    // One PTY connects the GUI shell to the whole foreground/background job.
    // Internal pipeline edges remain anonymous pipes; stdin of stage 0 and
    // stdout/stderr of the terminal-facing stages use the PTY slave.
    let mut pty_fds = [0u32; 2];
    let pty_rc = unsafe { openpty(pty_fds.as_mut_ptr()) };
    if (pty_rc as isize) < 0 {
        return Err(format!("openpty failed ({})", pty_rc as isize));
    }
    let pty_master = pty_fds[0];
    let pty_slave = pty_fds[1];
    let _ = unsafe { set_nonblock(pty_master) };

    let mut pipes = Vec::new();
    for _ in 0..commands.len().saturating_sub(1) {
        match make_pipe() {
            Ok(p) => pipes.push(p),
            Err(e) => {
                // Drop the slave before the master: closing a master while a
                // slave is still open generates SIGHUP for the foreground
                // process group, which includes the shell on this error path.
                close_unique(&[pty_slave as i32, pty_master as i32]);
                for (r, w) in pipes { close_unique(&[r as i32, w as i32]); }
                return Err(e);
            }
        }
    }

    let mut pids = Vec::new();
    let mut pgid = 0i32;
    for (i, cmd) in commands.iter().enumerate() {
        let base_in = if i == 0 { pty_slave as i32 } else { pipes[i - 1].0 as i32 };
        let base_out = if i + 1 == commands.len() {
            pty_slave as i32
        } else {
            pipes[i].1 as i32
        };
        let base_err = pty_slave as i32;

        // First stage asks the kernel to create a process group whose id is
        // its own PID (pgid=0). Following stages atomically join that group.
        let requested_pgid = if pgid == 0 { 0 } else { pgid };
        match spawn_stage(shell, cmd, base_in, base_out, base_err, requested_pgid, foreground) {
            Ok(pid) => {
                if pgid == 0 {
                    pgid = pid;
                }
                pids.push(pid);
            }
            Err(e) => {
                for pid in &pids { unsafe { let _ = kill(*pid, SIGKILL); } }
                if foreground { let _ = unsafe { tty_setfg(getpgrp()) }; }
                close_unique(&[pty_slave as i32, pty_master as i32]);
                for (r, w) in pipes { close_unique(&[r as i32, w as i32]); }
                return Err(e);
            }
        }
    }

    // Parent keeps only master. Every child-side stdio reference was duplicated
    // into the task fd tables by execve; the shell's slave reference can close.
    unsafe { close(pty_slave); }
    for (r, w) in pipes {
        close_unique(&[r as i32, w as i32]);
    }

    let last_pid = *pids.last().unwrap();
    Ok(RunningGroup {
        pgid,
        pids,
        last_pid,
        capture_fd: Some(pty_master),
        stdin_w: None,
        last_status: 0,
    })
}

fn drain_capture(fd: u32, out: &mut TermBuffer) -> bool {
    let mut any = false;
    let mut buf = [0u8; 512];
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
        if n == 0 || n == usize::MAX {
            break;
        }
        out.write_bytes(&buf[..n]);
        any = true;
    }
    any
}

/// Background output can arrive while the user is editing the prompt. Start it
/// on a fresh line; main() will redraw the prompt/editor afterwards.
fn drain_background_capture(fd: u32, out: &mut TermBuffer) -> bool {
    let mut buf = [0u8; 512];
    let first = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
    if first == 0 || first == usize::MAX {
        return false;
    }
    out.write_bytes(b"\r\n");
    out.write_bytes(&buf[..first]);
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr(), buf.len()) };
        if n == 0 || n == usize::MAX {
            break;
        }
        out.write_bytes(&buf[..n]);
    }
    true
}

fn wait_status_exit_code(status: i32) -> i32 {
    if syscall::wifexited(status) {
        syscall::wexitstatus(status)
    } else if syscall::wifsignaled(status) {
        128 + syscall::wtermsig(status) as i32
    } else {
        status
    }
}

/// Consume waitpid child-state events for a pipeline. Exits remove a child;
/// stop/continue transitions do not reap it. The transition flags describe the
/// newest event observed for each child in this drain; a later CONT overrides
/// an earlier STOP from the same child.
fn reap_group_nonblocking(group: &mut RunningGroup) -> (bool, bool, bool) {
    let mut i = 0usize;
    let mut stopped_now = false;
    let mut continued_now = false;
    while i < group.pids.len() {
        let pid = group.pids[i];
        let mut transition = 0u8; // 0 none, 1 stopped, 2 continued
        let mut removed = false;

        // A fast stop->continue can leave both transitions pending. Drain a
        // few events for this child so the final transition is not stale.
        for _ in 0..4 {
            let mut status = 0i32;
            let ret = unsafe {
                waitpid_status(pid, &mut status, WNOHANG | WUNTRACED | WCONTINUED)
            };
            if ret == 0 {
                break;
            }
            if ret == usize::MAX {
                group.pids.remove(i);
                removed = true;
                break;
            }
            if ret != pid as usize {
                break;
            }
            if syscall::wifstopped(status) {
                transition = 1;
                continue;
            }
            if syscall::wifcontinued(status) {
                transition = 2;
                continue;
            }

            if pid == group.last_pid {
                group.last_status = wait_status_exit_code(status);
            }
            group.pids.remove(i);
            removed = true;
            break;
        }

        if !removed {
            match transition {
                1 => stopped_now = true,
                2 => continued_now = true,
                _ => {}
            }
            i += 1;
        }
    }
    (group.pids.is_empty(), stopped_now, continued_now)
}

fn close_group_io(group: &mut RunningGroup) {
    if let Some(fd) = group.capture_fd.take() {
        unsafe { close(fd); }
    }
    if let Some(fd) = group.stdin_w.take() {
        unsafe { close(fd); }
    }
}

fn block_on_yield() {
    libfelix::async_rt::block_on(async { yield_now().await; });
}

fn supervise_group(
    mut group: RunningGroup,
    out: &mut TermBuffer,
    win: &mut Window,
) -> SuperviseResult {
    let shell_pgid = unsafe { getpgrp() };
    let _ = unsafe { tty_setfg(group.pgid) };
    if let Some(fd) = group.capture_fd {
        let _ = unsafe { set_nonblock(fd) };
    }
    let mut ui = UiBridge { win };

    loop {
        if let Some(fd) = group.capture_fd {
            if drain_capture(fd, out) {
                ui.redraw(out);
            }
        }

        match ui.poll_keys() {
            UiTick::Interrupt => {
                let control = [0x03u8];
                let delivered = group.capture_fd.map(|fd| unsafe { write(fd, control.as_ptr(), 1) }).unwrap_or(0);
                if delivered == 0 { unsafe { let _ = kill(-group.pgid, SIGINT); } }
            }
            UiTick::Suspend => {
                let control = [0x1au8];
                let delivered = group.capture_fd.map(|fd| unsafe { write(fd, control.as_ptr(), 1) }).unwrap_or(0);
                if delivered == 0 { unsafe { let _ = kill(-group.pgid, SIGTSTP); } }
            }
            UiTick::Data(buf, n) => {
                if let Some(master) = group.capture_fd {
                    unsafe { let _ = write(master, buf.as_ptr(), n); }
                }
            }
            _ => {}
        }

        let (all_exited, stopped, _) = reap_group_nonblocking(&mut group);
        if all_exited {
            if let Some(fd) = group.capture_fd {
                let _ = drain_capture(fd, out);
            }
            let status = group.last_status;
            close_group_io(&mut group);
            let _ = unsafe { tty_setfg(shell_pgid) };
            ui.redraw(out);
            return SuperviseResult::Exited(status);
        }

        if stopped {
            // Treat a pipeline as one job. If one stage stopped, freeze any
            // remaining stages as well so `bg`/`fg` resume a coherent group.
            unsafe { let _ = kill(-group.pgid, SIGSTOP); }
            // Consume the SIGSTOP transitions generated above. Otherwise a
            // later `fg` could immediately observe a stale WUNTRACED event.
            let _ = reap_group_nonblocking(&mut group);
            let _ = unsafe { tty_setfg(shell_pgid) };
            ui.redraw(out);
            return SuperviseResult::Stopped(group);
        }
        block_on_yield();
    }
}

fn job_index(shell: &Shell, spec: Option<&str>) -> Option<usize> {
    match spec {
        None => shell.jobs.len().checked_sub(1),
        Some(s) if s.starts_with('%') => {
            let id = s[1..].parse::<u32>().ok()?;
            shell.jobs.iter().position(|j| j.id == id)
        }
        Some(s) => {
            let pid = s.parse::<i32>().ok()?;
            shell.jobs.iter().position(|j| j.pids.contains(&pid) || j.last_pid == pid)
        }
    }
}

fn job_from_running(id: u32, command: String, state: JobState, group: RunningGroup) -> BackgroundJob {
    BackgroundJob {
        id,
        pgid: group.pgid,
        pids: group.pids,
        last_pid: group.last_pid,
        command,
        state,
        capture_fd: group.capture_fd,
        stdin_w: group.stdin_w,
        last_status: group.last_status,
    }
}

fn running_from_job(job: BackgroundJob) -> RunningGroup {
    RunningGroup {
        pgid: job.pgid,
        pids: job.pids,
        last_pid: job.last_pid,
        capture_fd: job.capture_fd,
        stdin_w: job.stdin_w,
        last_status: job.last_status,
    }
}

fn emit_line(file_fd: i32, out: &mut TermBuffer, line: &str) {
    if file_fd >= 0 {
        let mut s = line.to_string();
        s.push('\n');
        unsafe { let _ = write(file_fd as u32, s.as_ptr(), s.len()); }
    } else {
        out.push(line);
    }
}

fn special_stdout(shell: &Shell, cmd: &SimpleCmd) -> Result<i32, String> {
    let mut fds = prepare_redirs(shell, &cmd.redirs)?;
    close_if_open(&mut fds.stdin);
    close_if_open(&mut fds.stderr);
    Ok(fds.stdout)
}

fn split_assignment(s: &str) -> Option<(&str, &str)> {
    let (name, value) = s.split_once('=')?;
    if name.is_empty()
        || !name.bytes().next()?.is_ascii_alphabetic() && name.as_bytes()[0] != b'_'
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    Some((name, value))
}

fn parse_signal(s: &str) -> Option<u32> {
    let raw = s.strip_prefix('-').unwrap_or(s);
    match raw {
        "INT" | "SIGINT" => Some(SIGINT),
        "KILL" | "SIGKILL" => Some(SIGKILL),
        "TERM" | "SIGTERM" => Some(SIGTERM),
        "STOP" | "SIGSTOP" => Some(SIGSTOP),
        "TSTP" | "SIGTSTP" => Some(syscall::SIGTSTP),
        "TTIN" | "SIGTTIN" => Some(syscall::SIGTTIN),
        "TTOU" | "SIGTTOU" => Some(syscall::SIGTTOU),
        "CONT" | "SIGCONT" => Some(SIGCONT),
        _ => raw.parse::<u32>().ok(),
    }
}

fn wait_job(job: BackgroundJob, out: &mut TermBuffer) -> (i32, Option<BackgroundJob>) {
    if job.state == JobState::Stopped {
        out.push(&format!("wait: job %{} is stopped", job.id));
        return (1, Some(job));
    }
    let id = job.id;
    let command = job.command.clone();
    let mut group = running_from_job(job);
    if let Some(fd) = group.capture_fd { let _ = unsafe { set_nonblock(fd) }; }
    loop {
        if let Some(fd) = group.capture_fd { let _ = drain_capture(fd, out); }
        let (done, stopped, _) = reap_group_nonblocking(&mut group);
        if done {
            let status = group.last_status;
            close_group_io(&mut group);
            return (status, None);
        }
        if stopped {
            unsafe { let _ = kill(-group.pgid, SIGSTOP); }
            let _ = reap_group_nonblocking(&mut group);
            out.push(&format!("wait: job %{} stopped", id));
            return (1, Some(job_from_running(id, command, JobState::Stopped, group)));
        }
        block_on_yield();
    }
}

fn run_special_builtin(
    shell: &mut Shell,
    cmd: &SimpleCmd,
    out: &mut TermBuffer,
    win: &mut Window,
) -> Option<i32> {
    let name = cmd.args.first()?.as_str();

    // NAME=value as a standalone shell assignment.
    if cmd.args.len() == 1 && cmd.redirs.is_empty() {
        if let Some((key, value)) = split_assignment(name) {
            shell.set_var(key, value, false);
            return Some(0);
        }
    }

    match name {
        "export" => {
            let fd = match special_stdout(shell, cmd) { Ok(v) => v, Err(e) => { out.push(&e); return Some(1); } };
            if cmd.args.len() == 1 {
                for v in shell.env.iter().filter(|v| v.exported) {
                    emit_line(fd, out, &format!("export {}={}", v.name, v.value));
                }
            } else {
                for arg in cmd.args.iter().skip(1) {
                    if let Some((key, value)) = split_assignment(arg) {
                        shell.set_var(key, value, true);
                    } else if !shell.export_name(arg) {
                        shell.set_var(arg, "", true);
                    }
                }
            }
            if fd >= 0 { unsafe { close(fd as u32); } }
            Some(0)
        }
        "unset" => {
            for name in cmd.args.iter().skip(1) { shell.unset_var(name); }
            Some(0)
        }
        "env" | "set" => {
            // `set NAME=value` assigns a non-exported shell variable; plain set
            // lists all variables, while env lists only exported variables.
            if name == "set" && cmd.args.len() > 1 {
                for arg in cmd.args.iter().skip(1) {
                    if let Some((key, value)) = split_assignment(arg) {
                        shell.set_var(key, value, false);
                    } else {
                        out.push(&format!("set: expected NAME=value, got {}", arg));
                        return Some(1);
                    }
                }
                return Some(0);
            }
            let fd = match special_stdout(shell, cmd) { Ok(v) => v, Err(e) => { out.push(&e); return Some(1); } };
            for v in &shell.env {
                if name == "set" || v.exported {
                    emit_line(fd, out, &format!("{}={}", v.name, v.value));
                }
            }
            if fd >= 0 { unsafe { close(fd as u32); } }
            Some(0)
        }
        "jobs" => {
            if shell.jobs.is_empty() {
                out.push("No background jobs");
            } else {
                for job in &shell.jobs {
                    let state = if job.state == JobState::Stopped { "Stopped" } else { "Running" };
                    out.push(&format!("[{}] {:<7} pgid={} pid={} {}", job.id, state, job.pgid, job.last_pid, job.command));
                }
            }
            Some(0)
        }
        "bg" => {
            let Some(idx) = job_index(shell, cmd.args.get(1).map(|s| s.as_str())) else {
                out.push("bg: job not found");
                return Some(1);
            };
            let job = &mut shell.jobs[idx];
            unsafe { let _ = kill(-job.pgid, SIGCONT); }
            job.state = JobState::Running;
            if let Some(fd) = job.stdin_w.take() { unsafe { close(fd); } }
            out.push(&format!("[{}] continued in background", job.id));
            Some(0)
        }
        "fg" => {
            let Some(idx) = job_index(shell, cmd.args.get(1).map(|s| s.as_str())) else {
                out.push("fg: job not found");
                return Some(1);
            };
            let job = shell.jobs.remove(idx);
            let id = job.id;
            let command = job.command.clone();
            // Give tty ownership back before waking the stopped group; otherwise
            // a resumed reader can immediately receive SIGTTIN again.
            let _ = unsafe { tty_setfg(job.pgid) };
            unsafe { let _ = kill(-job.pgid, SIGCONT); }
            match supervise_group(running_from_job(job), out, win) {
                SuperviseResult::Exited(status) => Some(status),
                SuperviseResult::Stopped(group) => {
                    shell.jobs.push(job_from_running(id, command, JobState::Stopped, group));
                    Some(148)
                }
            }
        }
        "kill" => {
            if cmd.args.len() < 2 {
                out.push("Usage: kill [-SIGNAL] <pid|%job>");
                return Some(1);
            }
            let mut sig = SIGTERM;
            let mut pos = 1usize;
            if cmd.args[1].starts_with('-') {
                let Some(parsed) = parse_signal(&cmd.args[1]) else {
                    out.push("kill: bad signal");
                    return Some(1);
                };
                sig = parsed;
                pos += 1;
            }
            let Some(spec) = cmd.args.get(pos) else {
                out.push("kill: missing pid/job");
                return Some(1);
            };
            if let Some(idx) = job_index(shell, Some(spec)) {
                let ok = unsafe { kill(-shell.jobs[idx].pgid, sig) } != usize::MAX;
                if sig == SIGSTOP { shell.jobs[idx].state = JobState::Stopped; }
                if sig == SIGCONT { shell.jobs[idx].state = JobState::Running; }
                Some(if ok { 0 } else { 1 })
            } else if let Ok(pid) = spec.parse::<i32>() {
                Some(if unsafe { kill(pid, sig) } == 0 { 0 } else { 1 })
            } else {
                out.push("kill: process/job not found");
                Some(1)
            }
        }
        "wait" => {
            if let Some(spec) = cmd.args.get(1) {
                if let Some(idx) = job_index(shell, Some(spec)) {
                    let job = shell.jobs.remove(idx);
                    let (status, keep) = wait_job(job, out);
                    if let Some(job) = keep { shell.jobs.push(job); }
                    Some(status)
                } else if let Ok(pid) = spec.parse::<i32>() {
                    let mut status = 0;
                    let ret = unsafe { waitpid_status(pid, &mut status, 0) };
                    Some(if ret == pid as usize { wait_status_exit_code(status) } else { 1 })
                } else {
                    out.push("wait: job not found");
                    Some(1)
                }
            } else {
                let mut status = 0;
                while !shell.jobs.is_empty() {
                    let job = shell.jobs.remove(0);
                    let (s, keep) = wait_job(job, out);
                    status = s;
                    if let Some(job) = keep {
                        shell.jobs.push(job);
                        break;
                    }
                }
                Some(status)
            }
        }
        _ => None,
    }
}

fn execute_group(
    shell: &mut Shell,
    group: &CommandGroup,
    command_text: &str,
    out: &mut TermBuffer,
    win: &mut Window,
) -> i32 {
    let commands: Vec<SimpleCmd> = group.pipeline.iter().map(|c| expand_cmd(shell, c)).collect();
    if commands.is_empty() {
        return 0;
    }

    if commands.len() == 1 {
        if let Some(status) = run_special_builtin(shell, &commands[0], out, win) {
            return status;
        }
        shell.last_status = 0;
        if try_builtin(shell, &commands[0], out) {
            return shell.last_status;
        }
    }

    let detach = group.background || commands.last().map(has_stdout_redir).unwrap_or(false);
    let mut running = match spawn_pipeline(shell, &commands, !detach) {
        Ok(g) => g,
        Err(e) => {
            out.push(&e);
            return 127;
        }
    };

    if detach {
        if let Some(fd) = running.stdin_w.take() { unsafe { close(fd); } }
        let id = shell.alloc_job_id();
        let last_pid = running.last_pid;
        let pgid = running.pgid;
        shell.jobs.push(job_from_running(id, command_text.to_string(), JobState::Running, running));
        out.push(&format!("[{}] started pgid={} pid={}", id, pgid, last_pid));
        return 0;
    }

    match supervise_group(running, out, win) {
        SuperviseResult::Exited(status) => status,
        SuperviseResult::Stopped(group) => {
            let id = shell.alloc_job_id();
            let pid = group.last_pid;
            let pgid = group.pgid;
            shell.jobs.push(job_from_running(id, command_text.to_string(), JobState::Stopped, group));
            out.push(&format!("[{}] Stopped pgid={} pid={}", id, pgid, pid));
            148
        }
    }
}

pub(super) fn interpret(shell: &mut Shell, line: &str, out: &mut TermBuffer, win: &mut Window) {
    let groups = match parse_line(line) {
        Ok(v) => v,
        Err(e) => {
            out.push(&format!("syntax: {}", e));
            shell.last_status = 2;
            return;
        }
    };

    let mut prev = Connector::Always;
    let mut status = shell.last_status;
    for group in &groups {
        let run = match prev {
            Connector::Always => true,
            Connector::And => status == 0,
            Connector::Or => status != 0,
        };
        if run {
            status = execute_group(shell, group, line, out, win);
            shell.last_status = status;
            if shell.should_exit {
                break;
            }
        }
        prev = group.next;
    }
}

/// Drain output and reap completed background pipelines without blocking.
/// Returns true when terminal content changed.
pub(super) fn poll_background_jobs(shell: &mut Shell, out: &mut TermBuffer) -> bool {
    let mut changed = false;
    let mut i = 0usize;
    while i < shell.jobs.len() {
        let previous_state = shell.jobs[i].state;

        if previous_state == JobState::Running {
            if let Some(fd) = shell.jobs[i].capture_fd {
                if drain_background_capture(fd, out) { changed = true; }
            }
        }

        // waitpid is now the sole source of child job-control state. Keep ps/
        // task_list diagnostic-only so stop/continue transitions cannot race a
        // separate task-table snapshot.
        let mut group = RunningGroup {
            pgid: shell.jobs[i].pgid,
            pids: core::mem::take(&mut shell.jobs[i].pids),
            last_pid: shell.jobs[i].last_pid,
            capture_fd: shell.jobs[i].capture_fd,
            stdin_w: shell.jobs[i].stdin_w,
            last_status: shell.jobs[i].last_status,
        };
        let (done, stopped, continued) = reap_group_nonblocking(&mut group);

        if stopped && !done {
            unsafe { let _ = kill(-group.pgid, SIGSTOP); }
            // Consume stop notifications generated while normalizing the whole
            // pipeline into one stopped job.
            let _ = reap_group_nonblocking(&mut group);
        }

        shell.jobs[i].pids = group.pids;
        shell.jobs[i].last_status = group.last_status;

        if done || shell.jobs[i].pids.is_empty() {
            if let Some(fd) = shell.jobs[i].capture_fd.take() { unsafe { close(fd); } }
            if let Some(fd) = shell.jobs[i].stdin_w.take() { unsafe { close(fd); } }
            let status = shell.jobs[i].last_status;
            if !changed { out.write_bytes(b"\r\n"); }
            out.push(&format!("[{}] Done ({}) {}", shell.jobs[i].id, status, shell.jobs[i].command));
            shell.jobs.remove(i);
            changed = true;
            continue;
        }

        if stopped {
            shell.jobs[i].state = JobState::Stopped;
            if previous_state != JobState::Stopped {
                if !changed { out.write_bytes(b"\r\n"); }
                out.push(&format!("[{}] Stopped {}", shell.jobs[i].id, shell.jobs[i].command));
                changed = true;
            }
        } else if continued {
            shell.jobs[i].state = JobState::Running;
            if previous_state == JobState::Stopped {
                if !changed { out.write_bytes(b"\r\n"); }
                out.push(&format!("[{}] Continued {}", shell.jobs[i].id, shell.jobs[i].command));
                changed = true;
            }
        }
        i += 1;
    }
    changed
}
