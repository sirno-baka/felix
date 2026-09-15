//! Trident 4DWave / SiS 7018 / ALi M5451 AC'97 playback backend.
//!
//! Small no_std Rust port of the old OSS trident driver used as the hardware
//! reference. A single looping PCM voice uses a two-half DMA buffer. Shared IRQ
//! handling only acknowledges hardware and records which half is safe; refill
//! and mixing happen later from the PIT bottom half. If a safe shared PIC line
//! is unavailable, voice position is polled and controller IRQs remain off.

use alloc::boxed::Box;
use core::sync::atomic::{compiler_fence, Ordering};

use crate::drivers::audio::Mixer;
use crate::io::{inl, inw, io_wait, outb, outl, outw};
use crate::pci::bar::Bar;
use crate::pci::device::PciDevice;
use crate::KERNEL_OFFSET;

const VENDOR_TRIDENT: u16 = 0x1023;
const VENDOR_SIS: u16 = 0x1039;
const VENDOR_SIS_OLD: u16 = 0x0139;
const VENDOR_ALI: u16 = 0x10b9;

const DEV_DX: u16 = 0x2000;
const DEV_NX: u16 = 0x2001;
const DEV_SIS7018: u16 = 0x7018;
const DEV_ALI5451: u16 = 0x5451;

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

const SI_AC97_WRITE: u16 = 0x40;
const SI_AC97_READ: u16 = 0x44;
const SI_SERIAL_INTF_CTRL: u16 = 0x48;
const SI_AC97_GPIO: u16 = 0x4c;
const DX_AC97_WRITE: u16 = 0x40;
const DX_AC97_READ: u16 = 0x44;
const DX_AC97_COM_STAT: u16 = 0x48;
const NX_AC97_COM_STAT: u16 = 0x40;
const NX_AC97_WRITE: u16 = 0x44;
const NX_AC97_READ_PRIMARY: u16 = 0x48;
const ALI_AC97_WRITE: u16 = 0x40;
const ALI_AC97_READ: u16 = 0x44;
const ALI_SCTRL: u16 = 0x48;
const ALI_STIMER: u16 = 0xc8;
const ALI_GLOBAL_CONTROL: u16 = 0xd4;

const AC97_MASTER_VOL: u8 = 0x02;
const AC97_PCM_OUT_VOL: u8 = 0x18;
const AC97_POWERDOWN: u8 = 0x26;
const AC97_VENDOR_ID1: u8 = 0x7c;
const AC97_VENDOR_ID2: u8 = 0x7e;

const ENDLP_IE: u32 = 0x0000_1000;
const MIDLP_IE: u32 = 0x0000_2000;
const BANK_B_EN: u32 = 0x0001_0000;
const ADDRESS_IRQ: u32 = 0x0000_0020;
const MISC_ACK: u32 = 0x0000_8000 | 0x0000_0800 | 0x0000_0400;

const CHANNEL_LOOP: u32 = 0x0000_1000;
const CHANNEL_SIGNED: u32 = 0x0000_2000;
const CHANNEL_STEREO: u32 = 0x0000_4000;
const CHANNEL_16BITS: u32 = 0x0000_8000;
const PCM_LR: u16 = 0x0800;

const PCMOUT: u32 = 0x0001_0000;
const SURROUT: u32 = 0x0002_0000;
const CENTEROUT: u32 = 0x0004_0000;
const LFEOUT: u32 = 0x0008_0000;
const SECONDARY_ID: u32 = 0x0000_4000;

const SI_AC97_BUSY_WRITE: u32 = 0x8000;
const SI_AC97_BUSY_READ: u32 = 0x8000;
const SI_AC97_AUDIO_BUSY: u32 = 0x4000;
const DX_AC97_BUSY: u32 = 0x8000;
const NX_AC97_BUSY_WRITE: u32 = 0x0800;
const NX_AC97_BUSY_READ: u32 = 0x0800;
const NX_AC97_BUSY_DATA: u32 = 0x0400;
const ALI_AC97_BUSY: u32 = 0x8000;
const ALI_AC97_ACTION: u32 = 0x8000;
const ALI_AC97_AUDIO_BUSY: u32 = 0x4000;
const ALI_AC97_WRITE_MIXER: u32 = 0x0100;
const ALI_AC97_READ_MIXER_MASK: u32 = 0xfeff;

