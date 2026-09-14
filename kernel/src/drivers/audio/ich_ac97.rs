//! Intel ICH AC'97 audio controller (i810/ICH family).
//!
//! Initial version intentionally uses polling instead of PCI IRQs because Felix
//! does not yet have a shared legacy INTx dispatcher. Playback is synchronous.
//!
//! Supported Intel PCI IDs:
//!   8086:2415  82801AA (ICH)
//!   8086:2425  82801AB
//!   8086:2445  82801BA (ICH2)
//!   8086:2485  ICH3
//!   8086:24c5  ICH4
//!   8086:24d5  ICH5
//!   8086:25a6  6300ESB
//!   8086:266e  ICH6
//!   8086:27de  ICH7
//!   8086:2698  ESB2
//!   8086:7195  440MX

use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;
use core::cmp::min;
use core::ptr;
use core::sync::atomic::{compiler_fence, Ordering};

use crate::io::{inb, inl, inw, io_wait, outb, outl, outw};
use crate::pci::bar::Bar;
use crate::pci::device::PciDevice;
use crate::sync::mutex::Mutex;
use crate::KERNEL_OFFSET;

const INTEL_VENDOR: u16 = 0x8086;

const INTEL_ICH_AC97_IDS: &[u16] = &[
    0x2415, // 82801AA
    0x2425, // 82801AB
    0x2445, // 82801BA / ICH2
    0x2485, // ICH3
    0x24c5, // ICH4
    0x24d5, // ICH5
    0x25a6, // 6300ESB
    0x266e, // ICH6
    0x27de, // ICH7
    0x2698, // ESB2
    0x7195, // 440MX
];

// Native Audio Mixer (NAM) registers.
const AC97_RESET: u16 = 0x00;
const AC97_MASTER_VOL: u16 = 0x02;
const AC97_AUX_OUT_VOL: u16 = 0x04;
const AC97_PCM_OUT_VOL: u16 = 0x18;
const AC97_POWERDOWN: u16 = 0x26;
const AC97_EXT_AUDIO_ID: u16 = 0x28;
const AC97_EXT_AUDIO_CTRL: u16 = 0x2a;
const AC97_FRONT_DAC_RATE: u16 = 0x2c;
const AC97_VENDOR_ID1: u16 = 0x7c;
const AC97_VENDOR_ID2: u16 = 0x7e;

const AC97_EXT_VRA: u16 = 1 << 0;

// Native Audio Bus Master (NABM) registers.
const PO_BDBAR: u16 = 0x10;
const PO_CIV: u16 = 0x14;
const PO_LVI: u16 = 0x15;
const PO_SR: u16 = 0x16;
const PO_PICB: u16 = 0x18;
const PO_PIV: u16 = 0x1a;
const PO_CR: u16 = 0x1b;

const GLOB_CNT: u16 = 0x2c;
const GLOB_STA: u16 = 0x30;

// Global control/status.
const GLOB_CNT_GIE: u32 = 1 << 0;
const GLOB_CNT_COLD: u32 = 1 << 1;
const GLOB_STA_PRIMARY_CODEC_READY: u32 = 1 << 8;

// PCM-out status.
const SR_DCH: u16 = 1 << 0;   // DMA controller halted
const SR_CELV: u16 = 1 << 1;  // current equals last valid
const SR_LVBCI: u16 = 1 << 2;
const SR_BCIS: u16 = 1 << 3;
const SR_FIFOE: u16 = 1 << 4;
const SR_W1C: u16 = SR_LVBCI | SR_BCIS | SR_FIFOE;

// PCM-out control.
const CR_RPBM: u8 = 1 << 0;
const CR_RR: u8 = 1 << 1;

// Buffer descriptor control.
const BD_BUP: u32 = 1 << 30;
// We intentionally do not set IOC because this first Felix implementation polls.
const MAX_BDL_SAMPLES: usize = 0xff00;

#[repr(C, align(8))]
struct BdlEntry {
    addr: u32,
    control_len: u32,
}

pub struct IchAc97 {
    nam: u16,
    nabm: u16,
    irq: u8,
    device_id: u16,
    revision: u8,
}

static ICH_AC97: Mutex<Option<IchAc97>> = Mutex::new(None);

fn supported_id(device_id: u16) -> bool {
    INTEL_ICH_AC97_IDS.iter().any(|&id| id == device_id)
}

