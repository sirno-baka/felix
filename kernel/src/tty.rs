//! TTY/PTY core for Unix-style job control.
//!
//! The controlling TTY owns a foreground process group. Pseudo terminals use
//! a master/slave byte stream: the GUI shell owns master, foreground programs
//! inherit slave. Basic ISIG/ECHO/ICANON line-discipline state is kept here.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::arch::asm;

use crate::filesystem::file::PtySide;
use crate::multitasking::task::{TASK_MANAGER, MAX_TASKS};
use crate::sync::MutexLazy;
use crate::sync::mutex::Mutex;

pub const MAX_TTYS: usize = 4;
pub const MAX_PTYS: usize = 8;
const PTY_BUF: usize = 8192;
const CANON_BUF: usize = 1024;

pub const NCCS: usize = 19;
pub const VINTR: usize = 0;
pub const VQUIT: usize = 1;
pub const VERASE: usize = 2;
pub const VKILL: usize = 3;
pub const VEOF: usize = 4;
pub const VTIME: usize = 5;
pub const VMIN: usize = 6;
pub const VSUSP: usize = 10;

fn default_cc() -> [u8; NCCS] {
    let mut cc = [0u8; NCCS];
    cc[VINTR] = 0x03;  // ^C
    cc[VQUIT] = 0x1c;  // ^\\
    cc[VERASE] = 0x7f;
    cc[VKILL] = 0x15;  // ^U
    cc[VEOF] = 0x04;   // ^D
    cc[VTIME] = 0;
    cc[VMIN] = 1;
    cc[VSUSP] = 0x1a;  // ^Z
    cc
}

#[derive(Clone, Copy)]
pub struct TtyState {
    pub allocated: bool,
    pub session_id: i32,
    pub foreground_pgid: i32,
}

impl TtyState {
    pub const fn empty() -> Self {
        Self { allocated: false, session_id: -1, foreground_pgid: -1 }
    }
}

static mut TTYS: [TtyState; MAX_TTYS] = [TtyState::empty(); MAX_TTYS];

struct Pty {
    tty_id: usize,
    to_master: VecDeque<u8>,
    to_slave: VecDeque<u8>,
    canonical_input: Vec<u8>,
    /// Sizes of completed canonical records currently queued in to_slave.
    canonical_records: VecDeque<usize>,
    master_refs: u16,
    slave_refs: u16,
    canonical: bool,
    echo: bool,
    isig: bool,
    tostop: bool,
    eof_pending: bool,
    icrnl: bool,
    opost: bool,
    onlcr: bool,
    cc: [u8; NCCS],
}

impl Pty {
    fn new(tty_id: usize) -> Self {
        Self {
            tty_id,
            to_master: VecDeque::new(),
            to_slave: VecDeque::new(),
            canonical_input: Vec::new(),
            canonical_records: VecDeque::new(),
            master_refs: 1,
            slave_refs: 1,
            // A newly opened PTY starts with normal Unix-style cooked input.
            canonical: true,
            echo: true,
            isig: true,
            tostop: false,
            eof_pending: false,
            icrnl: true,
            opost: true,
            onlcr: true,
            cc: default_cc(),
        }
    }
}

struct PtyTable {
    slots: [Option<Pty>; MAX_PTYS],
}

fn new_pty_table() -> Mutex<PtyTable> {
    Mutex::new(PtyTable { slots: [const { None }; MAX_PTYS] })
}

static PTYS: MutexLazy<Mutex<PtyTable>> = MutexLazy::new(new_pty_table);

