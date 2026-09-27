use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
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

#[derive(Clone, Copy)]
enum TelnetState {
    Data,
    Iac,
    Option(u8),
    Subnegotiation,
    SubnegotiationIac,
}

struct TelnetDecoder {
    state: TelnetState,
}

impl TelnetDecoder {
    fn new() -> Self {
        Self {
            state: TelnetState::Data,
        }
    }

    fn feed(&mut self, byte: u8, stream: &mut TcpStream, out: &mut Vec<u8>) {
        match self.state {
            TelnetState::Data => {
                if byte == IAC {
                    self.state = TelnetState::Iac;
                } else {
                    out.push(byte);
                }
            }
            TelnetState::Iac => match byte {
                IAC => {
                    out.push(IAC);
                    self.state = TelnetState::Data;
                }
                DO | DONT | WILL | WONT => {
                    self.state = TelnetState::Option(byte);
                }
                SB => {
                    self.state = TelnetState::Subnegotiation;
                }
                _ => {
                    self.state = TelnetState::Data;
                }
            },
            TelnetState::Option(command) => {
                let response = match command {
                    DO => Some(WONT),
                    WILL => Some(DONT),
                    DONT | WONT => None,
                    _ => None,
                };
                if let Some(response) = response {
                    let _ = stream.write(&[IAC, response, byte]);
                }
                self.state = TelnetState::Data;
            }
            TelnetState::Subnegotiation => {
                if byte == IAC {
                    self.state = TelnetState::SubnegotiationIac;
                }
            }
            TelnetState::SubnegotiationIac => {
                if byte == SE {
                    self.state = TelnetState::Data;
                } else if byte != IAC {
                    self.state = TelnetState::Subnegotiation;
                }
            }
        }
    }
}

struct Session {
    stream: TcpStream,
    telnet: TelnetDecoder,
    cwd: PathBuf,
    skip_lf: bool,
}