const ALI_REV_02: u8 = 0x02;
const DMA_MASK_30BIT: u32 = 0x3fff_ffff;

// 4096 stereo frames = 16 KiB. Each half is ~42.7 ms at 48 kHz.
const RING_FRAMES: usize = 4096;
const RING_SAMPLES: usize = RING_FRAMES * 2;
const HALF_FRAMES: usize = RING_FRAMES / 2;
const HALF_SAMPLES: usize = HALF_FRAMES * 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chip {
    Dx,
    Nx,
    Sis7018,
    Ali5451,
}

#[repr(C, align(4096))]
struct DmaBuffer {
    samples: [i16; RING_SAMPLES],
}

pub struct Trident {
    chip: Chip,
    iobase: u16,
    irq: u8,
    revision: u8,
    channel: u8,
    dma: Box<DmaBuffer>,
    running: bool,
    irq_enabled: bool,
    idle_halves: usize,
    pending_halves: u8,
    last_poll_half: u8,
}

unsafe impl Send for Trident {}

fn identify(d: &PciDevice) -> Option<Chip> {
    match (d.vendor_id, d.device_id) {
        (VENDOR_TRIDENT, DEV_DX) => Some(Chip::Dx),
        (VENDOR_TRIDENT, DEV_NX) => Some(Chip::Nx),
        (VENDOR_SIS, DEV_SIS7018) | (VENDOR_SIS_OLD, DEV_SIS7018) => Some(Chip::Sis7018),
        (VENDOR_ALI, DEV_ALI5451) => Some(Chip::Ali5451),
        _ => None,
    }
}

fn io_bar0(dev: &PciDevice) -> Result<u16, &'static str> {
    match dev.get_bar(0) {
        Some(Bar::Io { address, .. }) if *address != 0 && *address <= 0xffff => Ok(*address as u16),
        Some(Bar::Io { .. }) => Err("Trident I/O BAR outside 16-bit port space"),
        _ => Err("Trident missing I/O BAR0"),
    }
}

fn virt_to_phys<T>(p: *const T) -> Result<u32, &'static str> {
    let v = p as usize;
    let off = KERNEL_OFFSET as usize;
    let phys = if v >= off { v - off } else { v };
    if phys > DMA_MASK_30BIT as usize {
        return Err("Trident DMA buffer above 30-bit bus-master limit");
    }
    Ok(phys as u32)
}