fn device_name(device_id: u16) -> &'static str {
    match device_id {
        0x2415 => "Intel 82801AA ICH AC'97",
        0x2425 => "Intel 82801AB AC'97",
        0x2445 => "Intel 82801BA ICH2 AC'97",
        0x2485 => "Intel ICH3 AC'97",
        0x24c5 => "Intel ICH4 AC'97",
        0x24d5 => "Intel ICH5 AC'97",
        0x25a6 => "Intel 6300ESB AC'97",
        0x266e => "Intel ICH6 AC'97",
        0x27de => "Intel ICH7 AC'97",
        0x2698 => "Intel ESB2 AC'97",
        0x7195 => "Intel 440MX AC'97",
        _ => "Intel ICH AC'97",
    }
}

fn io_bar(dev: &PciDevice, index: usize) -> Result<u16, &'static str> {
    match dev.get_bar(index) {
        Some(Bar::Io { address, .. }) if *address != 0 && *address <= u16::MAX as u32 => {
            Ok(*address as u16)
        }
        Some(Bar::Io { .. }) => Err("I/O BAR is outside x86 16-bit port space"),
        Some(Bar::Memory { .. }) => Err("expected I/O BAR"),
        _ => Err("missing PCI BAR"),
    }
}

#[inline]
fn nam_port(card: &IchAc97, reg: u16) -> u16 {
    card.nam.wrapping_add(reg)
}

#[inline]
fn nabm_port(card: &IchAc97, reg: u16) -> u16 {
    card.nabm.wrapping_add(reg)
}

#[inline]
fn codec_read(card: &IchAc97, reg: u16) -> u16 {
    inw(nam_port(card, reg))
}

#[inline]
fn codec_write(card: &IchAc97, reg: u16, value: u16) {
    outw(nam_port(card, reg), value);
}

fn virt_to_phys(ptr: *const u8) -> Result<u32, &'static str> {
    let v = ptr as usize;
    let off = KERNEL_OFFSET as usize;

    let p = if v >= off { v - off } else { v };
    if p > u32::MAX as usize {
        return Err("DMA address does not fit in 32 bits");
    }
    Ok(p as u32)
}

impl IchAc97 {
    fn reset_pcm_out(&self) -> Result<(), &'static str> {
        outb(nabm_port(self, PO_CR), 0);
        outb(nabm_port(self, PO_CR), CR_RR);

        let mut timeout = 100_000usize;
        while timeout != 0 {
            if (inb(nabm_port(self, PO_CR)) & CR_RR) == 0 {
                outw(nabm_port(self, PO_SR), SR_W1C);
                return Ok(());
            }
            timeout -= 1;
            core::hint::spin_loop();
        }

