use crate::io::outb;

const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CHANNEL0_DATA_PORT: u16 = 0x40;
const PIT_BASE_FREQUENCY: u32 = 1_193_182;

/// Инициализация PIT на заданную частоту
pub fn init(frequency: u32) {
    let divisor = PIT_BASE_FREQUENCY / frequency;

    // Команда: Канал 0, доступ младший/старший байт, Режим 2 (Rate Generator), Бинарный
    let command: u8 = 0x34;

    unsafe {
        outb(PIT_COMMAND_PORT, command);
        outb(PIT_CHANNEL0_DATA_PORT, (divisor & 0xFF) as u8);
        outb(PIT_CHANNEL0_DATA_PORT, ((divisor >> 8) & 0xFF) as u8);
    }
}