impl Trident {
    pub fn name(&self) -> &'static str {
        match self.chip {
            Chip::Dx => "Trident 4DWave DX",
            Chip::Nx => "Trident 4DWave NX",
            Chip::Sis7018 => "SiS 7018 PCI Audio",
            Chip::Ali5451 => "ALi M5451 Audio",
        }
    }

    pub fn irq(&self) -> u8 { self.irq }
    pub fn set_irq_enabled(&mut self, enabled: bool) { self.irq_enabled = enabled; }

    #[inline]
    fn p(&self, reg: u16) -> u16 { self.iobase.wrapping_add(reg) }

    fn bank_regs(&self) -> (u16, u16, u16, u16) {
        if self.chip == Chip::Ali5451 {
            (T4D_START_A, T4D_STOP_A, T4D_AINT_A, T4D_AINTEN_A)
        } else {
            (T4D_START_B, T4D_STOP_B, T4D_AINT_B, T4D_AINTEN_B)
        }
    }

    fn ali_tick_delay(&self) -> Result<(), &'static str> {
        let first = inl(self.p(ALI_STIMER));
        for _ in 0..0xffff {
            if inl(self.p(ALI_STIMER)) != first { return Ok(()); }
            core::hint::spin_loop();
        }
        Err("ALi system timer not advancing")
    }

    fn ac97_write(&self, reg: u8, val: u16) -> Result<(), &'static str> {
        let value = (val as u32) << 16;
        let (addr, mask, busy) = match self.chip {
            Chip::Sis7018 => (SI_AC97_WRITE, SI_AC97_BUSY_WRITE | SI_AC97_AUDIO_BUSY, SI_AC97_BUSY_WRITE),
            Chip::Dx => (DX_AC97_WRITE, DX_AC97_BUSY, DX_AC97_BUSY),
            Chip::Nx => (NX_AC97_WRITE, NX_AC97_BUSY_WRITE, NX_AC97_BUSY_WRITE),
            Chip::Ali5451 => {
                let mut m = ALI_AC97_ACTION | ALI_AC97_AUDIO_BUSY;
                if self.revision == ALI_REV_02 { m |= ALI_AC97_WRITE_MIXER; }
                (ALI_AC97_WRITE, m, ALI_AC97_BUSY)
            }
        };

        for _ in 0..0xffff {
            if (inw(self.p(addr)) as u32 & busy) == 0 {
                if self.chip == Chip::Ali5451 { self.ali_tick_delay()?; }
                outl(self.p(addr), value | mask | reg as u32);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("Trident AC97 write timeout")
    }

    fn ac97_read(&self, reg: u8) -> Result<u16, &'static str> {
        let (addr, mask, busy) = match self.chip {
            Chip::Sis7018 => (SI_AC97_READ, SI_AC97_BUSY_READ | SI_AC97_AUDIO_BUSY, SI_AC97_BUSY_READ),
            Chip::Dx => (DX_AC97_READ, DX_AC97_BUSY, DX_AC97_BUSY),
            Chip::Nx => (NX_AC97_READ_PRIMARY, NX_AC97_BUSY_READ, NX_AC97_BUSY_READ | NX_AC97_BUSY_DATA),
            Chip::Ali5451 => {
                let addr = if self.revision == ALI_REV_02 { ALI_AC97_WRITE } else { ALI_AC97_READ };
                let mut m = ALI_AC97_ACTION | ALI_AC97_AUDIO_BUSY;
                if self.revision == ALI_REV_02 { m &= ALI_AC97_READ_MIXER_MASK; }
                (addr, m, ALI_AC97_BUSY)
            }
        };

        for _ in 0..0xffff {
            if (inw(self.p(addr)) as u32 & busy) == 0 {
                if self.chip == Chip::Ali5451 { self.ali_tick_delay()?; }
                outl(self.p(addr), mask | reg as u32);
                for _ in 0..0xffff {
                    let data = inl(self.p(addr));
                    if data & busy == 0 { return Ok((data >> 16) as u16); }
                    core::hint::spin_loop();
                }
                break;
            }
        }
        Err("Trident AC97 read timeout")
    }

    fn init_ac97(&self) -> Result<(), &'static str> {
        match self.chip {
            Chip::Ali5451 => {
                outl(self.p(ALI_GLOBAL_CONTROL), 0x1800_0001);
                outl(self.p(T4D_AINTEN_A), 0);
                outl(self.p(T4D_AINT_A), 0xffff_ffff);
                outl(self.p(T4D_MUSICVOL_WAVEVOL), 0);
                outb(self.p(0x22), 0x10);
                let s = inl(self.p(ALI_SCTRL)) & 0x3fff;
                outl(self.p(ALI_SCTRL), s | PCMOUT | 0x8000);
            }
            Chip::Sis7018 => {
                outl(self.p(SI_AC97_GPIO), 0);
                outl(self.p(SI_SERIAL_INTF_CTRL), PCMOUT | SURROUT | CENTEROUT | LFEOUT | SECONDARY_ID);
                for _ in 0..20_000 { io_wait(); }
            }
            Chip::Dx => outl(self.p(DX_AC97_COM_STAT), 0x0002),
            Chip::Nx => outl(self.p(NX_AC97_COM_STAT), 0x0002),
        }

        self.ac97_write(AC97_POWERDOWN, 0)?;
        self.ac97_write(AC97_MASTER_VOL, 0)?;
        self.ac97_write(AC97_PCM_OUT_VOL, 0)?;
        let v1 = self.ac97_read(AC97_VENDOR_ID1).unwrap_or(0xffff);
        let v2 = self.ac97_read(AC97_VENDOR_ID2).unwrap_or(0xffff);
        crate::println!("[audio/trident] AC97 codec {:04x}:{:04x}", v1, v2);
        Ok(())
    }

    fn enable_loop_irqs(&self) {
        let mut gc = inl(self.p(T4D_LFO_GC_CIR));
        gc |= ENDLP_IE | MIDLP_IE;
        if self.chip == Chip::Sis7018 { gc |= BANK_B_EN; }
        outl(self.p(T4D_LFO_GC_CIR), gc);
    }

    fn voice_mask(&self) -> u32 { 1u32 << (self.channel & 31) }

    fn enable_voice_irq(&self) {
        let (_, _, _, ainten) = self.bank_regs();
        outl(self.p(ainten), inl(self.p(ainten)) | self.voice_mask());
    }

    fn disable_voice_irq(&self) {
        let (_, _, aint, ainten) = self.bank_regs();
        outl(self.p(ainten), inl(self.p(ainten)) & !self.voice_mask());
        outl(self.p(aint), self.voice_mask());
    }

    fn start_voice(&self) {
        let (start, _, _, _) = self.bank_regs();
        outl(self.p(start), self.voice_mask());
    }

    fn stop_voice(&self) {
        let (_, stop, _, _) = self.bank_regs();
        outl(self.p(stop), self.voice_mask());
    }

    fn program_voice(&self) -> Result<(), &'static str> {
        let lba = virt_to_phys(self.dma.samples.as_ptr())?;
        let eso = (RING_FRAMES - 1) as u32;
        let delta = 0x1000u32; // 48 kHz
        let control = CHANNEL_LOOP | CHANNEL_SIGNED | CHANNEL_STEREO | CHANNEL_16BITS;
        let attribute = if self.chip == Chip::Sis7018 { PCM_LR } else { 0 };

        let mut data = [0u32; 5];
        data[1] = lba;
        data[4] = control;
        match self.chip {
            Chip::Ali5451 => {
                data[0] = 0;
                data[2] = (eso << 16) | delta;
                data[3] = 0;
            }
            Chip::Sis7018 => {
                data[0] = 0;
                data[2] = (eso << 16) | delta;
                data[3] = (attribute as u32) << 16;
            }
            Chip::Dx => {
                data[0] = 0;
                data[2] = (eso << 16) | delta;
                data[3] = 0;
            }
            Chip::Nx => {
                data[0] = delta << 24;
                data[2] = ((delta << 16) & 0xff00_0000) | (eso & 0x00ff_ffff);
                data[3] = 0;
            }
        }

        outb(self.p(T4D_LFO_GC_CIR), self.channel);
        for (i, value) in data.iter().enumerate() {
            if i == 3 && self.chip == Chip::Ali5451 { continue; }
            outl(self.p(CHANNEL_START + (i as u16) * 4), *value);
        }
        Ok(())
    }

    fn current_frame(&self) -> u32 {
        outb(self.p(T4D_LFO_GC_CIR), self.channel);
        match self.chip {
            Chip::Nx => inl(self.p(CHANNEL_START)) & 0x00ff_ffff,
            _ => inw(self.p(CHANNEL_START + 2)) as u32,
        }
    }

    fn fill_half(&mut self, half: usize, mixer: &mut Mixer) -> bool {
        let start = (half & 1) * HALF_SAMPLES;
        mixer.mix_into(&mut self.dma.samples[start..start + HALF_SAMPLES])
    }

    pub fn kick(&mut self, mixer: &mut Mixer) -> Result<(), &'static str> {
        if self.running || !mixer.has_data() { return Ok(()); }

        self.dma.samples.fill(0);
        let _ = self.fill_half(0, mixer);
        let _ = self.fill_half(1, mixer);
        compiler_fence(Ordering::SeqCst);

        self.program_voice()?;
        if self.irq_enabled {
            self.enable_loop_irqs();
            self.enable_voice_irq();
        } else {
            self.disable_voice_irq();
        }
        outl(self.p(T4D_MUSICVOL_WAVEVOL), 0);
        self.idle_halves = 0;
        self.pending_halves = 0;
        self.last_poll_half = 0;
        self.start_voice();
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.stop_voice();
        self.disable_voice_irq();
        self.running = false;
        self.idle_halves = 0;
        self.pending_halves = 0;
    }

    /// Fast shared-IRQ path. Return false if the card did not assert its own
    /// address interrupt. It only ACKs hardware and records a refill request.
    pub fn ack_irq(&mut self) -> bool {
        if !self.running || !self.irq_enabled { return false; }
        let event = inl(self.p(T4D_MISCINT));
        if event & ADDRESS_IRQ == 0 { return false; }

        let (_, _, aint, _) = self.bank_regs();
        let active = inl(self.p(aint));
        let ours = active & self.voice_mask();
        if active != 0 { outl(self.p(aint), active); }
        outl(self.p(T4D_MISCINT), MISC_ACK);

        if ours != 0 {
            let cso = self.current_frame() as usize % RING_FRAMES;
            let safe_half = if cso >= HALF_FRAMES { 0 } else { 1 };
            self.pending_halves |= 1u8 << safe_half;
        }
        true
    }

    /// PIT bottom half. With a registered IRQ it consumes pending address
    /// interrupts. In pure-polling mode it watches CSO cross the half boundary.
    pub fn poll(&mut self, mixer: &mut Mixer) {
        if !self.running { return; }

        if self.irq_enabled {
            // Also inspect status here: if IRQ9 was storm-masked later, playback
            // continues because AINT/MISC are still drained by PIT.
            let event = inl(self.p(T4D_MISCINT));
            if event & ADDRESS_IRQ != 0 {
                let _ = self.ack_irq();
            }
        } else {
            let cso = self.current_frame() as usize % RING_FRAMES;
            let current_half = if cso >= HALF_FRAMES { 1u8 } else { 0u8 };
            if current_half != self.last_poll_half {
                // The half just left by hardware is now safe to refill.
                self.pending_halves |= 1u8 << self.last_poll_half;
                self.last_poll_half = current_half;
            }
        }

        let pending = self.pending_halves;
        self.pending_halves = 0;
        if pending == 0 { return; }

        for half in 0..2usize {
            if pending & (1u8 << half) == 0 { continue; }
            let data = self.fill_half(half, mixer);
            compiler_fence(Ordering::SeqCst);
            if data { self.idle_halves = 0; } else { self.idle_halves = self.idle_halves.saturating_add(1); }
        }

        if self.idle_halves >= 3 && !mixer.has_data() {
            self.stop();
        }
    }
}

