use crate::io::outb;

const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CHANNEL0_DATA_PORT: u16 = 0x40;
const PIT_BASE_FREQUENCY: u32 = 1_193_182;
const PIT_MIN_DIVISOR: u32 = 1;
const PIT_MAX_DIVISOR: u32 = 0xFFFF;

/// Инициализация PIT на заданную частоту.
///
/// PIT принимает не частоту, а 16-битный делитель, поэтому фактическая частота
/// почти всегда немного отличается от requested frequency. Сохраняем именно
/// реально запрограммированный делитель в time.rs.
pub fn init(frequency: u32) {
    assert!(frequency != 0, "PIT frequency must be non-zero");

    let divisor = (PIT_BASE_FREQUENCY / frequency).clamp(PIT_MIN_DIVISOR, PIT_MAX_DIVISOR);

    // Канал 0, lobyte/hibyte, mode 2 (rate generator), binary.
    let command: u8 = 0x34;

    unsafe {
        outb(PIT_COMMAND_PORT, command);
        outb(PIT_CHANNEL0_DATA_PORT, (divisor & 0xFF) as u8);
        outb(PIT_CHANNEL0_DATA_PORT, ((divisor >> 8) & 0xFF) as u8);
    }

    crate::time::set_pit_divisor(divisor);
}