        Err("PCM-out bus-master reset timed out")
    }

    fn initialize(&mut self) -> Result<(), &'static str> {
        // Keep the AC-link out of cold reset, but do not enable global IRQs.
        let mut gc = inl(nabm_port(self, GLOB_CNT));
        gc |= GLOB_CNT_COLD;
        gc &= !GLOB_CNT_GIE;
        outl(nabm_port(self, GLOB_CNT), gc);

        // Primary codec should assert ready after AC-link reset.
        let mut ready = false;
        for _ in 0..200_000 {
            if (inl(nabm_port(self, GLOB_STA)) & GLOB_STA_PRIMARY_CODEC_READY) != 0 {
                ready = true;
                break;
            }
            io_wait();
        }
        if !ready {
            return Err("primary AC'97 codec did not become ready");
        }

        // Reset codec registers and ensure the analog/DAC blocks are powered.
        codec_write(self, AC97_RESET, 0);
        for _ in 0..2_000 {
            io_wait();
        }
        codec_write(self, AC97_POWERDOWN, 0x0000);

        // Maximum volume / unmuted.
        codec_write(self, AC97_MASTER_VOL, 0x0000);
        codec_write(self, AC97_PCM_OUT_VOL, 0x0000);
        codec_write(self, AC97_AUX_OUT_VOL, 0x0000);

        self.reset_pcm_out()?;

        let vid1 = codec_read(self, AC97_VENDOR_ID1);
        let vid2 = codec_read(self, AC97_VENDOR_ID2);
        crate::println!(
            "[audio/ich] codec id={:04x}:{:04x}, master={:04x}, pcm={:04x}",
            vid1,
            vid2,
            codec_read(self, AC97_MASTER_VOL),
            codec_read(self, AC97_PCM_OUT_VOL),
        );

        Ok(())
    }

    fn set_rate(&self, rate: u32) -> Result<(), &'static str> {
        if !(8_000..=48_000).contains(&rate) {
            return Err("ICH AC'97 rate must be 8000..48000 Hz");
        }

        if rate == 48_000 {
            // 48 kHz is mandatory even without Variable Rate Audio support.
            if (codec_read(self, AC97_EXT_AUDIO_ID) & AC97_EXT_VRA) != 0 {
                let ext = codec_read(self, AC97_EXT_AUDIO_CTRL);
                codec_write(self, AC97_EXT_AUDIO_CTRL, ext | AC97_EXT_VRA);
                codec_write(self, AC97_FRONT_DAC_RATE, 48_000);
            }
            return Ok(());
        }

        if (codec_read(self, AC97_EXT_AUDIO_ID) & AC97_EXT_VRA) == 0 {
            return Err("AC'97 codec has no variable-rate audio; use 48000 Hz");
        }

        let ext = codec_read(self, AC97_EXT_AUDIO_CTRL);
        codec_write(self, AC97_EXT_AUDIO_CTRL, ext | AC97_EXT_VRA);
        codec_write(self, AC97_FRONT_DAC_RATE, rate as u16);

        // Some codecs quantize/reject unsupported rates. Require exact readback
        // so callers never silently get wrong playback speed.
        if codec_read(self, AC97_FRONT_DAC_RATE) != rate as u16 {
            return Err("AC'97 codec rejected requested sample rate");
        }

        Ok(())
    }

    fn play_one(&mut self, samples: &[i16], channels: u8) -> Result<(), &'static str> {
        let out_samples = match channels {
            1 => samples.len().checked_mul(2).ok_or("PCM size overflow")?,
            2 => samples.len(),
            _ => return Err("ICH AC'97 supports mono or stereo input"),
        };

        if out_samples == 0 {
            return Ok(());
        }
        if out_samples > MAX_BDL_SAMPLES {
            return Err("internal AC'97 playback chunk too large");
        }

        let dma_layout =
            Layout::array::<i16>(out_samples).map_err(|_| "invalid PCM DMA layout")?;
        let dma = unsafe { alloc_zeroed(dma_layout) } as *mut i16;
        if dma.is_null() {
            return Err("cannot allocate AC'97 PCM DMA buffer");
        }

        unsafe {
            if channels == 2 {
                ptr::copy_nonoverlapping(samples.as_ptr(), dma, samples.len());
            } else {
                for (i, &s) in samples.iter().enumerate() {
                    *dma.add(i * 2) = s;
                    *dma.add(i * 2 + 1) = s;
                }
            }
        }

        let bdl_layout = Layout::new::<BdlEntry>();
        let bdl = unsafe { alloc_zeroed(bdl_layout) } as *mut BdlEntry;
        if bdl.is_null() {
            unsafe { dealloc(dma as *mut u8, dma_layout) };
            return Err("cannot allocate AC'97 BDL");
        }

        let result = (|| -> Result<(), &'static str> {
            let dma_phys = virt_to_phys(dma as *const u8)?;
            let bdl_phys = virt_to_phys(bdl as *const u8)?;

            let dma_bytes = dma_layout.size() as u32;
            if dma_phys.checked_add(dma_bytes.saturating_sub(1)).is_none() {
                return Err("PCM DMA buffer crosses 4 GiB");
            }

            unsafe {
                (*bdl).addr = dma_phys;
                (*bdl).control_len = (out_samples as u32) | BD_BUP;
            }

            compiler_fence(Ordering::SeqCst);

            self.reset_pcm_out()?;
            outl(nabm_port(self, PO_BDBAR), bdl_phys);
            outb(nabm_port(self, PO_LVI), 0);
            outw(nabm_port(self, PO_SR), SR_W1C);

            // Start PCM-out DMA without IRQ enable bits.
            outb(nabm_port(self, PO_CR), CR_RPBM);

            let initial_picb = inw(nabm_port(self, PO_PICB));
            let mut ran = false;
            let mut completed = false;

            // Synchronous polling version. This cap is deliberately generous
            // for slow real PCI hardware but finite so a broken controller
            // cannot hang the kernel forever.
            let mut timeout = 25_000_000usize;
            while timeout != 0 {
                let sr = inw(nabm_port(self, PO_SR));
                let picb = inw(nabm_port(self, PO_PICB));

                if (sr & SR_FIFOE) != 0 {
                    outw(nabm_port(self, PO_SR), SR_FIFOE);
                    return Err("AC'97 PCM-out FIFO error");
                }

                if (sr & SR_DCH) == 0 || picb != initial_picb {
                    ran = true;
                }

                if ran && (sr & SR_DCH) != 0 {
                    completed = true;
                    break;
                }

                timeout -= 1;
                core::hint::spin_loop();
            }

            outb(nabm_port(self, PO_CR), 0);
            outw(nabm_port(self, PO_SR), SR_W1C);

            if !completed {
                crate::println!(
                    "[audio/ich] timeout: CIV={} LVI={} PIV={} PICB={} SR={:#06x}",
                    inb(nabm_port(self, PO_CIV)),
                    inb(nabm_port(self, PO_LVI)),
                    inb(nabm_port(self, PO_PIV)),
                    inw(nabm_port(self, PO_PICB)),
                    inw(nabm_port(self, PO_SR)),
                );
                return Err("AC'97 PCM-out playback timed out");
            }

            Ok(())
        })();

        unsafe {
            dealloc(bdl as *mut u8, bdl_layout);
            dealloc(dma as *mut u8, dma_layout);
        }

        result
    }

    fn play_pcm16(
        &mut self,
        samples: &[i16],
        rate: u32,
        channels: u8,
    ) -> Result<(), &'static str> {
        if channels != 1 && channels != 2 {
            return Err("channels must be 1 or 2");
        }
        if channels == 2 && (samples.len() & 1) != 0 {
            return Err("stereo PCM must contain whole L/R frames");
        }

        self.set_rate(rate)?;

        let max_input_samples = if channels == 1 {
            MAX_BDL_SAMPLES / 2
        } else {
            MAX_BDL_SAMPLES
        };

        let mut pos = 0usize;
        while pos < samples.len() {
            let mut count = min(samples.len() - pos, max_input_samples);
            if channels == 2 && (count & 1) != 0 {
                count -= 1;
            }
            if count == 0 {
                return Err("invalid final stereo PCM frame");
            }

            self.play_one(&samples[pos..pos + count], channels)?;
            pos += count;
        }

        Ok(())
    }
}

