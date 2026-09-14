//! Felix kernel audio core.
//!
//! Kernel-side stream format is deliberately fixed for now:
//! signed PCM16 little-endian, stereo, 48 kHz, interleaved L/R.
//! Decoders (WAV/OGG/MP3) belong in userspace; `/dev/audio` receives PCM.

pub mod ich_ac97;
pub mod trident;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::device::char::CharDevice;
use crate::filesystem::devfs::DevFS;
use crate::spin::KMutex;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u8 = 2;
pub const BITS: u8 = 16;

const MAX_STREAMS: usize = 8;
// 32768 interleaved samples = 16384 stereo frames ~= 341 ms at 48 kHz.
const STREAM_SAMPLES: usize = 32_768;

struct Stream {
    owner: i16,
    volume: u16, // 0..=256
    read: usize,
    write: usize,
    len: usize,
    data: [i16; STREAM_SAMPLES],
}

impl Stream {
    const fn new() -> Self {
        Self {
            owner: -1,
            volume: 256,
            read: 0,
            write: 0,
            len: 0,
            data: [0; STREAM_SAMPLES],
        }
    }

    fn clear_for(&mut self, owner: usize) {
        self.owner = owner.min(i16::MAX as usize) as i16;
        self.volume = 256;
        self.read = 0;
        self.write = 0;
        self.len = 0;
    }

    fn free_samples(&self) -> usize {
        STREAM_SAMPLES - self.len
    }

    fn push(&mut self, s: i16) -> bool {
        if self.len == STREAM_SAMPLES {
            return false;
        }
        self.data[self.write] = s;
        self.write = (self.write + 1) % STREAM_SAMPLES;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<i16> {
        if self.len == 0 {
            return None;
        }
        let s = self.data[self.read];
        self.read = (self.read + 1) % STREAM_SAMPLES;
        self.len -= 1;
        Some(s)
    }
}

pub(crate) struct Mixer {
    streams: [Stream; MAX_STREAMS],
}

impl Mixer {
    const fn new() -> Self {
        Self {
            streams: [
                Stream::new(), Stream::new(), Stream::new(), Stream::new(),
                Stream::new(), Stream::new(), Stream::new(), Stream::new(),
            ],
        }
    }

    fn stream_for(&mut self, owner: usize) -> Option<&mut Stream> {
        let owner_i = owner.min(i16::MAX as usize) as i16;
        if let Some(i) = self.streams.iter().position(|s| s.owner == owner_i) {
            return Some(&mut self.streams[i]);
        }

        // Prefer an unused slot, then reclaim a completely drained stream.
        let idx = self.streams
            .iter()
            .position(|s| s.owner < 0)
            .or_else(|| self.streams.iter().position(|s| s.len == 0))?;
        self.streams[idx].clear_for(owner);
        Some(&mut self.streams[idx])
    }

    fn write_pcm16le(&mut self, owner: usize, bytes: &[u8]) -> usize {
        // Stereo frame is exactly four bytes. Never split an L/R frame between
        // writes because the mixer works in interleaved stereo samples.
        let usable = bytes.len() & !3;
        if usable == 0 {
            return 0;
        }
        let Some(stream) = self.stream_for(owner) else { return 0; };
        let room = stream.free_samples() & !1;
        let samples_wanted = usable / 2;
        let samples = samples_wanted.min(room) & !1;

        for i in 0..samples {
            let p = i * 2;
            let s = i16::from_le_bytes([bytes[p], bytes[p + 1]]);
            if !stream.push(s) {
                return i * 2;
            }
        }
        samples * 2
    }

    /// Mix one hardware fragment. Returns true if at least one stream supplied
    /// non-silence data (the actual sample value may still happen to be zero).
    pub(crate) fn mix_into(&mut self, out: &mut [i16]) -> bool {
        let count = out.len() & !1;
        let mut supplied = false;

        let mut p = 0usize;
        while p < count {
            let mut left = 0i32;
            let mut right = 0i32;

            for stream in &mut self.streams {
                if stream.owner < 0 || stream.len < 2 {
                    continue;
                }
                let l = stream.pop().unwrap_or(0) as i32;
                let r = stream.pop().unwrap_or(0) as i32;
                let vol = stream.volume as i32;
                left += (l * vol) / 256;
                right += (r * vol) / 256;
                supplied = true;
            }

            out[p] = left.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            out[p + 1] = right.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            p += 2;
        }
        for s in &mut out[count..] {
            *s = 0;
        }
        supplied
    }

    pub(crate) fn has_data(&self) -> bool {
        self.streams.iter().any(|s| s.len >= 2)
    }

    pub fn set_volume(&mut self, owner: usize, volume: u16) -> bool {
        let owner_i = owner.min(i16::MAX as usize) as i16;
        let Some(stream) = self.streams.iter_mut().find(|s| s.owner == owner_i) else {
            return false;
        };
        stream.volume = volume.min(256);
        true
    }
}

pub(crate) enum Backend {
    Ich(ich_ac97::IchAc97),
    Trident(trident::Trident),
}

impl Backend {
    fn name(&self) -> &'static str {
        match self {
            Self::Ich(v) => v.name(),
            Self::Trident(v) => v.name(),
        }
    }

