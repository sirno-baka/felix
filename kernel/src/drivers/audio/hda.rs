//! Minimal Intel High Definition Audio playback backend.
//!
//! First target: Intel 6 Series/C200 (8086:1c20) + Conexant CX20590
//! (14f1:506e), as found in ThinkPad X220/T420 generation machines.
//!
//! Deliberately polling-only:
//! - codec verbs use the Immediate Command interface (ICOI/ICII/ICIS)
//! - stream progress is observed through LPIB from the PIT audio poll
//! - INTCTL and stream interrupt-enable bits stay disabled
//! - one cyclic output stream, PCM S16LE / stereo / 48 kHz

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, compiler_fence};

use crate::drivers::audio::Mixer;
use crate::memory::resources::{DmaBuffer as DmaAllocation, ResourceKind, dma_alloc_for, dma_free, reserve_and_ioremap};
use crate::pci::bar::Bar;
use crate::pci::device::PciDevice;

const INTEL: u16 = 0x8086;
const COUGAR_POINT_HDA: u16 = 0x1c20;

// Global HDA registers.
const GCAP: usize = 0x00;
const GCTL: usize = 0x08;
const STATESTS: usize = 0x0e;
const INTCTL: usize = 0x20;
const CORBCTL: usize = 0x4c;
const RIRBCTL: usize = 0x5c;
const ICOI: usize = 0x60;
const ICII: usize = 0x64;
const ICIS: usize = 0x68;

const GCTL_CRST: u32 = 1 << 0;
const ICIS_ICB: u16 = 1 << 0;
const ICIS_IRV: u16 = 1 << 1;

// Stream descriptors begin at 0x80, first all input streams, then output.
const SD_BASE: usize = 0x80;
const SD_STRIDE: usize = 0x20;
const SD_CTL0: usize = 0x00;
const SD_CTL2: usize = 0x02;
const SD_STS: usize = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0c;
const SD_FMT: usize = 0x12;
const SD_BDLPL: usize = 0x18;
const SD_BDLPU: usize = 0x1c;

const SD_CTL_SRST: u8 = 1 << 0;
const SD_CTL_RUN: u8 = 1 << 1;
const SD_STS_W1C: u8 = (1 << 2) | (1 << 3) | (1 << 4);

// 48 kHz base, x1 /1, 16-bit, 2 channels.
const HDA_FMT_48K_S16_STEREO: u16 = 0x0011;
const STREAM_TAG: u8 = 1;

// Codec parameters / verbs.
const PARAM_VENDOR_ID: u8 = 0x00;
const PARAM_NODE_COUNT: u8 = 0x04;
const PARAM_FUNCTION_TYPE: u8 = 0x05;

const VERB_GET_PARAMETER: u16 = 0x0f00;
const VERB_SET_CONNECT_SEL: u16 = 0x0701;
const VERB_SET_POWER_STATE: u16 = 0x0705;
const VERB_SET_CHANNEL_STREAMID: u16 = 0x0706;
const VERB_SET_PIN_WIDGET_CONTROL: u16 = 0x0707;

// 4-bit / 16-bit-payload verbs.
const VERB_SET_STREAM_FORMAT_4BIT: u8 = 0x2;
const VERB_SET_AMP_GAIN_MUTE_4BIT: u8 = 0x3;

const AMP_SET_OUTPUT: u16 = 1 << 15;
const AMP_SET_LEFT: u16 = 1 << 13;
const AMP_SET_RIGHT: u16 = 1 << 12;

const PINCTL_OUT: u8 = 1 << 6;
const PINCTL_HP: u8 = 1 << 7;

// CX20590 / Conexant 20672 used in ThinkPad X220/T420.
const CX20590_VENDOR_ID: u32 = 0x14f1_506e;
const CX20590_AFG: u8 = 0x01;
const CX20590_DAC: u8 = 0x10;
const CX20590_HP_PIN: u8 = 0x19;
const CX20590_SPK_PIN: u8 = 0x1f;
const CX20590_MAX_GAIN: u16 = 0x4a;

