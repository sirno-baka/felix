use alloc::vec::Vec;

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

#[derive(Clone, Copy, Debug)]
pub enum FileDescriptor {
    /// Keyboard input (legacy / default stdin)
    ConsoleIn,
    /// VGA/serial text output (default stdout/stderr)
    ConsoleOut,
    File {
        inode: u32,
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
    Device {
        inode: u32,
        offset: u64,
        mode: FileMode,
    },
}

impl FileDescriptor {
    pub fn new_file(inode: u32, mode: FileMode) -> Self {
        Self::File {
            inode,
            offset: 0,
            mode,
        }
    }

    pub fn new_socket(socket_id: usize) -> Self {
        Self::Socket { socket_id }
    }

    pub fn new_pipe(pipe_id: usize, end: PipeEnd) -> Self {
        Self::Pipe { pipe_id, end }
    }

    pub fn is_socket(&self) -> bool {
        matches!(self, Self::Socket { .. })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FileDescriptorTable {
    fds: [Option<FileDescriptor>; 64],
}

impl FileDescriptorTable {
    pub const fn new() -> Self {
        Self { fds: [None; 64] }
    }

    /// Default stdio: 0=ConsoleIn, 1=ConsoleOut, 2=ConsoleOut
    pub fn with_stdio() -> Self {
        let mut t = Self::new();
        t.fds[0] = Some(FileDescriptor::ConsoleIn);
        t.fds[1] = Some(FileDescriptor::ConsoleOut);
        t.fds[2] = Some(FileDescriptor::ConsoleOut);
        t
    }

    /// First free slot starting from 0.
    pub fn alloc_fd(&mut self) -> Option<usize> {
        self.fds.iter().position(|slot| slot.is_none())
    }

    /// Allocate fd >= min (useful to avoid clobbering 0/1/2 accidentally).
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

    pub fn insert(&mut self, fd: usize, desc: FileDescriptor) -> bool {
        if fd < self.fds.len() && self.fds[fd].is_none() {
            self.fds[fd] = Some(desc);
            true
        } else {
            false
        }
    }

    /// Install descriptor at `fd`, replacing any existing entry. Returns the old one.
    pub fn set(&mut self, fd: usize, desc: FileDescriptor) -> Option<FileDescriptor> {
        if fd >= self.fds.len() {
            return None;
        }
        self.fds[fd].replace(desc)
    }

    pub fn close(&mut self, fd: usize) -> Option<FileDescriptor> {
        if fd < self.fds.len() {
            self.fds[fd].take()
        } else {
            None
        }
    }

    pub fn take_all(&mut self) -> impl Iterator<Item = FileDescriptor> + '_ {
        self.fds.iter_mut().filter_map(|s| s.take())
    }

    /// Duplicate descriptor `old` into slot `new` (like dup2).
    pub fn dup2(&mut self, old: usize, new: usize) -> bool {
        if old >= self.fds.len() || new >= self.fds.len() {
            return false;
        }
        let desc = match self.fds[old] {
            Some(d) => d,
            None => return false,
        };
        if old == new {
            return true;
        }
        self.fds[new] = Some(desc);
        true
    }
}
