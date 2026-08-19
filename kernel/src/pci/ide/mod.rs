use alloc::sync::Arc;
use core::ffi::CStr;
use core::mem::size_of;

use crate::{println, spin};
use crate::spin::{KMutex, Mutex};
use crate::time::sleep;
use crate::utils::arcm::Arcm;
use core::cell::RefCell;

pub mod ata;
pub mod atapi;
pub mod channel;
pub mod device;

use ata::{
	ATAChannel,
	ATACommand,
	ATADirection,
	ATAIdentify,
	ATAReg,
	ATAStatus,
	ATA
};
use atapi::ATAPI;
use channel::IDEChannelRegisters;
pub use device::IDEDevice;
use crate::io::{inb, outb};

static IDE_IRQ_INVOKED: KMutex<u8> = KMutex::<u8>::new(0);
pub static IDE: Mutex<IDEController> =
	Mutex::<IDEController>::new(IDEController::new());

pub enum IDEType {
	ATA   = 0x00,
	ATAPI = 0x01
}

pub struct IDEController {
	devices: [IDEDevice; 4]
}
static mut IDE_BUF: [u8; 2048] = [0; 2048];
impl IDEController {
	/// Create a controller with default devices
	pub const fn new() -> Self {
		Self {
			devices: [
				IDEDevice::new(),
				IDEDevice::new(),
				IDEDevice::new(),
				IDEDevice::new()
			]
		}
	}

	/// Obtain reference to an existing device.
	///
	/// Actually return a reference to the device and should be clone afterward
	/// However if the reference is never needed this function will probably
	/// be change to return a clone of the device instead
	pub fn get_device(&self, num: u8) -> Option<IDEDevice> {
		if num > 3 || self.devices[num as usize].reserved == 0 {
			return None;
		}
		Some(self.devices[num as usize].clone())
	}



