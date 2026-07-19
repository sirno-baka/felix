use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Clone, Debug, Copy)]
pub struct FileDescriptor {
    pub inode: u32,
    pub offset: u64,
    pub mode: FileMode,
}

impl FileDescriptor {
    pub fn new(inode: u32, mode: FileMode) -> Self {
        Self {
            inode,
            offset: 0,
            mode,
        }
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

    /// Находит свободный дескриптор
    pub fn alloc_fd(&mut self) -> Option<usize> {
        if let Some(fd) = self.fds.iter().position(|slot| slot.is_none()) {
            return Some(fd + 5);
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

    pub fn close(&mut self, fd: usize) -> bool {
        if fd < self.fds.len() {
            self.fds[fd] = None;
            true
        } else {
            false
        }
    }
}