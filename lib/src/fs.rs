//! Высокоуровневый файловый API для userspace

use crate::syscall::{
    self, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY, S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO,
    S_IFMT, S_IFREG, S_IFSOCK,
};
use alloc::string::String;
use alloc::vec::Vec;

/// Ошибки файловых операций
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    NotFound,
    InvalidFd,
    WriteZero,
    Other(usize),
}

pub type IoResult<T> = Result<T, IoError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Directory,
    Regular,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub inode: u64,
    pub file_type: FileType,
    pub mode: u32,
    pub size: u64,
    pub mtime: u64,
    pub is_mount_point: bool,
}

impl DirEntry {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.file_type == FileType::Directory
    }

    #[inline]
    pub fn is_executable(&self) -> bool {
        self.file_type == FileType::Regular && self.mode & 0o111 != 0
    }
}

fn path_to_cstr(path: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(path.len() + 1);
    v.extend_from_slice(path.as_bytes());
    v.push(0);
    v
}

/// Открытый файл (или сокет, или stdin/stdout)
pub struct File {
    fd: u32,
}

impl File {
    fn from_syscall(path: &str, flags: u32) -> IoResult<Self> {
        let cpath = path_to_cstr(path);
        let fd = unsafe { syscall::open(cpath.as_ptr(), flags) };
        if fd == usize::MAX || (fd as i32) < 0 {
            Err(IoError::NotFound)
        } else {
            Ok(Self { fd: fd as u32 })
        }
    }

    /// Open an existing file (read-write).
    pub fn open(path: &str) -> IoResult<Self> {
        Self::from_syscall(path, O_RDWR)
    }

    pub fn open_ro(path: &str) -> IoResult<Self> {
        Self::from_syscall(path, O_RDONLY)
    }

    /// Create or truncate a file for writing (`O_CREAT | O_WRONLY | O_TRUNC`).
    pub fn create(path: &str) -> IoResult<Self> {
        Self::from_syscall(path, O_CREAT | O_WRONLY | O_TRUNC)
    }

    pub fn open_flags(path: &str, flags: u32) -> IoResult<Self> {
        Self::from_syscall(path, flags)
    }

    /// Создать File из уже известного fd (stdin=0, stdout=1 и т.д.)
    pub fn from_raw_fd(fd: u32) -> Self {
        Self { fd }
    }

    pub fn as_raw_fd(&self) -> u32 {
        self.fd
    }

    /// Прочитать до `buf.len()` байт.
    /// Возвращает количество реально прочитанных байт.
    /// 0 = EOF (или нет данных).
    pub fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = unsafe { syscall::read(self.fd, buf.as_mut_ptr(), buf.len()) };
        // ядро возвращает 0 при EOF / ошибке; usize::MAX при жёсткой ошибке
        if n == usize::MAX {
            Err(IoError::InvalidFd)
        } else {
            Ok(n)
        }
    }

    /// Читать файл целиком до EOF.
    /// Читает кусками по 512 байт, пока read не вернёт 0.
    pub fn read_to_end(&mut self) -> IoResult<Vec<u8>> {
        let mut result = Vec::new();
        let mut chunk = [0u8; 512];

        loop {
            let n = self.read(&mut chunk)?;
            if n == 0 {
                break; // EOF
            }
            result.extend_from_slice(&chunk[..n]);
        }
        Ok(result)
    }

    /// Прочитать ровно `n` байт (или меньше, если EOF раньше).
    pub fn read_exact_or_eof(&mut self, n: usize) -> IoResult<Vec<u8>> {
        let mut result = Vec::with_capacity(n);
        let mut left = n;
        let mut chunk = [0u8; 512];

        while left > 0 {
            let to_read = left.min(chunk.len());
            let got = self.read(&mut chunk[..to_read])?;
            if got == 0 {
                break;
            }
            result.extend_from_slice(&chunk[..got]);
            left -= got;
        }
        Ok(result)
    }

    /// Записать данные.
    pub fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = unsafe { syscall::write(self.fd, buf.as_ptr(), buf.len()) };
        if n == 0 && !buf.is_empty() {
            Err(IoError::WriteZero)
        } else if n == usize::MAX {
            Err(IoError::InvalidFd)
        } else {
            Ok(n)
        }
    }

    /// Записать всю строку / буфер.
    pub fn write_all(&mut self, buf: &[u8]) -> IoResult<()> {
        let mut offset = 0;
        while offset < buf.len() {
            let n = self.write(&buf[offset..])?;
            if n == 0 {
                return Err(IoError::WriteZero);
            }
            offset += n;
        }
        Ok(())
    }

    pub fn write_str(&mut self, s: &str) -> IoResult<()> {
        self.write_all(s.as_bytes())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        // stdin/stdout/stderr не закрываем
        if self.fd > 2 {
            unsafe {
                syscall::close(self.fd);
            }
        }
    }
}

// ====================== Удобные функции ======================

/// Прочитать весь файл в `Vec<u8>`.
pub fn read(path: &str) -> IoResult<Vec<u8>> {
    let mut f = File::open(path)?;
    f.read_to_end()
}

