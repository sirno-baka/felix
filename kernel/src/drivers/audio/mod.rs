//! Felix kernel audio core.
//!
//! Kernel stream format is intentionally fixed for the first complete path:
//! signed PCM16 little-endian, stereo, 48 kHz, interleaved L/R.
//! WAV/OGG/MP3 decoding belongs in userspace; `/dev/audio` receives PCM.

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

    fn free_samples(&self) -> usize { STREAM_SAMPLES - self.len }

    fn push(&mut self, s: i16) -> bool {
        if self.len == STREAM_SAMPLES { return false; }
        self.data[self.write] = s;
        self.write = (self.write + 1) % STREAM_SAMPLES;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<i16> {
        if self.len == 0 { return None; }
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

        let idx = self.streams
            .iter()
            .position(|s| s.owner < 0)
            .or_else(|| self.streams.iter().position(|s| s.len == 0))?;
        self.streams[idx].clear_for(owner);
        Some(&mut self.streams[idx])
    }

    fn write_pcm16le(&mut self, owner: usize, bytes: &[u8]) -> usize {
        // One stereo frame is exactly four bytes. Do not split L/R between
        // writes; callers can retry a partial write on the next scheduler turn.
        let usable = bytes.len() & !3;
        if usable == 0 { return 0; }
        let Some(stream) = self.stream_for(owner) else { return 0; };
        let room = stream.free_samples() & !1;
        let samples = (usable / 2).min(room) & !1;

        for i in 0..samples {
            let p = i * 2;
            if !stream.push(i16::from_le_bytes([bytes[p], bytes[p + 1]])) {
                return i * 2;
            }
        }
        samples * 2
    }

    /// Mix one hardware fragment. Returns true when at least one stream supplied
    /// a frame. Saturating/clipping is done in i32 so concurrent streams cannot
    /// wrap around and produce loud digital garbage.
    pub(crate) fn mix_into(&mut self, out: &mut [i16]) -> bool {
        let count = out.len() & !1;
        let mut supplied = false;
        let mut p = 0usize;

        while p < count {
            let mut left = 0i32;
            let mut right = 0i32;
            for stream in &mut self.streams {
                if stream.owner < 0 || stream.len < 2 { continue; }
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
        out[count..].fill(0);
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

    /// IRQ top half: status test + hardware ACK only.
    fn ack_irq(&mut self) -> bool {
        match self {
            Self::Ich(v) => v.ack_irq(),
            Self::Trident(v) => v.ack_irq(),
        }
    }

    /// PIT bottom half: consume pending completions and refill DMA.
    fn poll(&mut self, mixer: &mut Mixer) {
        match self {
            Self::Ich(v) => v.poll(mixer),
            Self::Trident(v) => v.poll(mixer),
        }
    }
}

struct AudioManager {
    mixer: Mixer,
    backend: Option<Backend>,
}

impl AudioManager {
    const fn new() -> Self {
        Self { mixer: Mixer::new(), backend: None }
    }
}

static AUDIO: KMutex<AudioManager> = KMutex::new(AudioManager::new());
static AUDIO_INODE: AtomicU32 = AtomicU32::new(0);

pub struct AudioCharDevice;

impl CharDevice for AudioCharDevice {
    fn read(&self, _offset: u64, _buf: &mut [u8]) -> usize { 0 }

    fn write(&self, _offset: u64, buf: &[u8]) -> usize {
        // Generic kernel/VFS writes have no process identity. sys_write uses the
        // owner-aware fast path and therefore gets its own mixer stream.
        write_stream(0, buf)
    }
}

/// Probe one playback controller, create `/dev/audio`, then attach its status
/// callback to the shared legacy INTx dispatcher. Intel ICH is preferred when
/// both controller families happen to be present.
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
            "[audio] {} ready: /dev/audio S16LE 48000 stereo; IRQ{} shared, PIT refill",
            name, irq
        ),
        Err(e) => crate::println!(
            "[audio] {} ready: /dev/audio S16LE 48000 stereo; polling only ({})",
            name, e
        ),
    }
}

pub fn is_available() -> bool {
    AUDIO.try_lock().map(|g| g.backend.is_some()).unwrap_or(false)
}

pub fn device_inode() -> u32 { AUDIO_INODE.load(Ordering::Acquire) }

pub fn is_audio_inode(inode: u32) -> bool {
    let own = device_inode();
    own != 0 && own == inode
}

/// Owner-aware `/dev/audio` write used by sys_write. Input is fixed-format
/// S16LE/48k/stereo. A partial return simply means this stream ring is full.
pub fn write_stream(owner: usize, bytes: &[u8]) -> usize {
    let mut audio = AUDIO.lock();
    if audio.backend.is_none() { return 0; }

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

/// Shared IRQ callback: no allocation, no mixer, no waiting. A backend MUST
/// return false when its status registers say this interrupt belongs elsewhere.
fn irq_entry(irq: u8) -> bool {
    let Some(mut audio) = AUDIO.try_lock() else { return false; };
    let Some(backend) = audio.backend.as_mut() else { return false; };
    if backend.irq() != irq { return false; }
    backend.ack_irq()
}

/// Audio bottom half. Called from PIT every tick. try_lock means a syscall that
/// is currently copying/mixing PCM is never deadlocked by the timer interrupt.
pub fn poll() {
    let Some(mut audio) = AUDIO.try_lock() else { return; };
    let AudioManager { mixer, backend } = &mut *audio;
    if let Some(backend) = backend.as_mut() {
        backend.poll(mixer);
    }
}
