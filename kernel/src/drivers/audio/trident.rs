use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;
use core::ptr::{copy_nonoverlapping, null_mut};

use crate::io::{inb, inl, inw, outb, outl, outw};
use crate::pci::bar::Bar;
use crate::pci::device::PciDevice;
use crate::sync::mutex::Mutex;

const PCI_VENDOR_TRIDENT: u16 = 0x1023;
const PCI_VENDOR_SIS: u16 = 0x1039;
const PCI_VENDOR_SIS_OLD_HEADER: u16 = 0x0139; // value used by the supplied 2.2-era header
const PCI_VENDOR_ALI: u16 = 0x10b9;

const PCI_DEVICE_TRIDENT_DX: u16 = 0x2000;
const PCI_DEVICE_TRIDENT_NX: u16 = 0x2001;
const PCI_DEVICE_SIS_7018: u16 = 0x7018;
const PCI_DEVICE_ALI_5451: u16 = 0x5451;

const T4D_REC_CH: u16 = 0x70;
const T4D_START_A: u16 = 0x80;
const T4D_STOP_A: u16 = 0x84;
const T4D_AINT_A: u16 = 0x98;
const T4D_LFO_GC_CIR: u16 = 0xa0;
const T4D_AINTEN_A: u16 = 0xa4;
const T4D_MUSICVOL_WAVEVOL: u16 = 0xa8;
const T4D_MISCINT: u16 = 0xb0;
const T4D_START_B: u16 = 0xb4;
const T4D_STOP_B: u16 = 0xb8;
const T4D_AINT_B: u16 = 0xd8;
const T4D_AINTEN_B: u16 = 0xdc;

const CHANNEL_START: u16 = 0xe0;
const CH_DX_CSO_ALPHA_FMS: u16 = 0xe0;
const CH_NX_DELTA_CSO: u16 = 0xe0;

const DX_ACR0_AC97_W: u16 = 0x40;
const DX_ACR1_AC97_R: u16 = 0x44;
const DX_ACR2_AC97_COM_STAT: u16 = 0x48;
const NX_ACR0_AC97_COM_STAT: u16 = 0x40;
const NX_ACR1_AC97_W: u16 = 0x44;
const NX_ACR2_AC97_R_PRIMARY: u16 = 0x48;
const SI_AC97_WRITE: u16 = 0x40;
const SI_AC97_READ: u16 = 0x44;
const SI_SERIAL_INTF_CTRL: u16 = 0x48;
const SI_AC97_GPIO: u16 = 0x4c;
const ALI_SCTRL: u16 = 0x48;
const ALI_AC97_WRITE: u16 = 0x40;
const ALI_AC97_READ: u16 = 0x44;
const ALI_GLOBAL_CONTROL: u16 = 0xd4;
const ALI_STIMER: u16 = 0xc8;

const DX_AC97_BUSY_WRITE: u32 = 0x8000;
const DX_AC97_BUSY_READ: u32 = 0x8000;
const DX_AC97_PLAYBACK: u32 = 0x0002;
const NX_AC97_BUSY_WRITE: u32 = 0x0800;
const NX_AC97_BUSY_READ: u32 = 0x0800;
const NX_AC97_BUSY_DATA: u32 = 0x0400;
const NX_AC97_PCM_OUTPUT: u32 = 0x0002;
const SI_AC97_BUSY_WRITE: u32 = 0x8000;
const SI_AC97_BUSY_READ: u32 = 0x8000;
const SI_AC97_AUDIO_BUSY: u32 = 0x4000;
const ALI_AC97_BUSY_WRITE: u32 = 0x8000;
const ALI_AC97_BUSY_READ: u32 = 0x8000;
const ALI_AC97_AUDIO_BUSY: u32 = 0x4000;
const ALI_AC97_WRITE_ACTION: u32 = 0x8000;
const ALI_AC97_READ_ACTION: u32 = 0x8000;
const ALI_AC97_WRITE_MIXER_REGISTER: u32 = 0x0100;
const ALI_AC97_READ_MIXER_REGISTER: u32 = 0xfeff;