	pub unsafe fn initialize(
		&mut self,
	) -> Result<(), u8> {
		unsafe {
			outb(0x3F6, 2);
			inb(0x3F6);
		}
		sleep(1);
		let mut ide_buf: [u8; 2048] = [0; 2048];
		// println!("init");
		// println!("primary");
		let mut primary = IDEChannelRegisters::new(
			ATAChannel::Primary,
			0x1F0,   // base
			0x3F4,   // ctrl
			0,       // bmide (пока не используем)
			0
		);
		// println!("secondary");
		let mut secondary = IDEChannelRegisters::new(
			ATAChannel::Secondary,
			0x170,
			0x374,
			0,
			0
		);
		// println!("channels");
		let mut channels: [Arcm<RefCell<IDEChannelRegisters>>; 2] = [
			Arcm::new(RefCell::new(primary)),
			Arcm::new(RefCell::new(secondary))
		];
		// 2- Disable IRQs
		primary
			.write(ATAReg::CONTROL, 2);
		sleep(5);
		secondary
			.write(ATAReg::CONTROL, 2);
		sleep(5);
		channels[0].lock().borrow_mut().software_reset();
		channels[1].lock().borrow_mut().software_reset();
		sleep(5);
		let mut count: usize = 0;
		// 3- Detect ATA-ATAPI Devices
		for i in 0..2 {

			for j in 0..2 {
				let mut err: u8 = 0;
				let mut r#type: u8 = IDEType::ATA as u8;
				// (I) Select Drive
				channels[i]
					.lock()
					.borrow_mut()
					.write(ATAReg::HDDEVSEL, 0xa0 | (j << 4));
				sleep(5);

				// (II) Send ATA Identify Command
				channels[i]
					.lock()
					.borrow_mut()
					.write(ATAReg::COMMAND, ATACommand::Identify as u8);
				sleep(5);

				// (III) Polling
				// If Status = 0, No Device
				let status = channels[i].lock().borrow_mut().read(ATAReg::STATUS);
				if status == 0x00 || status == 0xFF {
					continue; // нет устройства
				}
				sleep(5);

				loop {
					let status: u8 =
						channels[i].lock().borrow_mut().read(ATAReg::STATUS);
					if (status & ATAStatus::ERR) != 0 {
						err = 1;
						break;
					}
					if ((status & ATAStatus::BSY) == 0)
						&& ((status & ATAStatus::DRQ) != 0)
					{
						break;
					}
				}

				// (IV) Probe for ATAPI Devices
				if err != 0 {
					let cl: u8 =
						channels[i].lock().borrow_mut().read(ATAReg::LBA1);
					let ch: u8 =
						channels[i].lock().borrow_mut().read(ATAReg::LBA2);

					if cl == 0x14 && ch == 0xeb {
						r#type = IDEType::ATAPI as u8;
					} else if cl == 0x69 && ch == 0x96 {
						r#type = IDEType::ATAPI as u8;
					} else {
						// Unknown Type (may not be a device)
						continue;
					}

					channels[i].lock().borrow_mut().write(
						ATAReg::COMMAND,
						ATACommand::IdentifyPacket as u8
					);
					sleep(1);
				}

				// (V) Read Identification Space of the Device
				channels[i].lock().borrow_mut().read_buffer(
					ATAReg::DATA,
					unsafe { ide_buf.align_to_mut::<u32>().1 },
					128
				);

				// (VI) Read Device Parameters
				self.devices[count].reserved = 1;
				self.devices[count].r#type = r#type as u16;
				self.devices[count].channel = Some(channels[i].clone());
				self.devices[count].drive = j;
				self.devices[count].signature = u16::from_le_bytes(
					ide_buf[ATAIdentify::DEVICETYPE
						..ATAIdentify::DEVICETYPE + size_of::<u16>()]
						.try_into()
						.unwrap()
				);
				self.devices[count].capabilities = u16::from_le_bytes(
					ide_buf[ATAIdentify::CAPABILITIES
						..ATAIdentify::CAPABILITIES + size_of::<u16>()]
						.try_into()
						.unwrap()
				);
				self.devices[count].command_sets = u32::from_le_bytes(
					ide_buf[ATAIdentify::COMMANDSETS
						..ATAIdentify::COMMANDSETS + size_of::<u32>()]
						.try_into()
						.unwrap()
				);

				if self.devices[count].r#type == IDEType::ATA as u16 {
					// (VII) Get Size
					if (self.devices[count].command_sets & (1 << 26)) != 0 {
						// Device uses 48-Bit Addressing
						self.devices[count].size = u32::from_le_bytes(
							ide_buf[ATAIdentify::MAX_LBA_EXT
								..ATAIdentify::MAX_LBA_EXT + size_of::<u32>()]
								.try_into()
								.unwrap()
						);
					} else {
						// Device uses CHS or 28-Bit Addressing
						self.devices[count].size = u32::from_le_bytes(
							ide_buf[ATAIdentify::MAX_LBA
								..ATAIdentify::MAX_LBA + size_of::<u32>()]
								.try_into()
								.unwrap()
						);
					}
				} else {
					let device: &mut IDEDevice = &mut self.devices[count];
					match ATAPI::capacity(device, 0) {
						Ok(cap) => {self.devices[count].size = cap}
						Err(_) => {}
					};
				}

				// (VIII) String indicates model of device
				for k in (0..40).step_by(2) {
					self.devices[count].model[k] =
						ide_buf[ATAIdentify::MODEL as usize + k + 1];
					self.devices[count].model[k + 1] =
						ide_buf[ATAIdentify::MODEL as usize + k];
				}
				self.devices[count].model[40] = 0;

				count += 1;
			}
		}

		// 4- Print Summary
		for i in 0..4 {
			if self.devices[i].reserved == 1 {
				println!(
					"Found {} Drive {:.2}MB - {}",
					["ATA", "ATAPI"][self.devices[i].r#type as usize],
					self.devices[i].size as f32 / 1024.0 / 2.0,
					CStr::from_bytes_until_nul(&self.devices[i].model)
						.unwrap()
						.to_str()
						.unwrap()
				);
			}
		}

		Ok(())
	}

