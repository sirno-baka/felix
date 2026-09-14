//! Intel ICH/i810-style AC'97 playback backend.
//!
//! Uses a 32-entry Buffer Descriptor List and keeps eight fragments queued.
//! IRQ completion is preferred, but the exact same status service routine can
//! be called from the PIT fallback if legacy shared INTx is unreliable.

use alloc::boxed::Box;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{compiler_fence, Ordering};

use crate::drivers::audio::Mixer;
use crate::io::{inb, inl, inw, io_wait, outb, outl, outw};
use crate::pci::bar::Bar;
use crate::pci::device::PciDevice;
use crate::KERNEL_OFFSET;

const INTEL: u16 = 0x8086;
const IDS: &[u16] = &[
    0x2415, 0x2425, 0x2445, 0x2485, 0x24c5, 0x24d5, 0x25a6, 0x266e, 0x27de,
    0x2698, 0x7195,
];

// Native Audio Mixer registers.
const AC97_RESET: u16 = 0x00;
const AC97_MASTER_VOL: u16 = 0x02;
const AC97_PCM_OUT_VOL: u16 = 0x18;
const AC97_POWERDOWN: u16 = 0x26;
const AC97_EXT_AUDIO_ID: u16 = 0x28;
const AC97_EXT_AUDIO_CTRL: u16 = 0x2a;
const AC97_FRONT_DAC_RATE: u16 = 0x2c;
const AC97_VENDOR_ID1: u16 = 0x7c;
const AC97_VENDOR_ID2: u16 = 0x7e;
const AC97_EXT_VRA: u16 = 1 << 0;

// Native Audio Bus Master registers, PCM out engine.
const PO_BDBAR: u16 = 0x10;
const PO_CIV: u16 = 0x14;
const PO_LVI: u16 = 0x15;
const PO_SR: u16 = 0x16;
const PO_PICB: u16 = 0x18;
const PO_CR: u16 = 0x1b;
const GLOB_CNT: u16 = 0x2c;
const GLOB_STA: u16 = 0x30;

const GLOB_CNT_GIE: u32 = 1 << 0;
const GLOB_CNT_COLD: u32 = 1 << 1;
const GLOB_STA_PCR: u32 = 1 << 8;

const SR_DCH: u16 = 1 << 0;
const SR_LVBCI: u16 = 1 << 2;
const SR_BCIS: u16 = 1 << 3;
const SR_FIFOE: u16 = 1 << 4;
const SR_W1C: u16 = SR_LVBCI | SR_BCIS | SR_FIFOE;

const CR_RPBM: u8 = 1 << 0;
const CR_RR: u8 = 1 << 1;
const CR_FEIE: u8 = 1 << 3;
const CR_IOCE: u8 = 1 << 4;

const BD_BUP: u32 = 1 << 30;
const BD_IOC: u32 = 1 << 31;

const BDL_COUNT: usize = 32;
const ACTIVE_FRAGS: usize = 8;
const FRAMES_PER_FRAG: usize = 1024;
const SAMPLES_PER_FRAG: usize = FRAMES_PER_FRAG * 2;
const DMA_SAMPLES: usize = BDL_COUNT * SAMPLES_PER_FRAG;

#[repr(C)]
#[derive(Clone, Copy)]
struct BdlEntry {
    addr: u32,
    control_len: u32,
}

#[repr(C, align(16))]
struct Bdl {
    entries: [BdlEntry; BDL_COUNT],
}

#[repr(C, align(4096))]
struct DmaBuffer {
    samples: [i16; DMA_SAMPLES],
}

pub struct IchAc97 {
    nam: u16,
    nabm: u16,
    irq: u8,
    device_id: u16,
    revision: u8,
    bdl: Box<Bdl>,
    dma: Box<DmaBuffer>,
    running: bool,
    last_civ: u8,
    lvi: u8,
    idle_appends: usize,
}

unsafe impl Send for IchAc97 {}

fn supported(id: u16) -> bool {
    IDS.iter().any(|&x| x == id)
}

fn chip_name(id: u16) -> &'static str {
    match id {
        0x2415 => "Intel ICH AC'97",
        0x2425 => "Intel ICH0 AC'97",
        0x2445 => "Intel ICH2 AC'97",
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
        Some(Bar::Io { address, .. }) if *address != 0 && *address <= 0xffff => Ok(*address as u16),
        Some(Bar::Io { .. }) => Err("ICH AC97 I/O BAR outside 16-bit port space"),
        Some(Bar::Memory { .. }) => Err("ICH AC97 expected I/O BAR"),
        _ => Err("ICH AC97 missing BAR"),
    }
}