pub fn probe_first() -> Result<Option<Trident>, &'static str> {
    let Some((dev, chip)) = crate::pci::enumerate()
        .into_iter()
        .find_map(|d| identify(&d).map(|c| (d, c)))
    else { return Ok(None); };

    let iobase = io_bar0(&dev)?;
    let command = dev.read_u16(0x04);
    dev.write_u16(0x04, command | 0x0005); // I/O + bus master

    let channel = if chip == Chip::Ali5451 { 0 } else { 63 };
    let dma = Box::new(DmaBuffer { samples: [0; RING_SAMPLES] });
    let phys = virt_to_phys(dma.samples.as_ptr())?;
    let end = phys.checked_add((RING_SAMPLES * 2 - 1) as u32).ok_or("Trident DMA overflow")?;
    if end > DMA_MASK_30BIT { return Err("Trident DMA crosses 30-bit limit"); }

    let card = Trident {
        chip,
        iobase,
        irq: dev.interrupt_line,
        revision: dev.revision_id,
        channel,
        dma,
        running: false,
        irq_enabled: false,
        idle_halves: 0,
        pending_halves: 0,
        last_poll_half: 0,
    };
    card.init_ac97()?;

    crate::println!(
        "[audio/trident] {} io={:#06x} irq={} rev={:#04x} channel={}",
        card.name(), card.iobase, card.irq, card.revision, card.channel
    );
    Ok(Some(card))
}