/// Return an existing controlling tty or lazily attach one to the caller's
/// session. Descendants inherit tty_id from Task during exec.
fn ensure_controlling_tty(current_slot: usize) -> Option<usize> {
    if current_slot >= MAX_TASKS as usize { return None; }
    unsafe {
        let task = TASK_MANAGER.tasks[current_slot].as_mut()?;
        if task.tty_id >= 0 { return Some(task.tty_id as usize); }
        let sid = task.sid;
        if sid <= 0 { return None; }

        for (id, tty) in TTYS.iter().enumerate() {
            if tty.allocated && tty.session_id == sid {
                task.tty_id = id as i16;
                return Some(id);
            }
        }
        for (id, tty) in TTYS.iter_mut().enumerate() {
            if !tty.allocated {
                *tty = TtyState {
                    allocated: true,
                    session_id: sid,
                    foreground_pgid: task.pgid,
                };
                task.tty_id = id as i16;
                return Some(id);
            }
        }
    }
    None
}

pub fn set_foreground(current_slot: usize, pgid: i32) -> bool {
    if pgid <= 0 { return false; }
    let Some(tty_id) = ensure_controlling_tty(current_slot) else { return false; };
    unsafe {
        let caller_sid = match TASK_MANAGER.tasks[current_slot].as_ref() {
            Some(t) => t.sid,
            None => return false,
        };
        let group_exists = TASK_MANAGER.tasks.iter().flatten().any(|t| {
            !t.zombie && t.pgid == pgid && t.sid == caller_sid
        });
        if !group_exists { return false; }
        let tty = &mut TTYS[tty_id];
        if !tty.allocated || tty.session_id != caller_sid { return false; }
        tty.foreground_pgid = pgid;
        true
    }
}

pub fn foreground(current_slot: usize) -> Option<i32> {
    let tty_id = ensure_controlling_tty(current_slot)?;
    foreground_for_tty(tty_id)
}

pub fn foreground_pty(id: usize) -> Option<i32> {
    let tty_id = {
        let table = PTYS.get().lock();
        table.slots.get(id)?.as_ref()?.tty_id
    };
    foreground_for_tty(tty_id)
}

pub fn set_foreground_pty(current_slot: usize, id: usize, pgid: i32) -> bool {
    if pgid <= 0 || current_slot >= MAX_TASKS as usize { return false; }
    let tty_id = {
        let table = PTYS.get().lock();
        let Some(pty) = table.slots.get(id).and_then(|p| p.as_ref()) else { return false; };
        pty.tty_id
    };
    unsafe {
        let Some(caller) = TASK_MANAGER.tasks[current_slot].as_ref() else { return false; };
        if caller.tty_id != tty_id as i16 { return false; }
        let sid = caller.sid;
        let group_exists = TASK_MANAGER.tasks.iter().flatten().any(|t| {
            !t.zombie && t.sid == sid && t.pgid == pgid
        });
        if !group_exists { return false; }
        let Some(tty) = TTYS.get_mut(tty_id) else { return false; };
        if !tty.allocated || tty.session_id != sid { return false; }
        tty.foreground_pgid = pgid;
        true
    }
}

fn foreground_for_tty(tty_id: usize) -> Option<i32> {
    unsafe {
        let tty = TTYS.get(tty_id)?;
        tty.allocated.then_some(tty.foreground_pgid)
    }
}

pub fn tty_of_task(current_slot: usize) -> Option<i16> {
    ensure_controlling_tty(current_slot).map(|id| id as i16)
}

pub fn alloc_pty(current_slot: usize) -> Option<usize> {
    let tty_id = ensure_controlling_tty(current_slot)?;
    let mut table = PTYS.get().lock();
    for id in 0..MAX_PTYS {
        if table.slots[id].is_none() {
            table.slots[id] = Some(Pty::new(tty_id));
            return Some(id);
        }
    }
    None
}

pub fn add_ref(id: usize, side: PtySide) -> bool {
    let mut table = PTYS.get().lock();
    let Some(pty) = table.slots.get_mut(id).and_then(|p| p.as_mut()) else { return false; };
    match side {
        PtySide::Master => pty.master_refs = pty.master_refs.saturating_add(1),
        PtySide::Slave => pty.slave_refs = pty.slave_refs.saturating_add(1),
    }
    true
}