fn virt_to_phys<T>(p: *const T) -> Result<u32, &'static str> {
    let v = p as usize;
    let off = KERNEL_OFFSET as usize;
    let phys = if v >= off { v - off } else { v };
    if phys > u32::MAX as usize {
        return Err("ICH AC97 DMA address above 4GiB");
    }
    Ok(phys as u32)
}

impl IchAc97 {
    pub fn name(&self) -> &'static str { chip_name(self.device_id) }
    pub fn irq(&self) -> u8 { self.irq }

    #[inline]
    fn nport(&self, reg: u16) -> u16 { self.nam.wrapping_add(reg) }
    #[inline]
    fn bport(&self, reg: u16) -> u16 { self.nabm.wrapping_add(reg) }

    #[inline]
    fn codec_read(&self, reg: u16) -> u16 { inw(self.nport(reg)) }
    #[inline]
    fn codec_write(&self, reg: u16, value: u16) { outw(self.nport(reg), value); }

    fn reset_pcm_out(&self) -> Result<(), &'static str> {
        outb(self.bport(PO_CR), 0);
        outb(self.bport(PO_CR), CR_RR);
        for _ in 0..100_000 {
            if inb(self.bport(PO_CR)) & CR_RR == 0 {
                outw(self.bport(PO_SR), SR_W1C);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("ICH AC97 PCM-out reset timed out")
    }

    fn init_codec(&self) -> Result<(), &'static str> {
        // Bring AC-link out of cold reset while keeping interrupt routing quiet.
        let mut gc = inl(self.bport(GLOB_CNT));
        gc |= GLOB_CNT_COLD;
        gc &= !GLOB_CNT_GIE;
        outl(self.bport(GLOB_CNT), gc);

        let mut ready = false;
        for _ in 0..200_000 {
            if inl(self.bport(GLOB_STA)) & GLOB_STA_PCR != 0 {
                ready = true;
                break;
            }
            io_wait();
        }
        if !ready {
            return Err("ICH AC97 primary codec not ready");
        }

        self.codec_write(AC97_RESET, 0);
        for _ in 0..2000 { io_wait(); }
        self.codec_write(AC97_POWERDOWN, 0);
        self.codec_write(AC97_MASTER_VOL, 0x0000);
        self.codec_write(AC97_PCM_OUT_VOL, 0x0000);

        // Felix audio core is 48 kHz. AC'97 guarantees 48 kHz even when VRA
        // is absent; if VRA exists, explicitly select 48 kHz as well.
        if self.codec_read(AC97_EXT_AUDIO_ID) & AC97_EXT_VRA != 0 {
            let e = self.codec_read(AC97_EXT_AUDIO_CTRL);
            self.codec_write(AC97_EXT_AUDIO_CTRL, e | AC97_EXT_VRA);
            self.codec_write(AC97_FRONT_DAC_RATE, 48_000);
        }

        let v1 = self.codec_read(AC97_VENDOR_ID1);
        let v2 = self.codec_read(AC97_VENDOR_ID2);
        crate::println!("[audio/ich] AC97 codec {:04x}:{:04x}", v1, v2);
        self.reset_pcm_out()
    }

    fn fragment_mut(&mut self, index: usize) -> &mut [i16] {
        let start = (index & 31) * SAMPLES_PER_FRAG;
        &mut self.dma.samples[start..start + SAMPLES_PER_FRAG]
    }

    fn fill_fragment(&mut self, index: usize, mixer: &mut Mixer) -> bool {
        let frag = self.fragment_mut(index);
        mixer.mix_into(frag)
    }

