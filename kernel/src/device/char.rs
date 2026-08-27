use alloc::vec::Vec;
use crate::time::jiffies;

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


pub struct RandomDevice;

impl CharDevice for RandomDevice {
    fn read(&self, _offset: u64, _buf: &mut [u8]) -> usize {
        let mut seed = jiffies();

        for byte in _buf.iter_mut() {
            // Простой генератор Xorshift64
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = seed as u8;
        }

        _buf.len()

    }
    fn write(&self, _offset: u64, buf: &[u8]) -> usize {
        0
    }
}