pub fn close_ref(id: usize, side: PtySide) {
    let mut hangup_tty = None;
    {
        let mut table = PTYS.get().lock();
        let Some(slot) = table.slots.get_mut(id) else { return; };
        let Some(pty) = slot.as_mut() else { return; };
        match side {
            PtySide::Master => {
                let was_open = pty.master_refs != 0;
                pty.master_refs = pty.master_refs.saturating_sub(1);
                if was_open && pty.master_refs == 0 && pty.slave_refs != 0 {
                    // A terminal hangup releases a pending unterminated cooked
                    // line before waking readers and notifying the foreground.
                    if !pty.canonical_input.is_empty() {
                        let pending = core::mem::take(&mut pty.canonical_input);
                        let mut queued = 0usize;
                        for byte in pending {
                            if pty.to_slave.len() >= PTY_BUF { break; }
                            pty.to_slave.push_back(byte);
                            queued += 1;
                        }
                        if queued != 0 { pty.canonical_records.push_back(queued); }
                    }
                    hangup_tty = Some(pty.tty_id);
                }
            }
            PtySide::Slave => pty.slave_refs = pty.slave_refs.saturating_sub(1),
        }
        if pty.master_refs == 0 && pty.slave_refs == 0 {
            *slot = None;
        }
    }
    // Never signal while the PTY table lock is held: signal handling can close
    // descriptors and re-enter the PTY layer.
    if let Some(tty_id) = hangup_tty {
        signal_foreground(tty_id, crate::signal::SIGHUP);
        signal_foreground(tty_id, crate::signal::SIGCONT);
    }
}

pub fn readable(id: usize, side: PtySide) -> bool {
    let table = PTYS.get().lock();
    let Some(pty) = table.slots.get(id).and_then(|p| p.as_ref()) else { return false; };
    match side {
        PtySide::Master => !pty.to_master.is_empty() || pty.slave_refs == 0,
        PtySide::Slave if pty.canonical => {
            !pty.canonical_records.is_empty() || pty.eof_pending || pty.master_refs == 0
        }
        PtySide::Slave => !pty.to_slave.is_empty() || pty.master_refs == 0,
    }
}

pub fn writable(id: usize, side: PtySide) -> bool {
    let table = PTYS.get().lock();
    let Some(pty) = table.slots.get(id).and_then(|p| p.as_ref()) else { return false; };
    match side {
        PtySide::Master => pty.slave_refs > 0 && pty.to_slave.len() < PTY_BUF,
        PtySide::Slave => pty.master_refs > 0 && pty.to_master.len() < PTY_BUF,
    }
}

fn try_read_inner(id: usize, side: PtySide, out: &mut [u8]) -> (usize, bool) {
    let mut table = PTYS.get().lock();
    let Some(pty) = table.slots.get_mut(id).and_then(|p| p.as_mut()) else { return (0, true); };

    // Canonical ^D on an empty line is a one-shot zero-length read, not a
    // permanent hangup. Consume that marker before looking at peer lifetime.
    if side == PtySide::Slave && pty.canonical && pty.canonical_records.is_empty() && pty.eof_pending {
        pty.eof_pending = false;
        return (0, true);
    }

    if side == PtySide::Slave && pty.canonical {
        let Some(remaining) = pty.canonical_records.front().copied() else {
            return (0, pty.master_refs == 0);
        };
        let limit = out.len().min(remaining);
        let mut n = 0usize;
        while n < limit {
            let Some(byte) = pty.to_slave.pop_front() else { break; };
            out[n] = byte;
            n += 1;
        }
        if n == remaining {
            pty.canonical_records.pop_front();
        } else if let Some(front) = pty.canonical_records.front_mut() {
            *front = front.saturating_sub(n);
        }
        return (n, false);
    }

    let (queue, peer_open) = match side {
        PtySide::Master => (&mut pty.to_master, pty.slave_refs > 0),
        PtySide::Slave => (&mut pty.to_slave, pty.master_refs > 0),
    };
    let mut n = 0usize;
    while n < out.len() {
        let Some(byte) = queue.pop_front() else { break; };
        out[n] = byte;
        n += 1;
    }
    (n, !peer_open)
}

