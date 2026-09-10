use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::spin::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeEnd {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtySide {
    Master,
    Slave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Block,
    Char,
}

/// Resource identity stored in an fd slot. Mutable open state (file offset,
/// directory cookie and status flags) deliberately lives in a separate shared
/// OpenFileDescription so dup/dup2/exec inheritance have Unix semantics.
#[derive(Clone, Copy, Debug)]
pub enum FileDescriptor {
    ConsoleIn,
    ConsoleOut,
    File {
        inode: u32,
        /// Initial offset only. Once installed in an fd table, use the table's
        /// shared OFD offset helpers rather than mutating this field.
        offset: u64,
        mode: FileMode,
    },
    Socket {
        socket_id: usize,
    },
    Pipe {
        pipe_id: usize,
        end: PipeEnd,
    },
    Pty {
        pty_id: usize,
        side: PtySide,
    },
    Device {
        inode: u32,
        /// Initial offset; live offset is kept in the shared OFD.
        offset: u64,
        mode: FileMode,
        kind: DeviceKind,
    },
    /// Directory for getdents64. The path is immutable resource identity;
    /// `cookie` is only the initial value. Live cookie is shared in the OFD.
    Dir {
        path: [u8; 96],
        path_len: u8,
        cookie: u32,
    },
}

impl FileDescriptor {
    pub fn new_file(inode: u32, mode: FileMode) -> Self {
        Self::File { inode, offset: 0, mode }
    }

    pub fn new_socket(socket_id: usize) -> Self {
        Self::Socket { socket_id }
    }

    pub fn new_pipe(pipe_id: usize, end: PipeEnd) -> Self {
        Self::Pipe { pipe_id, end }
    }

    pub fn new_pty(pty_id: usize, side: PtySide) -> Self {
        Self::Pty { pty_id, side }
    }

    pub fn is_socket(&self) -> bool {
        matches!(self, Self::Socket { .. })
    }

    fn initial_offset(self) -> u64 {
        match self {
            Self::File { offset, .. } | Self::Device { offset, .. } => offset,
            _ => 0,
        }
    }

    fn initial_cookie(self) -> u32 {
        match self {
            Self::Dir { cookie, .. } => cookie,
            _ => 0,
        }
    }
}

pub const O_APPEND: u32 = 0x400;
pub const O_NONBLOCK: u32 = 0x800;
pub const OFD_STATUS_MASK: u32 = O_APPEND | O_NONBLOCK;

/// Unix open-file-description state. Every fd obtained via dup/dup2, and every
/// inherited fd installed by exec, points at the same Arc. Therefore offsets,
/// getdents cookies and F_SETFL status flags are shared exactly once.
#[derive(Debug)]
pub struct OpenFileDescription {
    pub offset: u64,
    pub dir_cookie: u32,
    pub status_flags: u32,
}

impl OpenFileDescription {
    fn from_descriptor(desc: FileDescriptor) -> Self {
        Self {
            offset: desc.initial_offset(),
            dir_cookie: desc.initial_cookie(),
            status_flags: 0,
        }
    }
}

pub type SharedOpenFile = Arc<Mutex<OpenFileDescription>>;

#[derive(Clone, Debug)]
pub struct ClosedFileDescriptor {
    pub desc: FileDescriptor,
    /// True when closing this fd dropped the last reference to its OFD. Pipe,
    /// socket and PTY backing objects must only be released in that case.
    pub last_open_ref: bool,
}

#[derive(Clone, Debug)]
pub struct FileDescriptorTable {
    fds: [Option<FileDescriptor>; 64],
    ofds: [Option<SharedOpenFile>; 64],
}

impl FileDescriptorTable {
    pub const fn new() -> Self {
        Self {
            fds: [None; 64],
            ofds: [const { None }; 64],
        }
    }

    /// Default stdio: 0=ConsoleIn, 1=ConsoleOut, 2=ConsoleOut.
    pub fn with_stdio() -> Self {
        let mut t = Self::new();
        let _ = t.insert(0, FileDescriptor::ConsoleIn);
        let _ = t.insert(1, FileDescriptor::ConsoleOut);
        let _ = t.insert(2, FileDescriptor::ConsoleOut);
        t
    }

    fn new_ofd(desc: FileDescriptor) -> SharedOpenFile {
        Arc::new(Mutex::new(OpenFileDescription::from_descriptor(desc)))
    }

    pub fn get_flags(&self, fd: usize) -> u32 {
        self.ofds
            .get(fd)
            .and_then(|v| v.as_ref())
            .map(|o| o.lock().status_flags)
            .unwrap_or(0)
    }

    pub fn set_flags(&mut self, fd: usize, flags: u32) -> bool {
        let Some(ofd) = self.ofds.get(fd).and_then(|v| v.as_ref()) else {
            return false;
        };
        ofd.lock().status_flags = flags & OFD_STATUS_MASK;
        true
    }

    pub fn is_nonblock(&self, fd: usize) -> bool {
        self.get_flags(fd) & O_NONBLOCK != 0
    }

    pub fn get_offset(&self, fd: usize) -> Option<u64> {
        Some(self.ofds.get(fd)?.as_ref()?.lock().offset)
    }

    pub fn set_offset(&self, fd: usize, offset: u64) -> bool {
        let Some(ofd) = self.ofds.get(fd).and_then(|v| v.as_ref()) else { return false; };
        ofd.lock().offset = offset;
        true
    }

    pub fn advance_offset(&self, fd: usize, amount: u64) -> Option<u64> {
        let ofd = self.ofds.get(fd)?.as_ref()?;
        let mut state = ofd.lock();
        state.offset = state.offset.saturating_add(amount);
        Some(state.offset)
    }

    pub fn get_cookie(&self, fd: usize) -> Option<u32> {
        Some(self.ofds.get(fd)?.as_ref()?.lock().dir_cookie)
    }

    pub fn set_cookie(&self, fd: usize, cookie: u32) -> bool {
        let Some(ofd) = self.ofds.get(fd).and_then(|v| v.as_ref()) else { return false; };
        ofd.lock().dir_cookie = cookie;
        true
    }

    /// First free slot starting from 0.
    pub fn alloc_fd(&mut self) -> Option<usize> {
        self.fds.iter().position(|slot| slot.is_none())
    }

    pub fn alloc_fd_from(&mut self, min: usize) -> Option<usize> {
        for i in min..self.fds.len() {
            if self.fds[i].is_none() {
                return Some(i);
            }
        }
        None
    }

    pub fn get(&self, fd: usize) -> Option<&FileDescriptor> {
        self.fds.get(fd)?.as_ref()
    }

    pub fn get_mut(&mut self, fd: usize) -> Option<&mut FileDescriptor> {
        self.fds.get_mut(fd)?.as_mut()
    }

    /// Snapshot an fd plus its shared OFD for process inheritance.
    pub fn clone_entry(&self, fd: usize) -> Option<(FileDescriptor, SharedOpenFile)> {
        let desc = *self.fds.get(fd)?.as_ref()?;
        let ofd = self.ofds.get(fd)?.as_ref()?.clone();
        Some((desc, ofd))
    }

    pub fn insert(&mut self, fd: usize, desc: FileDescriptor) -> bool {
        if fd >= self.fds.len() || self.fds[fd].is_some() {
            return false;
        }
        self.fds[fd] = Some(desc);
        self.ofds[fd] = Some(Self::new_ofd(desc));
        true
    }

    /// Install a descriptor that shares an existing open-file description.
    pub fn insert_shared(&mut self, fd: usize, desc: FileDescriptor, ofd: SharedOpenFile) -> bool {
        if fd >= self.fds.len() || self.fds[fd].is_some() {
            return false;
        }
        self.fds[fd] = Some(desc);
        self.ofds[fd] = Some(ofd);
        true
    }

    /// Install a fresh descriptor at fd, replacing an existing entry. This is
    /// intended for stdio bootstrap entries where the old descriptor has no
    /// backing resource that needs an explicit release.
    pub fn set(&mut self, fd: usize, desc: FileDescriptor) -> Option<FileDescriptor> {
        if fd >= self.fds.len() {
            return None;
        }
        self.ofds[fd] = Some(Self::new_ofd(desc));
        self.fds[fd].replace(desc)
    }

    pub fn set_shared(&mut self, fd: usize, desc: FileDescriptor, ofd: SharedOpenFile) -> Option<FileDescriptor> {
        if fd >= self.fds.len() {
            return None;
        }
        self.ofds[fd] = Some(ofd);
        self.fds[fd].replace(desc)
    }

    pub fn close(&mut self, fd: usize) -> Option<ClosedFileDescriptor> {
        if fd >= self.fds.len() {
            return None;
        }
        let desc = self.fds[fd].take()?;
        let ofd = self.ofds[fd].take();
        let last_open_ref = ofd
            .as_ref()
            .map(|o| Arc::strong_count(o) == 1)
            .unwrap_or(true);
        drop(ofd);
        Some(ClosedFileDescriptor { desc, last_open_ref })
    }

    pub fn take_all(&mut self) -> Vec<ClosedFileDescriptor> {
        let mut out = Vec::new();
        for fd in 0..self.fds.len() {
            if let Some(closed) = self.close(fd) {
                out.push(closed);
            }
        }
        out
    }

    /// Directory descriptors currently keep their namespace path as immutable
    /// backing identity. Preserve already-open directory fds across a rename by
    /// rewriting that identity while keeping the shared OFD cookie untouched.
    pub fn rewrite_dir_paths(&mut self, old: &str, new: &str) {
        let old_prefix = if old == "/" {
            "/".to_string()
        } else {
            alloc::format!("{}/", old.trim_end_matches('/'))
        };
        for slot in &mut self.fds {
            let Some(FileDescriptor::Dir { path, path_len, .. }) = slot.as_mut() else { continue; };
            let Ok(current) = core::str::from_utf8(&path[..*path_len as usize]) else { continue; };
            let suffix = if current == old {
                Some("")
            } else {
                current.strip_prefix(&old_prefix).map(|s| {
                    // strip_prefix removed the separator as part of old_prefix;
                    // put it back when appending below.
                    s
                })
            };
            let Some(suffix) = suffix else { continue; };
            let replacement = if suffix.is_empty() {
                new.to_string()
            } else if new == "/" {
                alloc::format!("/{}", suffix)
            } else {
                alloc::format!("{}/{}", new.trim_end_matches('/'), suffix)
            };
            let bytes = replacement.as_bytes();
            if bytes.len() >= path.len() { continue; }
            *path = [0; 96];
            path[..bytes.len()].copy_from_slice(bytes);
            *path_len = bytes.len() as u8;
        }
    }

    /// Duplicate old into new. Both fd numbers now reference the same OFD.
    /// Caller must close/release an existing `new` descriptor first.
    pub fn dup2(&mut self, old: usize, new: usize) -> bool {
        if old >= self.fds.len() || new >= self.fds.len() {
            return false;
        }
        let Some(desc) = self.fds[old] else { return false; };
        let Some(ofd) = self.ofds[old].as_ref().cloned() else { return false; };
        if old == new {
            return true;
        }
        self.fds[new] = Some(desc);
        self.ofds[new] = Some(ofd);
        true
    }
}
