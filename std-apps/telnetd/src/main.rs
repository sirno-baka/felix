use core::arch::asm;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

const DEFAULT_LISTEN: &str = "0.0.0.0:2323";

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_NAWS: u8 = 31;

const SYS_READ: u32 = 3;
const SYS_WRITE: u32 = 4;
const SYS_CLOSE: u32 = 6;
const SYS_WAITPID: u32 = 7;
const SYS_FCNTL: u32 = 55;
const SYS_SPAWN_PATH: u32 = 0xF002;
const SYS_OPENPTY: u32 = 0xF116;

const F_GETFL: u32 = 3;
const F_SETFL: u32 = 4;
const O_NONBLOCK: u32 = 0x800;
const WNOHANG: u32 = 1;

#[repr(C)]
struct ExecParams {
    stdin: i32,
    stdout: i32,
    stderr: i32,
    argc: u32,
    argv: *const *const u8,
    envc: u32,
    envp: *const *const u8,
    pgid: i32,
    foreground: u32,
}

#[inline]
unsafe fn syscall1(n: u32, a: u32) -> usize {
    let ret: usize;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("eax") n => ret,
            in("ebx") a,
            options(nostack, preserves_flags)
        );
    }
    ret
}

#[inline]
unsafe fn syscall3(n: u32, a: u32, b: u32, c: u32) -> usize {
    let ret: usize;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("eax") n => ret,
            in("ebx") a,
            in("ecx") b,
            in("edx") c,
            options(nostack, preserves_flags)
        );
    }
    ret
}

unsafe fn openpty(fds: &mut [u32; 2]) -> usize {
    unsafe { syscall1(SYS_OPENPTY, fds.as_mut_ptr() as u32) }
}

unsafe fn close_fd(fd: u32) {
    let _ = unsafe { syscall1(SYS_CLOSE, fd) };
}

unsafe fn fd_read(fd: u32, buf: &mut [u8]) -> usize {
    unsafe { syscall3(SYS_READ, fd, buf.as_mut_ptr() as u32, buf.len() as u32) }
}

unsafe fn fd_write(fd: u32, buf: &[u8]) -> usize {
    unsafe { syscall3(SYS_WRITE, fd, buf.as_ptr() as u32, buf.len() as u32) }
}

unsafe fn set_nonblock(fd: u32) -> bool {
    let flags = unsafe { syscall3(SYS_FCNTL, fd, F_GETFL, 0) };
    if flags == usize::MAX {
        return false;
    }
    unsafe { syscall3(SYS_FCNTL, fd, F_SETFL, flags as u32 | O_NONBLOCK) == 0 }
}

unsafe fn waitpid(pid: i32, status: &mut i32, options: u32) -> usize {
    unsafe {
        syscall3(
            SYS_WAITPID,
            pid as u32,
            status as *mut i32 as u32,
            options,
        )
    }
}

unsafe fn spawn_shell(slave: u32) -> Option<i32> {
    static PATH: &[u8] = b"/bin/shell\0";
    static ARG0: &[u8] = b"/bin/shell\0";
    static ENV_PATH: &[u8] = b"PATH=/bin:.\0";
    static ENV_HOME: &[u8] = b"HOME=/home/user\0";
    static ENV_TERM: &[u8] = b"TERM=xterm\0";
    static ENV_SHELL: &[u8] = b"SHELL=/bin/shell\0";

    let argv = [ARG0.as_ptr()];
    let envp = [
        ENV_PATH.as_ptr(),
        ENV_HOME.as_ptr(),
        ENV_TERM.as_ptr(),
        ENV_SHELL.as_ptr(),
    ];
    let params = ExecParams {
        stdin: slave as i32,
        stdout: slave as i32,
        stderr: slave as i32,
        argc: argv.len() as u32,
        argv: argv.as_ptr(),
        envc: envp.len() as u32,
        envp: envp.as_ptr(),
        pgid: 0,
        foreground: 1,
    };

    let pid = unsafe {
        syscall3(
            SYS_SPAWN_PATH,
            PATH.as_ptr() as u32,
            0,
            &params as *const ExecParams as u32,
        )
    };
    (pid != usize::MAX).then_some(pid as i32)
}