impl Session {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            telnet: TelnetDecoder::new(),
            cwd: PathBuf::from("/"),
            skip_lf: false,
        }
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.stream.write_all(data)
    }

    fn write_text(&mut self, text: &str) -> io::Result<()> {
        self.write_all(text.as_bytes())
    }

    fn prompt(&mut self) -> io::Result<()> {
        let cwd = self.cwd.to_string_lossy();
        self.write_text(&format!("felix:{}$ ", cwd))
    }

    fn read_line(&mut self) -> io::Result<Option<String>> {
        self.stream.set_nonblocking(false)?;
        let mut line = Vec::new();
        let mut raw = [0u8; 256];

        loop {
            let n = self.stream.read(&mut raw)?;
            if n == 0 {
                return Ok(None);
            }

            let mut decoded = Vec::with_capacity(n);
            for &byte in &raw[..n] {
                self.telnet.feed(byte, &mut self.stream, &mut decoded);
            }

            for byte in decoded {
                if self.skip_lf {
                    self.skip_lf = false;
                    if byte == b'\n' || byte == 0 {
                        continue;
                    }
                }

                match byte {
                    b'\r' => {
                        self.skip_lf = true;
                        self.write_all(b"\r\n")?;
                        return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
                    }
                    b'\n' => {
                        self.write_all(b"\r\n")?;
                        return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
                    }
                    0x08 | 0x7f => {
                        if line.pop().is_some() {
                            self.write_all(b"\x08 \x08")?;
                        }
                    }
                    0x03 => {
                        line.clear();
                        self.write_all(b"^C\r\n")?;
                        return Ok(Some(String::new()));
                    }
                    byte if byte >= 0x20 || byte == b'\t' => {
                        line.push(byte);
                        self.write_all(&[byte])?;
                    }
                    _ => {}
                }
            }
        }
    }

    fn run(&mut self) -> io::Result<()> {
        self.write_all(b"FELIX TELNET READY\r\n")?;
        self.write_all(b"raw TCP/telnet shell; type 'help' for commands\r\n")?;

        loop {
            self.prompt()?;
            let Some(line) = self.read_line()? else {
                return Ok(());
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let words = match split_words(line) {
                Ok(words) => words,
                Err(error) => {
                    self.write_text(&format!("parse error: {error}\r\n"))?;
                    continue;
                }
            };
            if words.is_empty() {
                continue;
            }

            match words[0].as_str() {
                "exit" | "quit" => {
                    self.write_all(b"bye\r\n")?;
                    return Ok(());
                }
                "help" => {
                    self.write_all(
                        b"telnetd builtins: help, exit, pwd, cd [dir], echo ...\r\n\
external commands are loaded from /bin; simple quotes and backslashes are supported\r\n\
while a command runs, input is forwarded to its stdin and Ctrl+C kills it\r\n",
                    )?;
                }
                "pwd" => {
                    self.write_text(&format!("{}\r\n", self.cwd.display()))?;
                }
                "cd" => {
                    let target = words.get(1).map(String::as_str).unwrap_or("/");
                    let path = resolve_path(&self.cwd, target);
                    match fs::metadata(&path) {
                        Ok(meta) if meta.is_dir() => {
                            self.cwd = normalize_path(&path);
                        }
                        Ok(_) => self.write_text("cd: not a directory\r\n")?,
                        Err(error) => self.write_text(&format!("cd: {error}\r\n"))?,
                    }
                }
                "echo" => {
                    self.write_text(&words[1..].join(" "))?;
                    self.write_all(b"\r\n")?;
                }
                _ => {
                    if !self.run_command(&words)? {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn run_command(&mut self, words: &[String]) -> io::Result<bool> {
        let program = if words[0].contains('/') {
            resolve_path(&self.cwd, &words[0])
        } else {
            PathBuf::from("/bin").join(&words[0])
        };

        let mut command = Command::new(&program);
        command
            .args(&words[1..])
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.write_text(&format!("{}: {error}\r\n", words[0]))?;
                return Ok(true);
            }
        };

        let mut child_stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_thread = match (stdout, self.stream.try_clone()) {
            (Some(stdout), Ok(stream)) => Some(relay_output(stdout, stream)),
            _ => None,
        };
        let stderr_thread = match (stderr, self.stream.try_clone()) {
            (Some(stderr), Ok(stream)) => Some(relay_output(stderr, stream)),
            _ => None,
        };

        self.stream.set_nonblocking(true)?;

        let mut disconnected = false;
        let mut interrupted = false;
        let mut raw = [0u8; 512];

        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }

            match self.stream.read(&mut raw) {
                Ok(0) => {
                    disconnected = true;
                    let _ = child.kill();
                    break wait_after_kill(&mut child)?;
                }
                Ok(n) => {
                    let mut decoded = Vec::with_capacity(n);
                    for &byte in &raw[..n] {
                        self.telnet.feed(byte, &mut self.stream, &mut decoded);
                    }

                    if decoded.contains(&0x03) {
                        interrupted = true;
                        let _ = child.kill();
                        break wait_after_kill(&mut child)?;
                    }

                    if let Some(stdin) = child_stdin.as_mut() {
                        if let Err(error) = stdin.write_all(&decoded) {
                            if error.kind() != ErrorKind::BrokenPipe {
                                let _ = child.kill();
                                break wait_after_kill(&mut child)?;
                            }
                        }
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => {
                    disconnected = true;
                    let _ = child.kill();
                    break wait_after_kill(&mut child)?;
                }
            }

            thread::sleep(Duration::from_millis(5));
        };

        drop(child_stdin);
        self.stream.set_nonblocking(false)?;

        if let Some(handle) = stdout_thread {
            let _ = handle.join();
        }
        if let Some(handle) = stderr_thread {
            let _ = handle.join();
        }

        if disconnected {
            return Ok(false);
        }

        if interrupted {
            self.write_all(b"^C\r\n")?;
        } else if !status.success() {
            match status.code() {
                Some(code) => self.write_text(&format!("[exit {code}]\r\n"))?,
                None => self.write_all(b"[terminated]\r\n")?,
            }
        }

        Ok(true)
    }
}

fn wait_after_kill(child: &mut Child) -> io::Result<ExitStatus> {
    child.wait()
}

fn write_relay_all(stream: &mut TcpStream, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => data = &data[written..],
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            // run_command() makes its control fd nonblocking. TcpStream clones
            // are dup() fds that share the same open-file description, so the
            // stdout/stderr relay becomes nonblocking too. Backpressure is not
            // a disconnect: keep draining the child pipe and retry the socket.
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn relay_output<R>(mut reader: R, mut stream: TcpStream) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0u8; 2048];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if write_relay_all(&mut stream, &buffer[..n]).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut parts: Vec<&str> = Vec::new();
    let text = path.to_string_lossy();

    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }

    if parts.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(format!("/{}", parts.join("/")))
    }
}

fn split_words(line: &str) -> Result<Vec<String>, &'static str> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();
    let mut quote = None;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }

        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }
    if quote.is_some() {
        return Err("unterminated quote");
    }
    if !current.is_empty() {
        words.push(current);
    }

    Ok(words)
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
    println!("telnetd: listening on {}", listener.local_addr()?);

    loop {
        let (stream, peer) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) => {
                println!("telnetd: accept failed: {error:?}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        println!("telnetd: client connected: {peer}");
        let mut session = Session::new(stream);
        if let Err(error) = session.run() {
            println!("telnetd: session {peer} error: {error:?}");
        }
        println!("telnetd: client disconnected: {peer}");
    }
}
