pub trait BlockDevice: Send {
    fn read_sectors(&self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8>;
    fn write_sectors(&mut self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8>;
    fn sector_size(&self) -> u32;
    /// Total number of addressable sectors, or zero when unavailable.
    fn sector_count(&self) -> u64 { 0 }
}