const BDL_COUNT: usize = 4;
const FRAMES_PER_FRAG: usize = 1024;
const SAMPLES_PER_FRAG: usize = FRAMES_PER_FRAG * 2;
const BYTES_PER_FRAG: usize = SAMPLES_PER_FRAG * core::mem::size_of::<i16>();
const DMA_SAMPLES: usize = BDL_COUNT * SAMPLES_PER_FRAG;
const DMA_BYTES: usize = DMA_SAMPLES * core::mem::size_of::<i16>();

#[repr(C)]
#[derive(Clone, Copy)]
struct BdlEntry {
    addr_lo: u32,
    addr_hi: u32,
    length: u32,
    flags: u32,
}

#[repr(C, align(128))]
struct Bdl {
    entries: [BdlEntry; BDL_COUNT],
}

#[repr(C, align(4096))]
struct DmaBuffer {
    samples: [i16; DMA_SAMPLES],
}

pub struct Hda {
    mmio: usize,
    irq: u8,
    codec: u8,
    codec_vendor: u32,
    stream: usize,
    bdl: *mut Bdl,
    dma: *mut DmaBuffer,
    bdl_mem: DmaAllocation,
    dma_mem: DmaAllocation,
    running: bool,
    last_frag: usize,
    idle_refills: usize,
    debug_polls: u8,
    debug_last_lpib: u32,
}

unsafe impl Send for Hda {}