/// Прочитать весь файл как UTF-8 строку.
pub fn read_to_string(path: &str) -> IoResult<String> {
    let data = read(path)?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// Write data to a file, creating it if needed.
pub fn write(path: &str, data: &[u8]) -> IoResult<()> {
    let mut f = File::create(path)?;
    f.write_all(data)
}

/// Создать директорию
pub fn create_dir(path: &str) -> IoResult<()> {
    let cpath = path_to_cstr(path);
    let ret = unsafe { syscall::mkdir(cpath.as_ptr()) };
    if ret == usize::MAX {
        Err(IoError::Other(ret))
    } else {
        Ok(())
    }
}

/// Удалить файл
pub fn remove_file(path: &str) -> IoResult<()> {
    let cpath = path_to_cstr(path);
    let ret = unsafe { syscall::unlink(cpath.as_ptr()) };
    if ret == usize::MAX {
        Err(IoError::NotFound)
    } else {
        Ok(())
    }
}

/// Удалить пустую директорию
pub fn remove_dir(path: &str) -> IoResult<()> {
    let cpath = path_to_cstr(path);
    let ret = unsafe { syscall::rmdir(cpath.as_ptr()) };
    if ret == usize::MAX {
        Err(IoError::NotFound)
    } else {
        Ok(())
    }
}

/// Список имён в директории (сырая строка от SYS_LS).
/// Оставлено для обратной совместимости; новый код должен использовать
/// [`read_dir_entries`].
pub fn read_dir(path: &str) -> IoResult<String> {
    let cpath = path_to_cstr(path);
    let mut buf = [0u8; 4096];
    let n = unsafe { syscall::ls(cpath.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    if n == 0 {
        Err(IoError::NotFound)
    } else {
        Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
    }
}

/// Получить metadata для одного пути через stat64.
pub fn metadata(path: &str) -> IoResult<syscall::Stat64> {
    let cpath = path_to_cstr(path);
    let mut st = syscall::Stat64::default();
    let ret = unsafe { syscall::stat64(cpath.as_ptr(), &mut st) };
    if (ret as i32) < 0 {
        Err(IoError::NotFound)
    } else {
        Ok(st)
    }
}

/// Точки монтирования, известные VFS.
pub fn mount_points() -> Vec<String> {
    let count = unsafe { syscall::mount_list(core::ptr::null_mut(), 0) };
    if count == 0 || (count as i32) < 0 {
        return Vec::new();
    }

    let mut raw = Vec::with_capacity(count);
    raw.resize_with(count, syscall::MountInfo::default);
    let written = unsafe { syscall::mount_list(raw.as_mut_ptr(), raw.len()) };
    if (written as i32) < 0 {
        return Vec::new();
    }

    raw.truncate(written.min(raw.len()));
    raw.into_iter()
        .filter_map(|entry| {
            let len = entry.path.iter().position(|&b| b == 0).unwrap_or(entry.path.len());
            if len == 0 {
                None
            } else {
                Some(String::from_utf8_lossy(&entry.path[..len]).into_owned())
            }
        })
        .collect()
}

/// Структурированное чтение директории через getdents64 + stat64.
///
/// Возвращает имя, inode, тип, mode, размер, mtime и признак mount point.
pub fn read_dir_entries(path: &str) -> IoResult<Vec<DirEntry>> {
    let dir = File::open_ro(path)?;
    let fd = dir.as_raw_fd();
    let mounts = mount_points();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];

    loop {
        let n = unsafe { syscall::getdents64(fd, buf.as_mut_ptr(), buf.len()) };
        if (n as i32) < 0 {
            return Err(IoError::Other(n));
        }
        if n == 0 {
            break;
        }

        let mut pos = 0usize;
        while pos < n {
            if n - pos < 19 {
                return Err(IoError::Other(usize::MAX));
            }

            let p = unsafe { buf.as_ptr().add(pos) };
            let inode = unsafe { core::ptr::read_unaligned(p as *const u64) };
            let reclen = unsafe { core::ptr::read_unaligned(p.add(16) as *const u16) } as usize;
            let dtype = unsafe { *p.add(18) };

            if reclen < 20 || pos + reclen > n {
                return Err(IoError::Other(usize::MAX));
            }

            let name_area = &buf[pos + 19..pos + reclen];
            let name_len = name_area.iter().position(|&b| b == 0).unwrap_or(name_area.len());
            let name = String::from_utf8_lossy(&name_area[..name_len]).into_owned();
            pos += reclen;

            if name.is_empty() || name == "." || name == ".." {
                continue;
            }

            let full_path = join_path(path, &name);
            let stat = metadata(&full_path).ok();

            let (mode, size, mtime, file_type) = if let Some(st) = stat {
                let mode = st.st_mode;
                let raw_size = st.st_size;
                let mtime = st.st_mtime;
                (
                    mode,
                    raw_size.max(0) as u64,
                    mtime as u64,
                    file_type_from_mode(mode),
                )
            } else {
                let fallback = match dtype {
                    4 => FileType::Directory,
                    8 => FileType::Regular,
                    _ => FileType::Unknown,
                };
                (0, 0, 0, fallback)
            };

            out.push(DirEntry {
                name,
                inode,
                file_type,
                mode,
                size,
                mtime,
                is_mount_point: mounts.iter().any(|m| same_path(m, &full_path)),
            });
        }
    }

    Ok(out)
}

fn file_type_from_mode(mode: u32) -> FileType {
    match mode & S_IFMT {
        S_IFDIR => FileType::Directory,
        S_IFREG => FileType::Regular,
        S_IFCHR => FileType::CharDevice,
        S_IFBLK => FileType::BlockDevice,
        S_IFIFO => FileType::Fifo,
        S_IFSOCK => FileType::Socket,
        _ => FileType::Unknown,
    }
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() || parent == "/" {
        let mut out = String::from("/");
        out.push_str(name);
        out
    } else {
        let mut out = String::from(parent.trim_end_matches('/'));
        out.push('/');
        out.push_str(name);
        out
    }
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}