/// Probe and initialize the first supported Intel ICH AC'97 controller.
pub fn init() {
    let dev = crate::pci::enumerate()
        .into_iter()
        .find(|d| d.vendor_id == INTEL_VENDOR && supported_id(d.device_id));

    let Some(dev) = dev else {
        return;
    };

    let nam = match io_bar(&dev, 0) {
        Ok(v) => v,
        Err(e) => {
            crate::println!("[audio/ich] {}: BAR0 error: {}", device_name(dev.device_id), e);
            return;
        }
    };
    let nabm = match io_bar(&dev, 1) {
        Ok(v) => v,
        Err(e) => {
            crate::println!("[audio/ich] {}: BAR1 error: {}", device_name(dev.device_id), e);
            return;
        }
    };

    dev.enable_bus_mastering();

    let mut card = IchAc97 {
        nam,
        nabm,
        irq: dev.interrupt_line,
        device_id: dev.device_id,
        revision: dev.revision_id,
    };

    match card.initialize() {
        Ok(()) => {
            crate::println!(
                "[audio/ich] {} at NAM={:#06x} NABM={:#06x} IRQ {} rev {:#04x} (polling)",
                device_name(card.device_id),
                card.nam,
                card.nabm,
                card.irq,
                card.revision,
            );
            *ICH_AC97.lock() = Some(card);
        }
        Err(e) => {
            crate::println!("[audio/ich] init failed: {}", e);
        }
    }
}

pub fn is_available() -> bool {
    ICH_AC97.lock().is_some()
}

/// Play signed little-endian PCM16 samples synchronously.
///
/// `channels` may be 1 or 2. Mono is duplicated to L/R in the DMA buffer.
pub fn play_pcm16(samples: &[i16], rate: u32, channels: u8) -> Result<(), &'static str> {
    let mut guard = ICH_AC97.lock();
    let card = guard.as_mut().ok_or("Intel ICH AC'97 is not initialized")?;
    card.play_pcm16(samples, rate, channels)
}
