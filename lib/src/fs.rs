//! Высокоуровневый файловый API для userspace

use alloc::string::String;
use alloc::vec::Vec;
use crate::syscall;

/// Ошибки файловых операций
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    NotFound,
    InvalidFd,
    WriteZero,
    Other(usize),
}

pub type IoResult<T> = Result<T, IoError>;

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
    /// Открыть файл по пути.
    /// flags пока игнорируются ядром, но передаём 0.
    pub fn open(path: &str) -> IoResult<Self> {
        let cpath = path_to_cstr(path);
        let fd = unsafe { syscall::open(cpath.as_ptr(), 0) };
        // Linux-style: negative = -errno; also accept legacy usize::MAX
        if fd == usize::MAX || (fd as i32) < 0 {
            Err(IoError::NotFound)
        } else {
            Ok(Self { fd: fd as u32 })
        }
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
            unsafe { syscall::close(self.fd); }
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

/// Записать данные в файл (перезаписывает).
pub fn write(path: &str, data: &[u8]) -> IoResult<()> {
    // пока у ядра нет O_CREAT/O_TRUNC — просто open + write
    let mut f = File::open(path)?;
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

/// Список имён в директории (сырая строка от SYS_LS)
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