fn background_slave_group(current_slot: usize, id: usize) -> Option<i32> {
    let tty_id = {
        let table = PTYS.get().lock();
        table.slots.get(id)?.as_ref()?.tty_id
    };
    let pgid = unsafe { TASK_MANAGER.tasks.get(current_slot)?.as_ref()?.pgid };
    let fg = foreground_for_tty(tty_id)?;
    (pgid > 0 && pgid != fg).then_some(pgid)
}

fn signal_group(pgid: i32, sig: u32) {
    let slots = unsafe { TASK_MANAGER.slots_in_pgid(pgid) };
    for slot in slots {
        let slot_i8 = slot as i8;
        match sig {
            crate::signal::SIGSTOP => { let _ = crate::signal::stop_task(slot_i8); }
            crate::signal::SIGTSTP | crate::signal::SIGTTIN | crate::signal::SIGTTOU => {
                let handler = unsafe {
                    TASK_MANAGER.tasks[slot]
                        .as_ref()
                        .map(|t| t.signal_handlers[(sig - 1) as usize])
                        .unwrap_or(crate::signal::SIG_DFL)
                };
                if handler == crate::signal::SIG_IGN {
                    continue;
                }
                if handler == crate::signal::SIG_DFL {
                    let _ = crate::signal::stop_task_with_signal(slot_i8, sig);
                } else {
                    let _ = crate::signal::send_signal(slot_i8, sig);
                }
            }
            crate::signal::SIGCONT => {
                let handler = unsafe {
                    TASK_MANAGER.tasks[slot]
                        .as_ref()
                        .map(|t| t.signal_handlers[(sig - 1) as usize])
                        .unwrap_or(crate::signal::SIG_DFL)
                };
                let resumed = crate::signal::continue_task(slot_i8);
                if resumed && handler != crate::signal::SIG_DFL && handler != crate::signal::SIG_IGN {
                    let _ = crate::signal::send_signal(slot_i8, sig);
                }
            }
            crate::signal::SIGKILL => { let _ = crate::signal::force_kill(slot_i8, sig); }
            _ => { let _ = crate::signal::send_signal(slot_i8, sig); }
        }
    }
}

pub fn read(current_slot: usize, id: usize, side: PtySide, out: &mut [u8], nonblock: bool) -> usize {
    if out.is_empty() { return 0; }

    let check_background = || -> bool {
        if side != PtySide::Slave { return false; }
        if let Some(pgid) = background_slave_group(current_slot, id) {
            signal_group(pgid, crate::signal::SIGTTIN);
            return true;
        }
        false
    };

    if check_background() { return usize::MAX; }

    // Master reads and canonical slave reads are record/stream blocking reads;
    // VMIN/VTIME only apply to noncanonical slave input.
    let term = if side == PtySide::Slave { termios(id) } else { None };
    if side != PtySide::Slave || term.map_or(true, |t| t.canonical) {
        loop {
            if check_background() { return usize::MAX; }
            let (n, eof) = try_read_inner(id, side, out);
            if n > 0 || eof || nonblock { return n; }
            unsafe { asm!("sti"); asm!("hlt"); asm!("cli"); }
        }
    }

    let term = term.unwrap();
    if nonblock {
        return try_read_inner(id, side, out).0;
    }

    let vmin = (term.cc[VMIN] as usize).min(out.len());
    let vtime_ms = (term.cc[VTIME] as u64).saturating_mul(100);
    if vmin == 0 && vtime_ms == 0 {
        return try_read_inner(id, side, out).0;
    }

    let mut total = 0usize;
    let mut deadline = if vmin == 0 && vtime_ms != 0 {
        Some(crate::time::uptime_ms().saturating_add(vtime_ms))
    } else {
        None
    };

    loop {
        if check_background() { return usize::MAX; }
        let (n, eof) = try_read_inner(id, side, &mut out[total..]);
        if n > 0 {
            total += n;
            if vmin == 0 || total >= vmin || total == out.len() {
                return total;
            }
            // POSIX noncanonical MIN>0,TIME>0: TIME is an inter-byte timer and
            // starts only after the first byte, then restarts after new data.
            if vtime_ms != 0 {
                deadline = Some(crate::time::uptime_ms().saturating_add(vtime_ms));
            }
        }
        if eof { return total; }
        if let Some(end) = deadline {
            if crate::time::uptime_ms() >= end { return total; }
        }
        unsafe { asm!("sti"); asm!("hlt"); asm!("cli"); }
    }
}