const PCMOUT: u32 = 0x0001_0000;
const SURROUT: u32 = 0x0002_0000;
const CENTEROUT: u32 = 0x0004_0000;
const LFEOUT: u32 = 0x0008_0000;
const SECONDARY_ID: u32 = 0x0000_4000;

const CHANNEL_LOOP: u32 = 0x0000_1000;
const CHANNEL_SIGNED: u32 = 0x0000_2000;
const CHANNEL_STEREO: u32 = 0x0000_4000;
const CHANNEL_16BITS: u32 = 0x0000_8000;

const AC97_RESET: u8 = 0x00;
const AC97_MASTER_VOLUME: u8 = 0x02;
const AC97_PCM_OUT_VOLUME: u8 = 0x18;

const DMA_ALIGN: usize = 4096;
const MAX_DMA_BYTES: usize = 128 * 1024;
const KERNEL_OFFSET: usize = crate::KERNEL_OFFSET as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    TridentDx,
    TridentNx,
    Sis7018,
    Ali5451,
}

impl Chip {
    fn name(self) -> &'static str {
        match self {
            Chip::TridentDx => "Trident 4DWave DX",
            Chip::TridentNx => "Trident 4DWave NX",
            Chip::Sis7018 => "SiS 7018",
            Chip::Ali5451 => "ALi M5451",
        }
    }
}

pub struct TridentAudio {
    chip: Chip,
    io: u16,
    irq: u8,
    revision: u8,
    playback_channel: u8,
}

unsafe impl Send for TridentAudio {}

pub static AUDIO: Mutex<Option<TridentAudio>> = Mutex::new(None);

fn identify(dev: &PciDevice) -> Option<Chip> {
    match (dev.vendor_id, dev.device_id) {
        (PCI_VENDOR_TRIDENT, PCI_DEVICE_TRIDENT_DX) => Some(Chip::TridentDx),
        (PCI_VENDOR_TRIDENT, PCI_DEVICE_TRIDENT_NX) => Some(Chip::TridentNx),
        (PCI_VENDOR_SIS, PCI_DEVICE_SIS_7018) | (PCI_VENDOR_SIS_OLD_HEADER, PCI_DEVICE_SIS_7018) => Some(Chip::Sis7018),
        (PCI_VENDOR_ALI, PCI_DEVICE_ALI_5451) => Some(Chip::Ali5451),
        _ => None,
    }
}

pub fn init() {
    let dev = crate::pci::enumerate().into_iter().find(|d| identify(d).is_some());
    let Some(dev) = dev else {
        crate::println!("[audio] no Trident/SiS/ALi audio controller found");
        return;
    };

    let Some(chip) = identify(&dev) else { return };
    let io = match dev.get_bar(0) {
        Some(Bar::Io { address, .. }) if *address <= u16::MAX as u32 => *address as u16,
        Some(_) => {
            crate::println!("[audio] {} BAR0 is not an I/O BAR", chip.name());
            return;
        }
        None => {
            crate::println!("[audio] {} has no BAR0", chip.name());
            return;
        }
    };

    dev.enable_io_space();
    dev.enable_bus_mastering();

    let mut card = TridentAudio {
        chip,
        io,
        irq: dev.interrupt_line,
        revision: dev.revision_id,
        playback_channel: if chip == Chip::Ali5451 { 0 } else { 63 },
    };

    if !card.init_ac97() {
        crate::println!("[audio] {} AC97 initialization failed", chip.name());
        return;
    }

    card.write32(T4D_MUSICVOL_WAVEVOL, 0);
    card.ac97_write(AC97_RESET, 0);
    // 0x0000 = 0 dB attenuation on standard AC'97 mixers.
    card.ac97_write(AC97_MASTER_VOLUME, 0x0000);
    card.ac97_write(AC97_PCM_OUT_VOLUME, 0x0000);

    crate::println!(
        "[audio] {} at I/O {:#06x}, IRQ {}, rev {:#04x} (polling)",
        chip.name(), io, card.irq, card.revision
    );
    *AUDIO.lock() = Some(card);
}

impl TridentAudio {
    #[inline]
    fn port(&self, reg: u16) -> u16 {
        self.io.wrapping_add(reg)
    }

