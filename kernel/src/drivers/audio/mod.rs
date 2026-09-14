//! Felix kernel audio core.
//!
//! Kernel stream format is intentionally fixed for the first complete path:
//! signed PCM16 little-endian, stereo, 48 kHz, interleaved L/R.
//! WAV/OGG/MP3 decoding belongs in userspace; `/dev/audio` receives PCM. A
//! small `play_wav` helper exists only for convenient kernel/hardware tests.

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
const STREAM_SAMPLES: usize = 32_768;

struct Stream {
    owner_pid: i32,
    volume: u16,
    read: usize,
    write: usize,
    len: usize,
    data: [i16; STREAM_SAMPLES],
}

impl Stream {
    const fn new() -> Self {
        Self { owner_pid: -1, volume: 256, read: 0, write: 0, len: 0, data: [0; STREAM_SAMPLES] }
    }

    fn clear_for(&mut self, owner_pid: i32) {
        self.owner_pid = owner_pid.max(0);
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

    fn stream_for(&mut self, owner_pid: i32) -> Option<&mut Stream> {
        let owner_pid = owner_pid.max(0);
        if let Some(i) = self.streams.iter().position(|s| s.owner_pid == owner_pid) {
            return Some(&mut self.streams[i]);
        }
        let idx = self.streams.iter().position(|s| s.owner_pid < 0)
            .or_else(|| self.streams.iter().position(|s| s.len == 0))?;
        self.streams[idx].clear_for(owner_pid);
        Some(&mut self.streams[idx])
    }

    fn write_pcm16le(&mut self, owner_pid: i32, bytes: &[u8]) -> usize {
        let usable = bytes.len() & !3;
        if usable == 0 { return 0; }
        let Some(stream) = self.stream_for(owner_pid) else { return 0; };
        let samples = (usable / 2).min(stream.free_samples() & !1) & !1;
        for i in 0..samples {
            let p = i * 2;
            if !stream.push(i16::from_le_bytes([bytes[p], bytes[p + 1]])) {
                return i * 2;
            }
        }
        samples * 2
    }

    pub(crate) fn mix_into(&mut self, out: &mut [i16]) -> bool {
        let count = out.len() & !1;
        let mut supplied = false;
        let mut p = 0usize;
        while p < count {
            let mut left = 0i32;
            let mut right = 0i32;
            for stream in &mut self.streams {
                if stream.owner_pid < 0 || stream.len < 2 { continue; }
                let vol = stream.volume as i32;
                left += (stream.pop().unwrap_or(0) as i32 * vol) / 256;
                right += (stream.pop().unwrap_or(0) as i32 * vol) / 256;
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

    pub fn set_volume(&mut self, owner_pid: i32, volume: u16) -> bool {
        let Some(stream) = self.streams.iter_mut().find(|s| s.owner_pid == owner_pid.max(0)) else { return false; };
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
        match self { Self::Ich(v) => v.name(), Self::Trident(v) => v.name() }
    }
    fn irq(&self) -> u8 {
        match self { Self::Ich(v) => v.irq(), Self::Trident(v) => v.irq() }
    }
    fn set_irq_enabled(&mut self, enabled: bool) {
        match self {
            Self::Ich(v) => v.set_irq_enabled(enabled),
            Self::Trident(v) => v.set_irq_enabled(enabled),
        }
    }
    fn kick(&mut self, mixer: &mut Mixer) -> Result<(), &'static str> {
        match self { Self::Ich(v) => v.kick(mixer), Self::Trident(v) => v.kick(mixer) }
    }
    fn ack_irq(&mut self) -> bool {
        match self { Self::Ich(v) => v.ack_irq(), Self::Trident(v) => v.ack_irq() }
    }
    fn poll(&mut self, mixer: &mut Mixer) {
        match self { Self::Ich(v) => v.poll(mixer), Self::Trident(v) => v.poll(mixer) }
    }
}

struct AudioManager {
    mixer: Mixer,
    backend: Option<Backend>,
}

impl AudioManager {
    const fn new() -> Self { Self { mixer: Mixer::new(), backend: None } }
}

static AUDIO: KMutex<AudioManager> = KMutex::new(AudioManager::new());
static AUDIO_INODE: AtomicU32 = AtomicU32::new(0);

pub struct AudioCharDevice;

impl CharDevice for AudioCharDevice {
    fn read(&self, _offset: u64, _buf: &mut [u8]) -> usize { 0 }

    fn write(&self, _offset: u64, buf: &[u8]) -> usize {
        // PID is stable and monotonic in Felix, unlike the scheduler slot. This
        // prevents a newly-created process from inheriting a drained/playing
        // stream merely because its slot was reused.
        let pid = unsafe { crate::multitasking::task::TASK_MANAGER.current_pid() };
        write_stream(pid.max(0), buf)
    }
}

pub fn init() {
    let backend = match ich_ac97::probe_first() {
        Ok(Some(v)) => Some(Backend::Ich(v)),
        Ok(None) => match trident::probe_first() {
            Ok(Some(v)) => Some(Backend::Trident(v)),
            Ok(None) => None,
            Err(e) => { crate::println!("[audio] Trident/SiS/ALi probe failed: {}", e); None }
        },
        Err(e) => {
            crate::println!("[audio] Intel ICH probe failed: {}", e);
            match trident::probe_first() {
                Ok(Some(v)) => Some(Backend::Trident(v)),
                Ok(None) => None,
                Err(e) => { crate::println!("[audio] Trident/SiS/ALi probe failed: {}", e); None }
            }
        }
    };

    let Some(mut backend) = backend else {
        crate::println!("[audio] no supported controller");
        return;
    };

    let irq = backend.irq();
    let name = backend.name();
    let irq_result = crate::drivers::shared_irq::register(irq, irq_entry);
    backend.set_irq_enabled(irq_result.is_ok());
    AUDIO.lock().backend = Some(backend);

    let inode = DevFS::register_char_global("audio", Box::new(AudioCharDevice));
    AUDIO_INODE.store(inode, Ordering::Release);

    match irq_result {
        Ok(()) => crate::println!(
            "[audio] {} ready: /dev/audio S16LE 48000 stereo; IRQ{} shared, PIT refill",
            name, irq
        ),
        Err(e) => crate::println!(
            "[audio] {} ready: /dev/audio S16LE 48000 stereo; pure polling ({})",
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

pub fn write_stream(owner_pid: i32, bytes: &[u8]) -> usize {
    let mut audio = AUDIO.lock();
    if audio.backend.is_none() { return 0; }
    let written = audio.mixer.write_pcm16le(owner_pid, bytes);
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

fn irq_entry(irq: u8) -> bool {
    let Some(mut audio) = AUDIO.try_lock() else { return false; };
    let Some(backend) = audio.backend.as_mut() else { return false; };
    if backend.irq() != irq { return false; }
    backend.ack_irq()
}

pub fn poll() {
    let Some(mut audio) = AUDIO.try_lock() else { return; };
    let AudioManager { mixer, backend } = &mut *audio;
    if let Some(backend) = backend.as_mut() { backend.poll(mixer); }
}

#[inline]
fn wav_u16(data: &[u8], off: usize) -> Result<u16, &'static str> {
    if off + 2 > data.len() { return Err("truncated WAV"); }
    Ok(u16::from_le_bytes([data[off], data[off + 1]]))
}

#[inline]
fn wav_u32(data: &[u8], off: usize) -> Result<u32, &'static str> {
    if off + 4 > data.len() { return Err("truncated WAV"); }
    Ok(u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]))
}

/// Convenience helper for kernel/hardware tests:
/// `drivers::audio::play_wav(include_bytes!("audio.wav"))`.
///
/// It deliberately performs no resampling/conversion; the file must already be
/// PCM16, stereo, 48 kHz. Large files can be only partially queued, exactly as
/// a nonblocking `/dev/audio` write can be partial.
pub fn play_wav(data: &[u8]) -> Result<usize, &'static str> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file");
    }

    let mut pos = 12usize;
    let mut format = None;
    let mut channels = None;
    let mut rate = None;
    let mut bits = None;
    let mut pcm = None;

    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = wav_u32(data, pos + 4)? as usize;
        pos += 8;
        if pos.checked_add(size).map_or(true, |end| end > data.len()) {
            return Err("invalid WAV chunk size");
        }
        let chunk = &data[pos..pos + size];
        if id == b"fmt " {
            if chunk.len() < 16 { return Err("invalid WAV fmt chunk"); }
            format = Some(wav_u16(chunk, 0)?);
            channels = Some(wav_u16(chunk, 2)?);
            rate = Some(wav_u32(chunk, 4)?);
            bits = Some(wav_u16(chunk, 14)?);
        } else if id == b"data" {
            pcm = Some(chunk);
        }
        pos += size + (size & 1);
    }

    if format != Some(1) { return Err("WAV must be uncompressed PCM"); }
    if channels != Some(CHANNELS as u16) { return Err("WAV must be stereo"); }
    if rate != Some(SAMPLE_RATE) { return Err("WAV must be 48000 Hz"); }
    if bits != Some(BITS as u16) { return Err("WAV must be 16-bit"); }
    let pcm = pcm.ok_or("WAV has no data chunk")?;
    if pcm.len() & 3 != 0 { return Err("WAV data is not whole stereo PCM16 frames"); }

    Ok(write_stream(0, pcm))
}
