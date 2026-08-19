use alloc::vec::Vec;

/// Трейт для символьных устройств (потоки байтов, без секторов)
pub trait CharDevice: Send + Sync {
    fn read(&self, offset: u64, buf: &mut [u8]) -> usize;
    fn write(&self, offset: u64, buf: &[u8]) -> usize;
}

/// /dev/null: читает EOF, пишет в никуда
pub struct NullDevice;

impl CharDevice for NullDevice {
    fn read(&self, _offset: u64, _buf: &mut [u8]) -> usize {
        0 // EOF
    }
    fn write(&self, _offset: u64, buf: &[u8]) -> usize {
        buf.len() // Успешно "принимает" все байты
    }
}

/// /dev/zero: читает нули, пишет в никуда
pub struct ZeroDevice;

impl CharDevice for ZeroDevice {
    fn read(&self, _offset: u64, buf: &mut [u8]) -> usize {
        for b in buf.iter_mut() {
            *b = 0;
        }
        buf.len()
    }
    fn write(&self, _offset: u64, buf: &[u8]) -> usize {
        buf.len()
    }
}