    fn prepare_bdl(&mut self) -> Result<(), &'static str> {
        let dma_phys = virt_to_phys(self.dma.samples.as_ptr())?;
        for i in 0..BDL_COUNT {
            let byte_off = (i * SAMPLES_PER_FRAG * core::mem::size_of::<i16>()) as u32;
            self.bdl.entries[i] = BdlEntry {
                addr: dma_phys.wrapping_add(byte_off),
                control_len: (SAMPLES_PER_FRAG as u32) | BD_BUP | BD_IOC,
            };
        }
        Ok(())
    }

    pub fn kick(&mut self, mixer: &mut Mixer) -> Result<(), &'static str> {
        if self.running || !mixer.has_data() {
            return Ok(());
        }

        self.reset_pcm_out()?;
        self.prepare_bdl()?;

        for i in 0..BDL_COUNT {
            self.fragment_mut(i).fill(0);
        }
        for i in 0..ACTIVE_FRAGS {
            let _ = self.fill_fragment(i, mixer);
        }

        let bdl_phys = virt_to_phys(self.bdl.entries.as_ptr())?;
        compiler_fence(Ordering::SeqCst);
        outl(self.bport(PO_BDBAR), bdl_phys);
        self.last_civ = 0;
        self.lvi = (ACTIVE_FRAGS - 1) as u8;
        self.idle_appends = 0;
        outb(self.bport(PO_LVI), self.lvi);
        outw(self.bport(PO_SR), SR_W1C);

        // Enable controller-global interrupt routing. If the PIC line is kept
        // masked, PIT polling still services these same status bits.
        let gc = inl(self.bport(GLOB_CNT)) | GLOB_CNT_GIE | GLOB_CNT_COLD;
        outl(self.bport(GLOB_CNT), gc);
        outb(self.bport(PO_CR), CR_RPBM | CR_FEIE | CR_IOCE);
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) {
        outb(self.bport(PO_CR), 0);
        outw(self.bport(PO_SR), SR_W1C);
        self.running = false;
        self.idle_appends = 0;
    }

    /// Inspect and service PCM-out status. Returns false when this controller
    /// has no pending source, which is mandatory for shared PCI INTx.
    pub fn service(&mut self, mixer: &mut Mixer) -> bool {
        if !self.running {
            return false;
        }
        let sr = inw(self.bport(PO_SR));
        let pending = sr & (SR_BCIS | SR_LVBCI | SR_FIFOE);
        if pending == 0 {
            return false;
        }

        outw(self.bport(PO_SR), pending);

        if sr & SR_FIFOE != 0 {
            crate::println!("[audio/ich] FIFO error SR={:#06x}", sr);
        }

        let civ = inb(self.bport(PO_CIV)) & 31;
        let mut completed = 0usize;
        while self.last_civ != civ && completed < BDL_COUNT {
            self.last_civ = (self.last_civ + 1) & 31;

            let append = ((self.lvi as usize + 1) & 31) as u8;
            let data = self.fill_fragment(append as usize, mixer);
            if data {
                self.idle_appends = 0;
            } else {
                self.idle_appends = self.idle_appends.saturating_add(1);
            }
            compiler_fence(Ordering::SeqCst);
            self.lvi = append;
            outb(self.bport(PO_LVI), self.lvi);
            completed += 1;
        }

        // If hardware reported completion before CIV visibly advanced, still
        // keep the queue moving by appending one fragment.
        if completed == 0 && sr & (SR_BCIS | SR_LVBCI) != 0 {
            let append = ((self.lvi as usize + 1) & 31) as u8;
            let data = self.fill_fragment(append as usize, mixer);
            if data { self.idle_appends = 0; } else { self.idle_appends += 1; }
            compiler_fence(Ordering::SeqCst);
            self.lvi = append;
            outb(self.bport(PO_LVI), self.lvi);
        }

        // After at least a full queued window of silence, all previously queued
        // audio has drained. Stop generating pointless IRQs; next write kicks it.
        if self.idle_appends >= ACTIVE_FRAGS + 2 && !mixer.has_data() {
            self.stop();
        } else if sr & SR_DCH != 0 && mixer.has_data() {
            // Resume if we briefly starved at LVI before software advanced it.
            outb(self.bport(PO_CR), CR_RPBM | CR_FEIE | CR_IOCE);
        }

        true
    }
}

pub fn probe_first() -> Result<Option<IchAc97>, &'static str> {
    let Some(dev) = crate::pci::enumerate()
        .into_iter()
        .find(|d| d.vendor_id == INTEL && supported(d.device_id))
    else {
        return Ok(None);
    };

    let nam = io_bar(&dev, 0)?;
    let nabm = io_bar(&dev, 1)?;
    dev.enable_bus_mastering();

    let bdl = Box::new(Bdl {
        entries: [BdlEntry { addr: 0, control_len: 0 }; BDL_COUNT],
    });
    let dma = Box::new(DmaBuffer { samples: [0; DMA_SAMPLES] });

    let mut card = IchAc97 {
        nam,
        nabm,
        irq: dev.interrupt_line,
        device_id: dev.device_id,
        revision: dev.revision_id,
        bdl,
        dma,
        running: false,
        last_civ: 0,
        lvi: 0,
        idle_appends: 0,
    };
    card.init_codec()?;
    card.prepare_bdl()?;

    crate::println!(
        "[audio/ich] {} NAM={:#06x} NABM={:#06x} irq={} rev={:#04x}",
        card.name(), card.nam, card.nabm, card.irq, card.revision
    );
    Ok(Some(card))
}