#[derive(Clone, Copy)]
enum TelnetState {
    Data,
    Iac,
    Option(u8),
    SubOption,
    SubData,
    SubIac,
}

struct TelnetDecoder {
    state: TelnetState,
    sub_option: u8,
    sub_data: Vec<u8>,
    pending_cr: bool,
    naws: Option<(u16, u16)>,
}

impl TelnetDecoder {
    fn new() -> Self {
        Self {
            state: TelnetState::Data,
            sub_option: 0,
            sub_data: Vec::new(),
            pending_cr: false,
            naws: None,
        }
    }

    fn initial_negotiation(out: &mut Vec<u8>) {
        out.extend_from_slice(&[
            IAC, WILL, OPT_ECHO,
            IAC, WILL, OPT_SGA,
            IAC, DO, OPT_SGA,
            IAC, DO, OPT_NAWS,
        ]);
    }

    fn push_data_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        if self.pending_cr {
            self.pending_cr = false;
            out.push(b'\r');
            if byte == 0 || byte == b'\n' {
                return;
            }
        }

        if byte == b'\r' {
            self.pending_cr = true;
        } else {
            out.push(byte);
        }
    }

    fn finish_subnegotiation(&mut self) {
        if self.sub_option == OPT_NAWS && self.sub_data.len() >= 4 {
            let width = u16::from_be_bytes([self.sub_data[0], self.sub_data[1]]);
            let height = u16::from_be_bytes([self.sub_data[2], self.sub_data[3]]);
            self.naws = Some((width, height));
            // Felix does not have TIOCSWINSZ yet. Keep the last NAWS value so
            // the relay is protocol-correct and can wire it to the ioctl later.
        }
        self.sub_data.clear();
    }

    fn feed(&mut self, byte: u8, data: &mut Vec<u8>, control: &mut Vec<u8>) {
        match self.state {
            TelnetState::Data => {
                if byte == IAC {
                    if self.pending_cr {
                        self.pending_cr = false;
                        data.push(b'\r');
                    }
                    self.state = TelnetState::Iac;
                } else {
                    self.push_data_byte(byte, data);
                }
            }
            TelnetState::Iac => match byte {
                IAC => {
                    self.push_data_byte(IAC, data);
                    self.state = TelnetState::Data;
                }
                DO | DONT | WILL | WONT => self.state = TelnetState::Option(byte),
                SB => self.state = TelnetState::SubOption,
                _ => self.state = TelnetState::Data,
            },
            TelnetState::Option(command) => {
                match command {
                    DO if byte != OPT_ECHO && byte != OPT_SGA => {
                        control.extend_from_slice(&[IAC, WONT, byte]);
                    }
                    DONT if byte == OPT_ECHO || byte == OPT_SGA => {
                        control.extend_from_slice(&[IAC, WONT, byte]);
                    }
                    WILL if byte != OPT_SGA && byte != OPT_NAWS => {
                        control.extend_from_slice(&[IAC, DONT, byte]);
                    }
                    WONT if byte == OPT_SGA || byte == OPT_NAWS => {
                        control.extend_from_slice(&[IAC, DONT, byte]);
                    }
                    _ => {}
                }
                self.state = TelnetState::Data;
            }
            TelnetState::SubOption => {
                self.sub_option = byte;
                self.sub_data.clear();
                self.state = TelnetState::SubData;
            }
            TelnetState::SubData => {
                if byte == IAC {
                    self.state = TelnetState::SubIac;
                } else {
                    self.sub_data.push(byte);
                }
            }
            TelnetState::SubIac => {
                if byte == IAC {
                    self.sub_data.push(IAC);
                    self.state = TelnetState::SubData;
                } else if byte == SE {
                    self.finish_subnegotiation();
                    self.state = TelnetState::Data;
                } else {
                    self.state = TelnetState::SubData;
                }
            }
        }
    }
}

fn append_telnet_data(out: &mut Vec<u8>, data: &[u8]) {
    for &byte in data {
        out.push(byte);
        if byte == IAC {
            out.push(IAC);
        }
    }
}