fn queue_echo(pty: &mut Pty, bytes: &[u8]) {
    if !pty.echo { return; }
    for &b in bytes {
        if pty.to_master.len() >= PTY_BUF { break; }
        pty.to_master.push_back(b);
    }
}

fn signal_foreground(tty_id: usize, sig: u32) {
    let Some(pgid) = foreground_for_tty(tty_id) else { return; };
    signal_group(pgid, sig);
}

fn commit_canonical(pty: &mut Pty) {
    let pending = core::mem::take(&mut pty.canonical_input);
    let mut queued = 0usize;
    for byte in pending {
        if pty.to_slave.len() >= PTY_BUF { break; }
        pty.to_slave.push_back(byte);
        queued += 1;
    }
    if queued != 0 {
        pty.canonical_records.push_back(queued);
    }
}

fn master_input_byte(pty: &mut Pty, b: u8) -> Option<(usize, u32)> {
    if pty.isig && b == pty.cc[VINTR] && b != 0 {
        pty.canonical_input.clear();
        queue_echo(pty, b"^C\r\n");
        return Some((pty.tty_id, crate::signal::SIGINT));
    }
    if pty.isig && b == pty.cc[VQUIT] && b != 0 {
        pty.canonical_input.clear();
        queue_echo(pty, b"^\\\r\n");
        return Some((pty.tty_id, crate::signal::SIGQUIT));
    }
    if pty.isig && b == pty.cc[VSUSP] && b != 0 {
        pty.canonical_input.clear();
        queue_echo(pty, b"^Z\r\n");
        return Some((pty.tty_id, crate::signal::SIGTSTP));
    }

    let b = if pty.icrnl && b == b'\r' { b'\n' } else { b };
    if pty.canonical {
        if b == pty.cc[VERASE] || (pty.cc[VERASE] == 0x7f && b == 0x08) {
            if pty.canonical_input.pop().is_some() {
                queue_echo(pty, b"\x08 \x08");
            }
            return None;
        }
        if b == pty.cc[VKILL] && b != 0 {
            if !pty.canonical_input.is_empty() {
                pty.canonical_input.clear();
                queue_echo(pty, b"^U\r\n");
            }
            return None;
        }
        if b == pty.cc[VEOF] && b != 0 {
            if pty.canonical_input.is_empty() {
                pty.eof_pending = true;
            } else {
                commit_canonical(pty);
            }
            return None;
        }
        if pty.canonical_input.len() < CANON_BUF {
            pty.canonical_input.push(b);
            queue_echo(pty, &[b]);
        }
        if b == b'\n' {
            commit_canonical(pty);
        }
    } else if pty.to_slave.len() < PTY_BUF {
        pty.to_slave.push_back(b);
        queue_echo(pty, &[b]);
    }
    None
}

