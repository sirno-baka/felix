use super::ata::ATAChannel;
use super::{ATAReg, ATAStatus};
use crate::io::{inb, insl, outb};
use crate::time::sleep;

#[derive(Clone, Copy)]
pub struct IDEChannelRegisters {
	pub r#type: ATAChannel, // 0 - Primary Channel, 1 - Secondary Channel
	pub base:   u16,        // I/O Base
	ctrl:       u16,        // ControlBase
	bmide:      u16,        // Bus Master IDE
	pub n_ien:  u8          // nIEN (No Interrupt)
}

impl IDEChannelRegisters {
	pub const fn new(
		channel: ATAChannel,
		base: u16,
		ctrl: u16,
		bmide: u16,
		n_ien: u8
	) -> Self {
		Self { r#type: channel, base, ctrl, bmide, n_ien }
	}

	pub fn read(&mut self, reg: u8) -> u8 {
		let mut result: u8 = 0;
		if reg > 0x07 && reg < 0x0c {
			self.write(ATAReg::CONTROL, 0x80 | self.n_ien);
		}
		if reg < 0x08 {
			result = inb(self.base + reg as u16 - 0x00);
		} else if reg < 0x0c {
			result = inb(self.base + reg as u16 - 0x06);
		} else if reg < 0x0e {
			result = inb(self.ctrl + reg as u16 - 0x0a);
		} else if reg < 0x16 {
			result = inb(self.bmide + reg as u16 - 0x0e);
		}
		if reg > 0x07 && reg < 0x0c {
			self.write(ATAReg::CONTROL, self.n_ien);
		}
		return result;
	}

	pub fn software_reset(&mut self) {
		// 1. Устанавливаем SRST + nIEN
		// Пишем напрямую, минуя логику HOB
		outb(self.ctrl + 2, 0x06);   // 0x3F6 / 0x376

		// Минимальная задержка ~5-10 мкс
		for _ in 0..5 {
			let _ = inb(self.ctrl + 2); // читаем ALTSTATUS
		}

		// 2. Сбрасываем SRST, оставляем nIEN = 1
		outb(self.ctrl + 2, 0x02);

		// 3. Ждём сброса BSY (с жёстким таймаутом)
		let mut timeout = 100_000; // достаточно большой
		while timeout > 0 {
			let status = inb(self.base + 7); // STATUS
			if (status & 0x80) == 0 {        // BSY == 0
				break;
			}
			// маленькая задержка
			let _ = inb(self.ctrl + 2);
			timeout -= 1;
		}
		sleep(5);
	}

	pub fn read_buffer(&mut self, reg: u8, buffer: &mut [u32], quads: u32) {
		if reg > 0x07 && reg < 0x0c {
			self.write(ATAReg::CONTROL, 0x80 | self.n_ien);
		}
		if reg < 0x08 {
			insl(self.base + reg as u16 - 0x00, buffer.as_mut_ptr(), quads);
		} else if reg < 0x0c {
			insl(self.base + reg as u16 - 0x06, buffer.as_mut_ptr(), quads);
		} else if reg < 0x0e {
			insl(self.ctrl + reg as u16 - 0x0a, buffer.as_mut_ptr(), quads);
		} else if reg < 0x16 {
			insl(self.bmide + reg as u16 - 0x0e, buffer.as_mut_ptr(), quads);
		}
		if reg > 0x07 && reg < 0x0c {
			self.write(ATAReg::CONTROL, self.n_ien);
		}
	}

	pub fn write(&mut self, reg: u8, data: u8) {
		if reg > 0x07 && reg < 0x0c {
			self.write(ATAReg::CONTROL, 0x80 | self.n_ien);
		}
		if reg < 0x08 {
			outb(self.base + reg as u16 - 0x00, data);
		} else if reg < 0x0c {
			outb(self.base + reg as u16 - 0x06, data);
		} else if reg < 0x0e {
			outb(self.ctrl + reg as u16 - 0x0a, data);
		} else if reg < 0x16 {
			outb(self.bmide + reg as u16 - 0x0e, data);
		}
		if reg > 0x07 && reg < 0x0c {
			self.write(ATAReg::CONTROL, self.n_ien);
		}
	}

	pub fn polling(&mut self, advanced_check: u32) -> Result<(), u8> {
		for _ in 0..4 {
			self.read(ATAReg::ALTSTATUS);
		}

		let mut timeout = 500; // подбери под свой sleep
		while (self.read(ATAReg::STATUS) & ATAStatus::BSY) != 0 {
			if timeout == 0 {
				return Err(0xFF); // timeout
			}
			// можно sleep(0) или просто крутить
			timeout -= 1;
		}

		if advanced_check != 0 {
			// Read Status Register
			let state: u8 = self.read(ATAReg::STATUS);

			// (III) Check for errors
			if (state & ATAStatus::ERR) != 0 {
				return Err(2);
			}

			// (IV) Check if device fault
			if (state & ATAStatus::DF) != 0 {
				return Err(1);
			}

			// (V) Check DRQ
			// BSY = 0; DF = 0; Err = 0; So we should check for DRQ now
			if (state & ATAStatus::DRQ) == 0 {
				return Err(3);
			}
		}
		// No Error
		Ok(())
	}
}