fn flush_socket(stream: &mut TcpStream, pending: &mut Vec<u8>) -> io::Result<()> {
    while !pending.is_empty() {
        match stream.write(pending) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => {
                pending.drain(..n);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn flush_pty(master: u32, pending: &mut Vec<u8>) {
    while !pending.is_empty() {
        let n = unsafe { fd_write(master, pending) };
        if n == 0 || n == usize::MAX || n > pending.len() {
            break;
        }
        pending.drain(..n);
    }
}

fn relay_session(mut stream: TcpStream) -> io::Result<()> {
    let mut fds = [0u32; 2];
    if unsafe { openpty(&mut fds) } == usize::MAX {
        return Err(io::Error::other("openpty failed"));
    }
    let master = fds[0];
    let slave = fds[1];

    if !unsafe { set_nonblock(master) } {
        unsafe {
            close_fd(slave);
            close_fd(master);
        }
        return Err(io::Error::other("cannot set PTY master nonblocking"));
    }

    let Some(shell_pid) = (unsafe { spawn_shell(slave) }) else {
        unsafe {
            close_fd(slave);
            close_fd(master);
        }
        return Err(io::Error::other("cannot spawn /bin/shell"));
    };
    unsafe {
        close_fd(slave);
    }

    stream.set_nonblocking(true)?;

    let mut decoder = TelnetDecoder::new();
    let mut to_socket = Vec::new();
    let mut to_pty = Vec::new();
    TelnetDecoder::initial_negotiation(&mut to_socket);

    let mut network_buf = [0u8; 2048];
    let mut pty_buf = [0u8; 2048];
    let mut disconnected = false;

    loop {
        flush_socket(&mut stream, &mut to_socket)?;
        flush_pty(master, &mut to_pty);

        loop {
            match stream.read(&mut network_buf) {
                Ok(0) => {
                    disconnected = true;
                    break;
                }
                Ok(n) => {
                    for &byte in &network_buf[..n] {
                        decoder.feed(byte, &mut to_pty, &mut to_socket);
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            break;
        }

        loop {
            let n = unsafe { fd_read(master, &mut pty_buf) };
            if n == 0 || n == usize::MAX || n > pty_buf.len() {
                break;
            }
            append_telnet_data(&mut to_socket, &pty_buf[..n]);
        }

        let mut status = 0i32;
        let waited = unsafe { waitpid(shell_pid, &mut status, WNOHANG) };
        if waited == shell_pid as usize || waited == usize::MAX {
            // Drain output queued just before exit.
            loop {
                let n = unsafe { fd_read(master, &mut pty_buf) };
                if n == 0 || n == usize::MAX || n > pty_buf.len() {
                    break;
                }
                append_telnet_data(&mut to_socket, &pty_buf[..n]);
            }
            let _ = flush_socket(&mut stream, &mut to_socket);
            unsafe {
                close_fd(master);
            }
            return Ok(());
        }

        if to_socket.is_empty() && to_pty.is_empty() {
            thread::sleep(Duration::from_millis(2));
        }
    }

    // Closing the PTY master is the terminal hangup. The kernel sends
    // SIGHUP/SIGCONT to the PTY foreground group; no command-specific kill or
    // Ctrl+C handling belongs in telnetd.
    unsafe {
        close_fd(master);
    }
    let mut status = 0i32;
    let _ = unsafe { waitpid(shell_pid, &mut status, 0) };
    Ok(())
}

fn listen_addr() -> String {
    match std::env::args().nth(1) {
        None => DEFAULT_LISTEN.to_string(),
        Some(arg) if arg.bytes().all(|b| b.is_ascii_digit()) => format!("0.0.0.0:{arg}"),
        Some(arg) => arg,
    }
}

fn main() -> io::Result<()> {
    let addr = listen_addr();
    println!("telnetd: binding {addr}");
    let listener = TcpListener::bind(&addr)?;
    listener.set_nonblocking(true)?;
    println!("telnetd: listening on {addr}");

    loop {
        let (stream, peer) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => {
                println!("telnetd: accept failed: {error:?}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        println!("telnetd: client connected: {peer}");
        if let Err(error) = relay_session(stream) {
            println!("telnetd: session {peer} error: {error:?}");
        }
        println!("telnetd: client disconnected: {peer}");
    }
}