	fn wait_irq() {
		loop {
			if *IDE_IRQ_INVOKED.lock() != 0 {
				break;
			}
		}
		*IDE_IRQ_INVOKED.lock() = 0;
	}

	pub fn irq() {
		*IDE_IRQ_INVOKED.lock() = 1;
	}

	fn read(channel: &mut IDEChannelRegisters, reg: u8) -> u8 {
		channel.read(reg)
	}

	fn write(channel: &mut IDEChannelRegisters, reg: u8, data: u8) {
		channel.write(reg, data);
	}

	fn read_buffer(
		channel: &mut IDEChannelRegisters,
		reg: u8,
		buffer: &mut [u32],
		quads: u32
	) {
		channel.read_buffer(reg, buffer, quads);
	}

	fn polling(
		channel: &mut IDEChannelRegisters,
		advanced_check: u32
	) -> Result<(), u8> {
		channel.polling(advanced_check)
	}

	/// Read sector from a drive
	///
	/// Parameters:
	/// + drive: drive number which can be from 0 to 3
	/// + numsects: number of sectors to be read. If 0, the ATA controller will now we want 256 sectors
	/// + lba: LBA address --> index of the sector, which allows us to acces disks up to 2TB
	/// + edi: adress of the buffer we want to fill
	pub fn read_sectors(
		&mut self,
		drive: u8,
		numsects: u8,
		lba: u32,
		edi: u32
	) -> Result<(), u8> {
		let device: &mut IDEDevice = &mut self.devices[drive as usize];

		// 1- Check if the drive presents
		if drive > 3 || device.reserved == 0 {
			// Drive not found
			return Err(0x1);
		// 2- Check if inputs are valid
		} else if (lba + numsects as u32 > device.size)
			&& (device.r#type == IDEType::ATA as u16)
		{
			// Seeking to invalid position
			return Err(0x2);
		// 3- Read in PIO Mode through Polling & IRQs
		} else {
			if device.r#type == IDEType::ATA as u16 {
				match ATA::access(
					ATADirection::Read as u8,
					device,
					lba,
					numsects,
					edi
				) {
					Ok(_) => {},
					Err(err) => return Err(device.print_error(err))
				}
			} else if device.r#type == IDEType::ATAPI as u16 {
				for i in 0..numsects {
					match ATAPI::read(
						device,
						lba + i as u32,
						1,
						edi + i as u32 * atapi::SECTOR_SIZE
					) {
						Ok(_) => {},
						Err(err) => return Err(device.print_error(err))
					}
				}
			}
		}
		Ok(())
	}

	/// Read sector from a drive
	///
	/// Parameters:
	/// + drive: drive number which can be from 0 to 3
	/// + numsects: number of sectors to write. If 0, the ATA controller will now we want 256 sectors
	/// + lba: LBA address --> index of the sector, which allows us to access disks up to 2TB
	/// + edi: adress of the buffer we want to fill
	pub fn write_sectors(
		&mut self,
		drive: u8,
		numsects: u8,
		lba: u32,
		edi: u32
	) -> Result<(), u8> {
		let device: &mut IDEDevice = &mut self.devices[drive as usize];

		// 1- Check if the drive presents
		if drive > 3 || device.reserved == 0 {
			// Drive not found
			return Err(0x1);
		// 2- Check if inputs are valid
		} else if (lba + numsects as u32 > device.size)
			&& (device.r#type == IDEType::ATA as u16)
		{
			return Err(0x2);
		// 3- Read in PIO Mode through Polling & IRQs
		} else {
			if device.r#type == IDEType::ATA as u16 {
				match ATA::access(
					ATADirection::Write as u8,
					device,
					lba,
					numsects,
					edi
				) {
					Ok(_) => {},
					Err(err) => return Err(device.print_error(err))
				}
			} else if device.r#type == IDEType::ATAPI as u16 {
				// Write-Protected
				return Err(0x4);
			}
		}
		Ok(())
	}
}