    #[inline]
    fn read8(&self, reg: u16) -> u8 { inb(self.port(reg)) }
    #[inline]
    fn read16(&self, reg: u16) -> u16 { inw(self.port(reg)) }
    #[inline]
    fn read32(&self, reg: u16) -> u32 { inl(self.port(reg)) }
    #[inline]
    fn write8(&self, reg: u16, v: u8) { outb(self.port(reg), v) }
    #[inline]
    fn write16(&self, reg: u16, v: u16) { outw(self.port(reg), v) }
    #[inline]
    fn write32(&self, reg: u16, v: u32) { outl(self.port(reg), v) }

    fn init_ac97(&mut self) -> bool {
        match self.chip {
            Chip::Ali5451 => {
                self.write32(ALI_GLOBAL_CONTROL, 0x1800_0001);
                self.write32(T4D_AINTEN_A, 0);
                self.write32(T4D_AINT_A, 0xffff_ffff);
                self.write32(T4D_MUSICVOL_WAVEVOL, 0);
                self.write8(0x22, 0x10);
                let sctrl = self.read32(ALI_SCTRL) & 0x3fff;
                self.write32(ALI_SCTRL, sctrl | PCMOUT | 0x8000);
            }
            Chip::Sis7018 => {
                self.write32(SI_AC97_GPIO, 0);
                self.write32(
                    SI_SERIAL_INTF_CTRL,
                    PCMOUT | SURROUT | CENTEROUT | LFEOUT | SECONDARY_ID,
                );
                delay_loops(150_000);
            }
            Chip::TridentDx => self.write32(DX_ACR2_AC97_COM_STAT, DX_AC97_PLAYBACK),
            Chip::TridentNx => self.write32(NX_ACR0_AC97_COM_STAT, NX_AC97_PCM_OUTPUT),
        }

        // A readable vendor/reset register is enough to verify the AC-link.
        let v = self.ac97_read(AC97_RESET);
        v != 0xffff
    }

    fn ac97_write(&self, reg: u8, value: u16) -> bool {
        match self.chip {
            Chip::Ali5451 => self.ali_ac97_write(reg, value),
            _ => {
                let (addr, mask, busy) = match self.chip {
                    Chip::Sis7018 => (SI_AC97_WRITE, SI_AC97_BUSY_WRITE | SI_AC97_AUDIO_BUSY, SI_AC97_BUSY_WRITE),
                    Chip::TridentDx => (DX_ACR0_AC97_W, DX_AC97_BUSY_WRITE, DX_AC97_BUSY_WRITE),
                    Chip::TridentNx => (NX_ACR1_AC97_W, NX_AC97_BUSY_WRITE, NX_AC97_BUSY_WRITE),
                    Chip::Ali5451 => unreachable!(),
                };
                if !self.wait_clear16(addr, busy as u16) { return false; }
                self.write32(addr, ((value as u32) << 16) | mask | reg as u32);
                true
            }
        }
    }

    fn ac97_read(&self, reg: u8) -> u16 {
        match self.chip {
            Chip::Ali5451 => self.ali_ac97_read(reg),
            _ => {
                let (addr, mask, busy) = match self.chip {
                    Chip::Sis7018 => (SI_AC97_READ, SI_AC97_BUSY_READ | SI_AC97_AUDIO_BUSY, SI_AC97_BUSY_READ),
                    Chip::TridentDx => (DX_ACR1_AC97_R, DX_AC97_BUSY_READ, DX_AC97_BUSY_READ),
                    Chip::TridentNx => (NX_ACR2_AC97_R_PRIMARY, NX_AC97_BUSY_READ, NX_AC97_BUSY_READ | NX_AC97_BUSY_DATA),
                    Chip::Ali5451 => unreachable!(),
                };
                self.write32(addr, mask | reg as u32);
                for _ in 0..0xffff {
                    let data = self.read32(addr);
                    if data & busy == 0 { return (data >> 16) as u16; }
                    core::hint::spin_loop();
                }
                0xffff
            }
        }
    }