    fn irq(&self) -> u8 {
        match self {
            Self::Ich(v) => v.irq(),
            Self::Trident(v) => v.irq(),
        }
    }

    fn kick(&mut self, mixer: &mut Mixer) -> Result<(), &'static str> {
        match self {
            Self::Ich(v) => v.kick(mixer),
            Self::Trident(v) => v.kick(mixer),
        }
    }

    fn service(&mut self, mixer: &mut Mixer) -> bool {
        match self {
            Self::Ich(v) => v.service(mixer),
            Self::Trident(v) => v.service(mixer),
        }
    }
}

struct AudioManager {
    mixer: Mixer,
    backend: Option<Backend>,
}

impl AudioManager {
    const fn new() -> Self {
        Self {
            mixer: Mixer::new(),
            backend: None,
        }
    }
}

static AUDIO: KMutex<AudioManager> = KMutex::new(AudioManager::new());
static AUDIO_INODE: AtomicU32 = AtomicU32::new(0);

pub struct AudioCharDevice;

impl CharDevice for AudioCharDevice {
    fn read(&self, _offset: u64, _buf: &mut [u8]) -> usize {
        0
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> usize {
        // Generic VFS callers do not carry process identity. Sys_write uses the
        // owner-aware fast path below; owner 0 is kept for kernel/test writes.
        write_stream(0, buf)
    }
}

/// Probe one playback controller, create `/dev/audio`, and register its shared
/// legacy INTx owner. Intel ICH is preferred when both are present.
pub fn init() {
    let backend = match ich_ac97::probe_first() {
        Ok(Some(v)) => Some(Backend::Ich(v)),
        Ok(None) => match trident::probe_first() {
            Ok(Some(v)) => Some(Backend::Trident(v)),
            Ok(None) => None,
            Err(e) => {
                crate::println!("[audio] Trident/SiS/ALi probe failed: {}", e);
                None
            }
        },
        Err(e) => {
            crate::println!("[audio] Intel ICH probe failed: {}", e);
            match trident::probe_first() {
                Ok(Some(v)) => Some(Backend::Trident(v)),
                Ok(None) => None,
                Err(e) => {
                    crate::println!("[audio] Trident/SiS/ALi probe failed: {}", e);
                    None
                }
            }
        }
    };

    let Some(backend) = backend else {
        crate::println!("[audio] no supported controller");
        return;
    };

    let irq = backend.irq();
    let name = backend.name();
    AUDIO.lock().backend = Some(backend);

    let inode = DevFS::register_char_global("audio", Box::new(AudioCharDevice));
    AUDIO_INODE.store(inode, Ordering::Release);

    match crate::drivers::shared_irq::register(irq, irq_entry) {
        Ok(()) => crate::println!(
            "[audio] {} ready: /dev/audio = S16LE 48000Hz stereo, IRQ{} shared + PIT fallback",
            name,
            irq
        ),
        Err(e) => crate::println!(
            "[audio] {} ready: /dev/audio = S16LE 48000Hz stereo, IRQ{} polling fallback ({})",
            name,
            irq,
            e
        ),
    }
}

pub fn is_available() -> bool {
    AUDIO.try_lock().map(|g| g.backend.is_some()).unwrap_or(false)
}

pub fn device_inode() -> u32 {
    AUDIO_INODE.load(Ordering::Acquire)
}

pub fn is_audio_inode(inode: u32) -> bool {
    let own = device_inode();
    own != 0 && own == inode
}

/// Owner-aware `/dev/audio` write used by sys_write. Input is fixed-format
/// little-endian PCM16 stereo 48 kHz. A partial return means the stream ring is
/// full; userspace may retry the remainder after the next DMA fragment.
pub fn write_stream(owner: usize, bytes: &[u8]) -> usize {
    let mut audio = AUDIO.lock();
    if audio.backend.is_none() {
        return 0;
    }

    let written = audio.mixer.write_pcm16le(owner, bytes);
    if written != 0 {
        let AudioManager { mixer, backend } = &mut *audio;
        if let Some(backend) = backend.as_mut() {
            if let Err(e) = backend.kick(mixer) {
                crate::println!("[audio] start failed: {}", e);
            }
        }
    }
    written
}

/// Shared-IRQ callback. Every backend first checks its own status registers and
/// returns false when the interrupt belongs to another PCI device on the line.
fn irq_entry(irq: u8) -> bool {
    let Some(mut audio) = AUDIO.try_lock() else { return false; };
    let AudioManager { mixer, backend } = &mut *audio;
    let Some(backend) = backend.as_mut() else { return false; };
    if backend.irq() != irq {
        return false;
    }
    backend.service(mixer)
}

/// PIT fallback for old machines where a shared INTx line is unreliable or has
/// been storm-masked. It is deliberately non-blocking and does no allocation.
pub fn poll() {
    let Some(mut audio) = AUDIO.try_lock() else { return; };
    let AudioManager { mixer, backend } = &mut *audio;
    if let Some(backend) = backend.as_mut() {
        let _ = backend.service(mixer);
    }
}