impl Hda {
    pub fn name(&self) -> &'static str {
        match self.codec_vendor {
            CX20590_VENDOR_ID => "Intel HDA / Conexant CX20590",
            _ => "Intel High Definition Audio",
        }
    }

    pub fn irq(&self) -> u8 {
        self.irq
    }

    // Audio core keeps the same backend API as legacy cards. HDA intentionally
    // ignores this and remains polling-only for now.
    pub fn set_irq_enabled(&mut self, _enabled: bool) {
        self.w32(INTCTL, 0);
    }

    #[inline]
    fn r8(&self, reg: usize) -> u8 {
        unsafe { read_volatile((self.mmio + reg) as *const u8) }
    }
    #[inline]
    fn r16(&self, reg: usize) -> u16 {
        unsafe { read_volatile((self.mmio + reg) as *const u16) }
    }
    #[inline]
    fn r32(&self, reg: usize) -> u32 {
        unsafe { read_volatile((self.mmio + reg) as *const u32) }
    }
    #[inline]
    fn w8(&self, reg: usize, value: u8) {
        unsafe { write_volatile((self.mmio + reg) as *mut u8, value) }
    }
    #[inline]
    fn w16(&self, reg: usize, value: u16) {
        unsafe { write_volatile((self.mmio + reg) as *mut u16, value) }
    }
    #[inline]
    fn w32(&self, reg: usize, value: u32) {
        unsafe { write_volatile((self.mmio + reg) as *mut u32, value) }
    }

    #[inline]
    fn sr(&self, off: usize) -> usize {
        self.stream + off
    }

    fn map_bar(dev: &PciDevice) -> Result<(usize, u32, u32), &'static str> {
        let (phys, size) = match dev.get_bar(0) {
            Some(Bar::Memory { address, size, .. }) if *address != 0 => {
                (*address, (*size).max(0x1000))
            }
            Some(Bar::Memory { .. }) => return Err("HDA BAR0 has zero address"),
            Some(Bar::Io { .. }) => return Err("HDA BAR0 is I/O, expected MMIO"),
            _ => return Err("HDA missing BAR0"),
        };

        let virt = reserve_and_ioremap(phys as u64, size as usize, ResourceKind::Mmio, "hda-mmio")
            .map_err(|_| "HDA MMIO mapping failed")?;
        Ok((virt.0 as usize, phys, size))
    }

    fn controller_reset(&self) -> Result<u16, &'static str> {
        // No controller or stream interrupts in this backend.
        self.w32(INTCTL, 0);
        self.w8(CORBCTL, 0);
        self.w8(RIRBCTL, 0);

        let g = self.r32(GCTL);
        self.w32(GCTL, g & !GCTL_CRST);
        for _ in 0..200_000 {
            if self.r32(GCTL) & GCTL_CRST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if self.r32(GCTL) & GCTL_CRST != 0 {
            return Err("HDA controller reset assert timeout");
        }

        self.w32(GCTL, self.r32(GCTL) | GCTL_CRST);
        for _ in 0..500_000 {
            if self.r32(GCTL) & GCTL_CRST != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if self.r32(GCTL) & GCTL_CRST == 0 {
            return Err("HDA controller reset release timeout");
        }

        // Give codecs time to signal presence; STATESTS is W1C, so only read it.
        let mut sts = 0u16;
        for _ in 0..500_000 {
            sts = self.r16(STATESTS) & 0x7fff;
            if sts != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if sts == 0 {
            return Err("HDA no codec present in STATESTS");
        }
        Ok(sts)
    }

    fn immediate(&self, codec: u8, nid: u8, verb12: u16, payload: u8) -> Result<u32, &'static str> {
        for _ in 0..200_000 {
            if self.r16(ICIS) & ICIS_ICB == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if self.r16(ICIS) & ICIS_ICB != 0 {
            return Err("HDA immediate interface busy");
        }

        // Clear old response-valid, then launch a new command.
        self.w16(ICIS, ICIS_IRV);
        let cmd = ((codec as u32) << 28)
            | ((nid as u32) << 20)
            | (((verb12 as u32) & 0x0fff) << 8)
            | payload as u32;
        self.w32(ICOI, cmd);
        self.w16(ICIS, ICIS_ICB);

        for _ in 0..300_000 {
            let s = self.r16(ICIS);
            if s & ICIS_ICB == 0 && s & ICIS_IRV != 0 {
                let response = self.r32(ICII);
                self.w16(ICIS, ICIS_IRV);
                return Ok(response);
            }
            core::hint::spin_loop();
        }
        Err("HDA immediate command timeout")
    }

    fn immediate16(
        &self,
        codec: u8,
        nid: u8,
        verb4: u8,
        payload: u16,
    ) -> Result<u32, &'static str> {
        for _ in 0..200_000 {
            if self.r16(ICIS) & ICIS_ICB == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if self.r16(ICIS) & ICIS_ICB != 0 {
            return Err("HDA immediate interface busy");
        }

        self.w16(ICIS, ICIS_IRV);
        let cmd = ((codec as u32) << 28)
            | ((nid as u32) << 20)
            | (((verb4 as u32) & 0x0f) << 16)
            | payload as u32;
        self.w32(ICOI, cmd);
        self.w16(ICIS, ICIS_ICB);

        for _ in 0..300_000 {
            let s = self.r16(ICIS);
            if s & ICIS_ICB == 0 && s & ICIS_IRV != 0 {
                let response = self.r32(ICII);
                self.w16(ICIS, ICIS_IRV);
                return Ok(response);
            }
            core::hint::spin_loop();
        }
        Err("HDA immediate command timeout")
    }

    #[inline]
    fn get_parameter(&self, nid: u8, param: u8) -> Result<u32, &'static str> {
        self.immediate(self.codec, nid, VERB_GET_PARAMETER, param)
    }

    fn find_codec(&mut self, present: u16) -> Result<(), &'static str> {
        for codec in 0..15u8 {
            if present & (1u16 << codec) == 0 {
                continue;
            }
            self.codec = codec;
            let vendor = self.immediate(codec, 0, VERB_GET_PARAMETER, PARAM_VENDOR_ID)?;
            crate::println!("[audio/hda] codec{} vendor={:08x}", codec, vendor);
            if vendor == CX20590_VENDOR_ID {
                self.codec_vendor = vendor;
                return Ok(());
            }
        }
        Err("HDA CX20590 codec not found")
    }

    fn dump_codec_shape(&self) {
        if let Ok(root_nodes) = self.get_parameter(0, PARAM_NODE_COUNT) {
            let first = ((root_nodes >> 16) & 0xff) as u8;
            let count = (root_nodes & 0xff) as u8;
            crate::println!(
                "[audio/hda] root nodes first={:#04x} count={}",
                first,
                count
            );
            for n in first..first.saturating_add(count) {
                if let Ok(ft) = self.get_parameter(n, PARAM_FUNCTION_TYPE) {
                    crate::println!("[audio/hda] node {:#04x} function-type={:#x}", n, ft & 0xff);
                }
            }
        }
    }

    fn setup_cx20590(&self) -> Result<(), &'static str> {
        // Function group + playback widgets to D0.
        self.immediate(self.codec, CX20590_AFG, VERB_SET_POWER_STATE, 0)?;
        self.immediate(self.codec, CX20590_DAC, VERB_SET_POWER_STATE, 0)?;
        self.immediate(self.codec, CX20590_SPK_PIN, VERB_SET_POWER_STATE, 0)?;
        self.immediate(self.codec, CX20590_HP_PIN, VERB_SET_POWER_STATE, 0)?;

        // Both output pins use DAC 0x10 for the first, known-working path.
        self.immediate(self.codec, CX20590_SPK_PIN, VERB_SET_CONNECT_SEL, 0)?;
        self.immediate(self.codec, CX20590_HP_PIN, VERB_SET_CONNECT_SEL, 0)?;
        self.immediate(
            self.codec,
            CX20590_SPK_PIN,
            VERB_SET_PIN_WIDGET_CONTROL,
            PINCTL_OUT,
        )?;
        self.immediate(
            self.codec,
            CX20590_HP_PIN,
            VERB_SET_PIN_WIDGET_CONTROL,
            PINCTL_OUT | PINCTL_HP,
        )?;

        // Unmute both DAC channels.
        let amp = AMP_SET_OUTPUT | AMP_SET_LEFT | AMP_SET_RIGHT | CX20590_MAX_GAIN;
        self.immediate16(self.codec, CX20590_DAC, VERB_SET_AMP_GAIN_MUTE_4BIT, amp)?;

        self.immediate(
            self.codec,
            CX20590_DAC,
            VERB_SET_CHANNEL_STREAMID,
            STREAM_TAG << 4,
        )?;
        self.immediate16(
            self.codec,
            CX20590_DAC,
            VERB_SET_STREAM_FORMAT_4BIT,
            HDA_FMT_48K_S16_STEREO,
        )?;
        Ok(())
    }

    fn reset_stream(&self) -> Result<(), &'static str> {
        self.w8(self.sr(SD_CTL0), self.r8(self.sr(SD_CTL0)) & !SD_CTL_RUN);
        self.w8(self.sr(SD_CTL0), self.r8(self.sr(SD_CTL0)) | SD_CTL_SRST);
        for _ in 0..100_000 {
            if self.r8(self.sr(SD_CTL0)) & SD_CTL_SRST != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if self.r8(self.sr(SD_CTL0)) & SD_CTL_SRST == 0 {
            return Err("HDA stream reset assert timeout");
        }
        self.w8(self.sr(SD_CTL0), self.r8(self.sr(SD_CTL0)) & !SD_CTL_SRST);
        for _ in 0..100_000 {
            if self.r8(self.sr(SD_CTL0)) & SD_CTL_SRST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if self.r8(self.sr(SD_CTL0)) & SD_CTL_SRST != 0 {
            return Err("HDA stream reset release timeout");
        }
        self.w8(self.sr(SD_STS), SD_STS_W1C);
        Ok(())
    }

    fn prepare_bdl(&mut self) -> Result<(), &'static str> {
        let dma_phys = self.dma_mem.phys.0;
        for i in 0..BDL_COUNT {
            unsafe { (*self.bdl).entries[i] = BdlEntry {
                addr_lo: dma_phys.wrapping_add((i * BYTES_PER_FRAG) as u32),
                addr_hi: 0,
                length: BYTES_PER_FRAG as u32,
                flags: 0, // polling only: no IOC
            }; }
        }
        Ok(())
    }

    fn fragment_mut(&mut self, index: usize) -> &mut [i16] {
        let start = (index % BDL_COUNT) * SAMPLES_PER_FRAG;
        unsafe {
            let samples = &mut (*self.dma).samples;
            &mut samples[start..start + SAMPLES_PER_FRAG]
        }
    }

    fn fill_fragment(&mut self, index: usize, mixer: &mut Mixer) -> bool {
        mixer.mix_into(self.fragment_mut(index))
    }

    fn program_stream(&mut self) -> Result<(), &'static str> {
        self.reset_stream()?;
        self.prepare_bdl()?;
        let bdl_phys = self.bdl_mem.phys.0;

        self.w32(self.sr(SD_BDLPL), bdl_phys);
        self.w32(self.sr(SD_BDLPU), 0);
        self.w32(self.sr(SD_CBL), DMA_BYTES as u32);
        self.w16(self.sr(SD_LVI), (BDL_COUNT - 1) as u16);
        self.w16(self.sr(SD_FMT), HDA_FMT_48K_S16_STEREO);
        self.w8(self.sr(SD_STS), SD_STS_W1C);

        // Stream tag is bits 7:4 of CTL byte 2. No interrupt-enable bits.
        let ctl2 = self.r8(self.sr(SD_CTL2));
        self.w8(self.sr(SD_CTL2), (ctl2 & 0x0f) | (STREAM_TAG << 4));
        Ok(())
    }

    pub fn kick(&mut self, mixer: &mut Mixer) -> Result<(), &'static str> {
        if self.running || !mixer.has_data() {
            return Ok(());
        }

        self.reset_stream()?;
        unsafe { (*self.dma).samples.fill(0); }
        self.idle_refills = 0;
        for i in 0..BDL_COUNT {
            if self.fill_fragment(i, mixer) {
                self.idle_refills = 0;
            } else {
                self.idle_refills += 1;
            }
        }
        self.prepare_bdl()?;
        self.program_stream()?;
        self.setup_cx20590()?;

        compiler_fence(Ordering::SeqCst);
        self.last_frag = 0;
        self.debug_polls = 0;
        self.debug_last_lpib = 0;
        self.w8(self.sr(SD_CTL0), self.r8(self.sr(SD_CTL0)) | SD_CTL_RUN);
        self.running = true;

        let dma_phys = self.dma_mem.phys.0;
        let bdl_phys = self.bdl_mem.phys.0;
        crate::println!(
            "[audio/hda] RUN stream={:#x} dma={:#010x} bdl={:#010x} CBL={} LVI={} FMT={:#06x} CTL={:02x}:{:02x}:{:02x} STS={:#04x} LPIB={}",
            self.stream,
            dma_phys,
            bdl_phys,
            self.r32(self.sr(SD_CBL)),
            self.r16(self.sr(SD_LVI)),
            self.r16(self.sr(SD_FMT)),
            self.r8(self.sr(SD_CTL2)),
            self.r8(self.sr(SD_CTL0 + 1)),
            self.r8(self.sr(SD_CTL0)),
            self.r8(self.sr(SD_STS)),
            self.r32(self.sr(SD_LPIB)),
        );
        Ok(())
    }

    fn stop(&mut self) {
        self.w8(self.sr(SD_CTL0), self.r8(self.sr(SD_CTL0)) & !SD_CTL_RUN);
        self.w8(self.sr(SD_STS), SD_STS_W1C);
        self.running = false;
        self.idle_refills = 0;
    }

    // Not used in polling mode; retained so audio::Backend has one shape.
    pub fn ack_irq(&mut self) -> bool {
        false
    }

    /// PIT bottom half: use LPIB to see which BDL fragment hardware has left,
    /// refill those fragments, and never depend on HDA interrupts.
    pub fn poll(&mut self, mixer: &mut Mixer) {
        if !self.running {
            return;
        }

        let lpib_raw = self.r32(self.sr(SD_LPIB));
        let lpib = (lpib_raw as usize) % DMA_BYTES;
        let current = (lpib / BYTES_PER_FRAG).min(BDL_COUNT - 1);

        if self.debug_polls < 8 || lpib_raw != self.debug_last_lpib {
            if self.debug_polls < 8 {
                crate::println!(
                    "[audio/hda] poll#{} LPIB={} frag={} CTL={:#04x} STS={:#04x}",
                    self.debug_polls,
                    lpib_raw,
                    current,
                    self.r8(self.sr(SD_CTL0)),
                    self.r8(self.sr(SD_STS)),
                );
            }
            self.debug_last_lpib = lpib_raw;
            self.debug_polls = self.debug_polls.saturating_add(1);
        }
        let mut progressed = 0usize;

        while self.last_frag != current && progressed < BDL_COUNT {
            let safe = self.last_frag;
            let supplied = self.fill_fragment(safe, mixer);
            if supplied {
                self.idle_refills = 0;
            } else {
                self.idle_refills = self.idle_refills.saturating_add(1);
            }
            compiler_fence(Ordering::SeqCst);
            self.last_frag = (self.last_frag + 1) % BDL_COUNT;
            progressed += 1;
        }

        // ~6 fragments = ~128 ms of silence after the queued data drains.
        if self.idle_refills >= BDL_COUNT + 2 && !mixer.has_data() {
            self.stop();
        }
    }
}