    fn ali_ac97_write(&self, reg: u8, value: u16) -> bool {
        let mut mask = ALI_AC97_WRITE_ACTION | ALI_AC97_AUDIO_BUSY;
        if self.revision == 0x02 { mask |= ALI_AC97_WRITE_MIXER_REGISTER; }
        if !self.wait_clear16(ALI_AC97_WRITE, ALI_AC97_BUSY_WRITE as u16) { return false; }
        wait_ali_timer_tick(self);
        self.write32(ALI_AC97_WRITE, ((value as u32) << 16) | mask | reg as u32);
        true
    }

    fn ali_ac97_read(&self, reg: u8) -> u16 {
        let addr = if self.revision == 0x02 { ALI_AC97_WRITE } else { ALI_AC97_READ };
        if !self.wait_clear16(addr, ALI_AC97_BUSY_READ as u16) { return 0xffff; }
        wait_ali_timer_tick(self);
        let mut mask = ALI_AC97_READ_ACTION | ALI_AC97_AUDIO_BUSY;
        if self.revision == 0x02 { mask &= ALI_AC97_READ_MIXER_REGISTER; }
        self.write32(addr, mask | reg as u32);
        for _ in 0..0xffff {
            if self.read16(addr) & ALI_AC97_BUSY_READ as u16 == 0 {
                return (self.read32(addr) >> 16) as u16;
            }
            core::hint::spin_loop();
        }
        0xffff
    }

    fn wait_clear16(&self, reg: u16, mask: u16) -> bool {
        for _ in 0..0xffff {
            if self.read16(reg) & mask == 0 { return true; }
            core::hint::spin_loop();
        }
        false
    }

    fn select_voice(&self, channel: u8) {
        self.write8(T4D_LFO_GC_CIR, channel);
    }

    fn setup_voice(&self, phys: u32, bytes: usize, rate: u32, stereo: bool) -> Result<(), &'static str> {
        if bytes < 2 { return Err("empty PCM buffer"); }
        let samples = bytes / 2;
        let frames = if stereo { samples / 2 } else { samples };
        if frames == 0 || frames > 0xffff { return Err("PCM chunk too large"); }

        let delta = compute_rate(rate);
        let mut control = CHANNEL_LOOP | CHANNEL_16BITS | CHANNEL_SIGNED;
        if stereo { control |= CHANNEL_STEREO; }
        let eso = (frames - 1) as u32;
        let channel = self.playback_channel;
        self.select_voice(channel);

