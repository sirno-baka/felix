//! Process execution, pipelines and job control for the Felix shell.

use super::*;

struct RunningGroup {
    pgid: i32,
    pids: Vec<i32>,
    last_pid: i32,
    last_status: i32,
}

enum SuperviseResult {
    Exited(i32),
    Stopped(RunningGroup),
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
    // WASM still uses the in-memory interpreter ABI. Native ELF and Rhai use
    // path-based spawn below so large binaries are never duplicated in the
    // shell heap.
    if path.ends_with(".wasm") {
        let mut file = File::open(path).ok()?;
        let image = file.read_to_end().ok()?;
        if image.len() < 4 || &image[..4] != b"\0asm" { return None; }

        let mut argv_store = Vec::new();
        for arg in args {
            let mut value = arg.clone();
            value.push('\0');
            argv_store.push(value);
        }
        let argv: Vec<*const u8> = argv_store.iter().map(|s| s.as_ptr()).collect();
        let env_store = shell.exported_env();
        let envp: Vec<*const u8> = env_store.iter().map(|s| s.as_ptr()).collect();
        let pid = unsafe {
            spawn_wasm_env_pgid(
                image.as_ptr(), image.len(), stdin_fd, stdout_fd, stderr_fd,
                &argv, &envp, pgid, foreground,
            )
        };
        return (pid != usize::MAX).then_some(pid as i32);
    }

    // Rhai is the system script format. The shell keeps script execution in
    // userspace by transparently spawning /bin/rhai with the script path as argv[1].
    let is_rhai = path.ends_with(".rhai");
    let executable = if is_rhai { "/bin/rhai" } else { path };