pub fn probe_first() -> Result<Option<Hda>, &'static str> {
    let Some(dev) = crate::pci::enumerate()
        .into_iter()
        .find(|d| d.vendor_id == INTEL && d.device_id == COUGAR_POINT_HDA)
    else {
        return Ok(None);
    };

    // Intel PCH HDA PCI quirks, matching Linux snd_hda_intel:
    // - TCSEL[2:0] must be zero; non-zero traffic class is known to cause
    //   playback static on some HDA codecs.
    // - DEVC.NOSNOOP (bit 11) must be clear so DMA is cache-coherent with
    //   the CPU. This matters on real Cougar Point hardware; QEMU is coherent
    //   regardless and therefore hides this class of bug.
    let tcsel_before = dev.read_u8(0x44);
    let devc_before = dev.read_u16(0x78);
    dev.write_u8(0x44, tcsel_before & !0x07);
    dev.write_u16(0x78, devc_before & !(1 << 11));
    let tcsel_after = dev.read_u8(0x44);
    let devc_after = dev.read_u16(0x78);
    crate::println!(
        "[audio/hda] PCI TCSEL {:#04x}->{:#04x} DEVC {:#06x}->{:#06x} (snoop={})",
        tcsel_before,
        tcsel_after,
        devc_before,
        devc_after,
        if devc_after & (1 << 11) == 0 {
            "on"
        } else {
            "off"
        },
    );

    // HDA uses MMIO + bus-master DMA. Preserve unrelated PCI command bits.
    let cmd = dev.read_u16(0x04);
    dev.write_u16(0x04, cmd | 0x0006);

    let (mmio, phys, size) = Hda::map_bar(&dev)?;
    let bdl_mem = dma_alloc_for("hda BDL", core::mem::size_of::<Bdl>(), core::mem::align_of::<Bdl>(), u32::MAX as u64)
        .map_err(|_| "HDA BDL DMA allocation failed")?;
    let dma_mem = match dma_alloc_for("hda PCM", core::mem::size_of::<DmaBuffer>(), core::mem::align_of::<DmaBuffer>(), u32::MAX as u64) {
        Ok(mem) => mem,
        Err(_) => {
            let _ = dma_free(bdl_mem);
            return Err("HDA PCM DMA allocation failed");
        }
    };
    let bdl = bdl_mem.as_mut_ptr() as *mut Bdl;
    let dma = dma_mem.as_mut_ptr() as *mut DmaBuffer;

    let mut card = Hda {
        mmio,
        irq: dev.interrupt_line,
        codec: 0,
        codec_vendor: 0,
        stream: 0,
        bdl,
        dma,
        bdl_mem,
        dma_mem,
        running: false,
        last_frag: 0,
        idle_refills: 0,
        debug_polls: 0,
        debug_last_lpib: 0,
    };

    let present = card.controller_reset()?;
    let gcap = card.r16(GCAP);
    let iss = ((gcap >> 8) & 0x0f) as usize;
    let oss = ((gcap >> 12) & 0x0f) as usize;
    if oss == 0 {
        return Err("HDA controller has no output streams");
    }
    card.stream = SD_BASE + iss * SD_STRIDE;

    crate::println!(
        "[audio/hda] 8086:1c20 BAR0={:#010x} size={:#x} virt={:#010x} irq={} GCAP={:#06x} ISS={} OSS={} stream={:#x} codecs={:#06x}",
        phys,
        size,
        mmio,
        card.irq,
        gcap,
        iss,
        oss,
        card.stream,
        present,
    );

    card.find_codec(present)?;
    card.dump_codec_shape();
    card.setup_cx20590()?;
    card.program_stream()?;

    crate::println!(
        "[audio/hda] CX20590 ready: DAC=0x10 speaker=0x1f hp=0x19, polling LPIB, no IRQ"
    );
    Ok(Some(card))
}