        let mut data = [0u32; 5];
        data[1] = phys;
        data[4] = control;
        match self.chip {
            Chip::Ali5451 | Chip::Sis7018 | Chip::TridentDx => {
                data[0] = 0;
                data[2] = (eso << 16) | (delta & 0xffff);
                data[3] = if self.chip == Chip::Sis7018 { (0x0800u32 << 16) } else { 0 };
            }
            Chip::TridentNx => {
                data[0] = delta << 24;
                data[2] = ((delta << 16) & 0xff00_0000) | (eso & 0x00ff_ffff);
                data[3] = 0;
            }
        }
        for (i, value) in data.iter().enumerate() {
            if self.chip == Chip::Ali5451 && i == 3 { continue; }
            self.write32(CHANNEL_START + (i as u16) * 4, *value);
        }
        Ok(())
    }

    fn start_voice(&self) {
        let channel = self.playback_channel;
        let mask = 1u32 << (channel & 31);
        let reg = if channel >= 32 { T4D_START_B } else { T4D_START_A };
        self.write32(reg, mask);
    }

    fn stop_voice(&self) {
        let channel = self.playback_channel;
        let mask = 1u32 << (channel & 31);
        let reg = if channel >= 32 { T4D_STOP_B } else { T4D_STOP_A };
        self.write32(reg, mask);
        // IRQs are deliberately unused in the polling implementation.
        let ainten = if channel >= 32 { T4D_AINTEN_B } else { T4D_AINTEN_A };
        self.write32(ainten, self.read32(ainten) & !mask);
        let aint = if channel >= 32 { T4D_AINT_B } else { T4D_AINT_A };
        self.write32(aint, mask);
    }

    fn current_sample(&self) -> u32 {
        self.select_voice(self.playback_channel);
        match self.chip {
            Chip::TridentNx => self.read32(CH_NX_DELTA_CSO) & 0x00ff_ffff,
            _ => self.read16(CH_DX_CSO_ALPHA_FMS + 2) as u32,
        }
    }

    fn play_chunk(&mut self, pcm: &[i16], rate: u32, stereo: bool) -> Result<(), &'static str> {
        let bytes = pcm.len().checked_mul(2).ok_or("PCM size overflow")?;
        if bytes == 0 || bytes > MAX_DMA_BYTES { return Err("invalid PCM chunk size"); }
        let layout = Layout::from_size_align(bytes, DMA_ALIGN).map_err(|_| "bad DMA layout")?;
        let dma = unsafe { alloc_zeroed(layout) };
        if dma == null_mut() { return Err("DMA allocation failed"); }
        unsafe { copy_nonoverlapping(pcm.as_ptr() as *const u8, dma, bytes); }

        let virt = dma as usize;
        if virt < KERNEL_OFFSET {
            unsafe { dealloc(dma, layout); }
            return Err("DMA buffer is not in higher-half memory");
        }
        let phys = (virt - KERNEL_OFFSET) as u32;
        if phys > 0x3fff_ffff {
            unsafe { dealloc(dma, layout); }
            return Err("DMA buffer exceeds controller 30-bit mask");
        }

        let frames = if stereo { pcm.len() / 2 } else { pcm.len() };
        if let Err(e) = self.setup_voice(phys, bytes, rate, stereo) {
            unsafe { dealloc(dma, layout); }
            return Err(e);
        }
        self.start_voice();

        // Poll CSO. CHANNEL_LOOP is kept because that is how the original
        // hardware driver programs playback voices; stop immediately after the
        // first pass reaches the end of the programmed sample range.
        let end = frames.saturating_sub(1) as u32;
        let mut seen_progress = false;
        let mut last = 0u32;
        let mut timeout = frames.saturating_mul(2_000).max(500_000).min(20_000_000);
        while timeout != 0 {
            let cso = self.current_sample();
            if cso != last { seen_progress = true; last = cso; }
            if seen_progress && cso >= end.saturating_sub(2) { break; }
            timeout -= 1;
            core::hint::spin_loop();
        }
        self.stop_voice();
        unsafe { dealloc(dma, layout); }
        if timeout == 0 { Err("playback DMA timeout") } else { Ok(()) }
    }
}

/// Play signed 16-bit little-endian PCM synchronously.
/// `channels` may be 1 or 2; rate is clamped to 4..48 kHz by the hardware formula.
pub fn play_pcm16(samples: &[i16], rate: u32, channels: u8) -> Result<(), &'static str> {
    if channels != 1 && channels != 2 { return Err("only mono/stereo PCM is supported"); }
    if samples.is_empty() { return Ok(()); }
    let stereo = channels == 2;
    let frame_samples = if stereo { 2 } else { 1 };
    let max_samples = (0xffffusize * frame_samples).min(MAX_DMA_BYTES / 2);

    let mut card_guard = AUDIO.lock();
    let card = card_guard.as_mut().ok_or("audio controller not initialized")?;
    let mut pos = 0;
    while pos < samples.len() {
        let mut end = core::cmp::min(pos + max_samples, samples.len());
        if stereo && ((end - pos) & 1) != 0 { end -= 1; }
        if end == pos { break; }
        card.play_chunk(&samples[pos..end], rate.clamp(4_000, 48_000), stereo)?;
        pos = end;
    }
    Ok(())
}

fn compute_rate(rate: u32) -> u32 {
    match rate.clamp(4_000, 48_000) {
        44_100 => 0x0eb3,
        8_000 => 0x02ab,
        48_000 => 0x1000,
        r => (((r << 12) + r) / 48_000) & 0xffff,
    }
}

fn wait_ali_timer_tick(card: &TridentAudio) {
    let first = card.read32(ALI_STIMER);
    for _ in 0..0xffff {
        if card.read32(ALI_STIMER) != first { break; }
        core::hint::spin_loop();
    }
}

fn delay_loops(mut loops: usize) {
    while loops != 0 {
        core::hint::spin_loop();
        loops -= 1;
    }
}

pub fn is_available() -> bool {
    AUDIO.lock().is_some()
}