    let mut argv_store = Vec::new();
    if is_rhai {
        let mut runtime = String::from("/bin/rhai");
        runtime.push('\0');
        argv_store.push(runtime);
        let mut script = path.to_string();
        script.push('\0');
        argv_store.push(script);
        for arg in args.iter().skip(1) {
            let mut s = arg.clone();
            s.push('\0');
            argv_store.push(s);
        }
    } else if args.is_empty() {
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

    let mut executable_c = executable.to_string();
    executable_c.push('\0');
    let pid = unsafe { spawn_path_env_pgid(
        executable_c.as_ptr(), stdin_fd, stdout_fd, stderr_fd,
        &argv, &envp, pgid, foreground,
    ) };
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
    // Builtins in a pipeline must run in a child context just like a Unix
    // subshell. Reuse the same /bin/shell in a small non-interactive mode
    // rather than duplicating builtin implementations or forcing applets into
    // /bin. Stateful builtins therefore do not mutate the parent shell.
    let builtin_stage = BUILTINS.iter().any(|name| *name == cmd.args[0]);
    let (full, exec_args) = if builtin_stage {
        let mut args = Vec::with_capacity(cmd.args.len() + 2);
        args.push(String::from("/bin/shell"));
        args.push(String::from("--builtin"));
        args.extend(cmd.args.iter().cloned());
        (String::from("/bin/shell"), args)
    } else {
        let full = shell
            .find_executable(&cmd.args[0])
            .ok_or_else(|| format!("{}: command not found", cmd.args[0]))?;
        (full, cmd.args.clone())
    };

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

    let pid = spawn(shell, &full, sin, sout, serr, &exec_args, pgid, foreground)
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

    // The shell and every job share one controlling TTY. Anonymous pipes are
    // used only between pipeline stages; the terminal-facing ends inherit the
    // shell's fd 0/1/2.
    let mut pipes = Vec::new();
    for _ in 0..commands.len().saturating_sub(1) {
        match make_pipe() {
            Ok(p) => pipes.push(p),
            Err(e) => {
                for (r, w) in pipes {
                    close_unique(&[r as i32, w as i32]);
                }
                return Err(e);
            }
        }
    }

    let mut pids = Vec::new();
    let mut pgid = 0i32;
    for (i, cmd) in commands.iter().enumerate() {
        let base_in = if i == 0 { 0 } else { pipes[i - 1].0 as i32 };
        let base_out = if i + 1 == commands.len() {
            1
        } else {
            pipes[i].1 as i32
        };
        let base_err = 2;

        let requested_pgid = if pgid == 0 { 0 } else { pgid };
        match spawn_stage(
            shell,
            cmd,
            base_in,
            base_out,
            base_err,
            requested_pgid,
            foreground,
        ) {
            Ok(pid) => {
                if pgid == 0 {
                    pgid = pid;
                }
                pids.push(pid);
            }
            Err(e) => {
                for pid in &pids {
                    unsafe { let _ = kill(*pid, SIGKILL); }
                }
                for (r, w) in pipes {
                    close_unique(&[r as i32, w as i32]);
                }
                return Err(e);
            }
        }
    }

    for (r, w) in pipes {
        close_unique(&[r as i32, w as i32]);
    }

    let last_pid = *pids.last().unwrap();
    Ok(RunningGroup {
        pgid,
        pids,
        last_pid,
        last_status: 0,
    })
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

fn wait_for_activity() {
    unsafe { syscall::sys_sleep(10); }
}

fn reclaim_shell_tty() {
    let shell_pgid = unsafe { getpgrp() };
    if shell_pgid > 0 {
        let _ = unsafe { tcsetpgrp(0, shell_pgid) };
    }
}

fn supervise_group(mut group: RunningGroup) -> SuperviseResult {
    let _ = unsafe { tcsetpgrp(0, group.pgid) };

    loop {
        let (all_exited, stopped, _) = reap_group_nonblocking(&mut group);
        if all_exited {
            reclaim_shell_tty();
            return SuperviseResult::Exited(group.last_status);
        }

        if stopped {
            // Normalize the whole pipeline into one stopped job before giving
            // the terminal back to the shell.
            unsafe { let _ = kill(-group.pgid, SIGSTOP); }
            let _ = reap_group_nonblocking(&mut group);
            reclaim_shell_tty();
            return SuperviseResult::Stopped(group);
        }

        wait_for_activity();
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
        last_status: group.last_status,
    }
}

fn running_from_job(job: BackgroundJob) -> RunningGroup {
    RunningGroup {
        pgid: job.pgid,
        pids: job.pids,
        last_pid: job.last_pid,
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
    loop {
        let (done, stopped, _) = reap_group_nonblocking(&mut group);
        if done {
            return (group.last_status, None);
        }
        if stopped {
            unsafe { let _ = kill(-group.pgid, SIGSTOP); }
            let _ = reap_group_nonblocking(&mut group);
            out.push(&format!("wait: job %{} stopped", id));
            return (
                1,
                Some(job_from_running(id, command, JobState::Stopped, group)),
            );
        }
        wait_for_activity();
    }
}

fn run_special_builtin(
    shell: &mut Shell,
    cmd: &SimpleCmd,
    out: &mut TermBuffer,
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
            let _ = unsafe { tcsetpgrp(0, job.pgid) };
            unsafe { let _ = kill(-job.pgid, SIGCONT); }
            match supervise_group(running_from_job(job)) {
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
) -> i32 {
    let commands: Vec<SimpleCmd> = group.pipeline.iter().map(|c| expand_cmd(shell, c)).collect();
    if commands.is_empty() {
        return 0;
    }

    if commands.len() == 1 {
        if let Some(status) = run_special_builtin(shell, &commands[0], out) {
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
        let id = shell.alloc_job_id();
        let last_pid = running.last_pid;
        let pgid = running.pgid;
        shell.jobs.push(job_from_running(id, command_text.to_string(), JobState::Running, running));
        out.push(&format!("[{}] started pgid={} pid={}", id, pgid, last_pid));
        return 0;
    }

    match supervise_group(running) {
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

pub(super) fn run_builtin_argv(shell: &mut Shell, args: Vec<String>, out: &mut TermBuffer) -> i32 {
    if args.is_empty() {
        return 0;
    }
    let cmd = SimpleCmd {
        args,
        redirs: Vec::new(),
    };
    if let Some(status) = run_special_builtin(shell, &cmd, out) {
        return status;
    }
    shell.last_status = 0;
    if try_builtin(shell, &cmd, out) {
        shell.last_status
    } else {
        127
    }
}

pub(super) fn interpret(shell: &mut Shell, line: &str, out: &mut TermBuffer) {
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
            status = execute_group(shell, group, line, out);
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
        let mut group = RunningGroup {
            pgid: shell.jobs[i].pgid,
            pids: core::mem::take(&mut shell.jobs[i].pids),
            last_pid: shell.jobs[i].last_pid,
            last_status: shell.jobs[i].last_status,
        };

        let (done, stopped, continued) = reap_group_nonblocking(&mut group);
        if stopped && !done {
            unsafe { let _ = kill(-group.pgid, SIGSTOP); }
            let _ = reap_group_nonblocking(&mut group);
        }

        shell.jobs[i].pids = group.pids;
        shell.jobs[i].last_status = group.last_status;

        if done || shell.jobs[i].pids.is_empty() {
            let status = shell.jobs[i].last_status;
            out.write_bytes(b"\r\n");
            out.push(&format!(
                "[{}] Done ({}) {}",
                shell.jobs[i].id, status, shell.jobs[i].command
            ));
            shell.jobs.remove(i);
            changed = true;
            continue;
        }

        if stopped {
            shell.jobs[i].state = JobState::Stopped;
            if previous_state != JobState::Stopped {
                out.write_bytes(b"\r\n");
                out.push(&format!(
                    "[{}] Stopped {}",
                    shell.jobs[i].id, shell.jobs[i].command
                ));
                changed = true;
            }
        } else if continued {
            shell.jobs[i].state = JobState::Running;
            if previous_state == JobState::Stopped {
                out.write_bytes(b"\r\n");
                out.push(&format!(
                    "[{}] Continued {}",
                    shell.jobs[i].id, shell.jobs[i].command
                ));
                changed = true;
            }
        }

        i += 1;
    }

    changed
}