pub fn write(current_slot: usize, id: usize, side: PtySide, data: &[u8], nonblock: bool) -> usize {
    if side == PtySide::Slave {
        let tostop = {
            let table = PTYS.get().lock();
            table.slots.get(id).and_then(|p| p.as_ref()).map_or(false, |p| p.tostop)
        };
        if tostop {
            if let Some(pgid) = background_slave_group(current_slot, id) {
                signal_group(pgid, crate::signal::SIGTTOU);
                return usize::MAX;
            }
        }
    }

    let mut written = 0usize;
    while written < data.len() {
        let mut pending_signal = None;
        let progressed = {
            let mut table = PTYS.get().lock();
            let Some(pty) = table.slots.get_mut(id).and_then(|p| p.as_mut()) else { return written; };
            match side {
                PtySide::Master => {
                    if pty.slave_refs == 0 { return written; }
                    let before = pty.to_slave.len();
                    pending_signal = master_input_byte(pty, data[written]);
                    // ISIG consumes the byte; otherwise it is accepted when
                    // queued or buffered canonically.
                    if pending_signal.is_some() || pty.canonical || pty.to_slave.len() > before {
                        true
                    } else {
                        false
                    }
                }
                PtySide::Slave => {
                    if pty.master_refs == 0 { return written; }
                    let byte = data[written];
                    if pty.opost && pty.onlcr && byte == b'\n' {
                        if pty.to_master.len().saturating_add(2) > PTY_BUF {
                            false
                        } else {
                            pty.to_master.push_back(b'\r');
                            pty.to_master.push_back(b'\n');
                            true
                        }
                    } else if pty.to_master.len() >= PTY_BUF {
                        false
                    } else {
                        pty.to_master.push_back(byte);
                        true
                    }
                }
            }
        };

        if let Some((tty_id, sig)) = pending_signal {
            signal_foreground(tty_id, sig);
        }
        if progressed {
            written += 1;
            continue;
        }
        if nonblock { break; }
        unsafe { asm!("sti"); asm!("hlt"); asm!("cli"); }
    }
    written
}

#[derive(Clone, Copy)]
pub struct TermiosState {
    pub canonical: bool,
    pub echo: bool,
    pub isig: bool,
    pub tostop: bool,
    pub icrnl: bool,
    pub opost: bool,
    pub onlcr: bool,
    pub cc: [u8; NCCS],
}

pub fn termios(id: usize) -> Option<TermiosState> {
    let table = PTYS.get().lock();
    let pty = table.slots.get(id)?.as_ref()?;
    Some(TermiosState {
        canonical: pty.canonical,
        echo: pty.echo,
        isig: pty.isig,
        tostop: pty.tostop,
        icrnl: pty.icrnl,
        opost: pty.opost,
        onlcr: pty.onlcr,
        cc: pty.cc,
    })
}

pub fn flush_input(id: usize) -> bool {
    let mut table = PTYS.get().lock();
    let Some(pty) = table.slots.get_mut(id).and_then(|p| p.as_mut()) else { return false; };
    pty.to_slave.clear();
    pty.canonical_input.clear();
    pty.canonical_records.clear();
    pty.eof_pending = false;
    true
}

pub fn set_termios(id: usize, state: TermiosState) -> bool {
    let mut table = PTYS.get().lock();
    let Some(pty) = table.slots.get_mut(id).and_then(|p| p.as_mut()) else { return false; };
    let was_canonical = pty.canonical;
    pty.canonical = state.canonical;
    pty.echo = state.echo;
    pty.isig = state.isig;
    pty.tostop = state.tostop;
    pty.icrnl = state.icrnl;
    pty.opost = state.opost;
    pty.onlcr = state.onlcr;
    pty.cc = state.cc;
    if was_canonical && !state.canonical {
        // Bytes already released as complete canonical records become one raw
        // byte stream, and an unfinished edit buffer is released immediately.
        pty.canonical_records.clear();
        if !pty.canonical_input.is_empty() {
            let pending = core::mem::take(&mut pty.canonical_input);
            for byte in pending {
                if pty.to_slave.len() >= PTY_BUF { break; }
                pty.to_slave.push_back(byte);
            }
        }
        pty.eof_pending = false;
    }
    true
}

// Compatibility helpers for existing kernel callers.
pub fn mode(id: usize) -> Option<(bool, bool, bool)> {
    let t = termios(id)?;
    Some((t.canonical, t.echo, t.isig))
}

pub fn set_mode(id: usize, canonical: bool, echo: bool, isig: bool) -> bool {
    let Some(mut t) = termios(id) else { return false; };
    t.canonical = canonical;
    t.echo = echo;
    t.isig = isig;
    set_termios(id, t)
}
