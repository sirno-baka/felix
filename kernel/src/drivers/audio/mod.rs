//! Felix kernel audio core.
//!
//! Kernel stream format is intentionally fixed for the first complete path:
//! signed PCM16 little-endian, stereo, 48 kHz, interleaved L/R.
//! WAV/OGG/MP3 decoding belongs in userspace; `/dev/audio` receives PCM. A
//! small `play_wav` helper exists only for convenient kernel/hardware tests.

pub mod hda;
pub mod ich_ac97;
pub mod trident;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

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
        Self {
            owner_pid: -1,
            volume: 256,
            read: 0,
            write: 0,
            len: 0,
            data: [0; STREAM_SAMPLES],
        }
    }

    fn clear_for(&mut self, owner_pid: i32) {
        self.owner_pid = owner_pid.max(0);
        self.volume = 256;
        self.read = 0;
        self.write = 0;
        self.len = 0;
    }

    fn free_samples(&self) -> usize {
        STREAM_SAMPLES - self.len
    }

    fn can_write_stereo_frame(&self) -> bool {
        self.free_samples() >= 2
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
                Stream::new(),
                Stream::new(),
                Stream::new(),
                Stream::new(),
                Stream::new(),
                Stream::new(),
                Stream::new(),
                Stream::new(),
            ],
        }
    }

    fn stream_for(&mut self, owner_pid: i32) -> Option<&mut Stream> {
        let owner_pid = owner_pid.max(0);
        if let Some(i) = self.streams.iter().position(|s| s.owner_pid == owner_pid) {
            return Some(&mut self.streams[i]);
        }
        let idx = self
            .streams
            .iter()
            .position(|s| s.owner_pid < 0)
            .or_else(|| self.streams.iter().position(|s| s.len == 0))?;
        self.streams[idx].clear_for(owner_pid);
        Some(&mut self.streams[idx])
    }

    fn can_write(&self, owner_pid: i32) -> bool {
        let owner_pid = owner_pid.max(0);
        if let Some(stream) = self.streams.iter().find(|s| s.owner_pid == owner_pid) {
            return stream.can_write_stereo_frame();
        }
        self.streams.iter().any(|s| s.owner_pid < 0 || s.len == 0)
    }

    fn write_pcm16le(&mut self, owner_pid: i32, bytes: &[u8]) -> usize {
        let usable = bytes.len() & !3;
        if usable == 0 {
            return 0;
        }
        let Some(stream) = self.stream_for(owner_pid) else {
            return 0;
        };
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
                if stream.owner_pid < 0 || stream.len < 2 {
                    continue;
                }
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
        let Some(stream) = self
            .streams
            .iter_mut()
            .find(|s| s.owner_pid == owner_pid.max(0))
        else {
            return false;
        };
        stream.volume = volume.min(256);
        true
    }
}

pub(crate) enum Backend {
    Hda(hda::Hda),
    Ich(ich_ac97::IchAc97),
    Trident(trident::Trident),
}

impl Backend {
    fn name(&self) -> &'static str {
        match self {
            Self::Hda(v) => v.name(),
            Self::Ich(v) => v.name(),
            Self::Trident(v) => v.name(),
        }
    }
    fn irq(&self) -> u8 {
        match self {
            Self::Hda(v) => v.irq(),
            Self::Ich(v) => v.irq(),
            Self::Trident(v) => v.irq(),
        }
    }
    fn set_irq_enabled(&mut self, enabled: bool) {
        match self {
            Self::Hda(v) => v.set_irq_enabled(enabled),
            Self::Ich(v) => v.set_irq_enabled(enabled),
            Self::Trident(v) => v.set_irq_enabled(enabled),
        }
    }
    fn kick(&mut self, mixer: &mut Mixer) -> Result<(), &'static str> {
        match self {
            Self::Hda(v) => v.kick(mixer),
            Self::Ich(v) => v.kick(mixer),
            Self::Trident(v) => v.kick(mixer),
        }
    }
    fn ack_irq(&mut self) -> bool {
        match self {
            Self::Hda(v) => v.ack_irq(),
            Self::Ich(v) => v.ack_irq(),
            Self::Trident(v) => v.ack_irq(),
        }
    }
    fn poll(&mut self, mixer: &mut Mixer) -> bool {
        match self {
            Self::Hda(v) => v.poll(mixer),
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
        Self {
            mixer: Mixer::new(),
            backend: None,
        }
    }
}

static AUDIO: KMutex<AudioManager> = KMutex::new(AudioManager::new());
static AUDIO_INODE: AtomicU32 = AtomicU32::new(0);
static AUDIO_IRQ: AtomicU8 = AtomicU8::new(u8::MAX);
static AUDIO_IRQ_PENDING: AtomicBool = AtomicBool::new(false);
static AUDIO_IRQ_STATUS: AtomicU32 = AtomicU32::new(0);
static AUDIO_FALLBACK_TICK: AtomicU8 = AtomicU8::new(0);

const IRQ_ROUTE_NONE: u8 = 0;
const IRQ_ROUTE_HDA: u8 = 1;
const IRQ_ROUTE_ICH: u8 = 2;
const IRQ_ROUTE_TRIDENT: u8 = 3;
static AUDIO_IRQ_ROUTE: AtomicU8 = AtomicU8::new(IRQ_ROUTE_NONE);
static AUDIO_IRQ_ARG0: AtomicUsize = AtomicUsize::new(0);
static AUDIO_IRQ_ARG1: AtomicU32 = AtomicU32::new(0);
static AUDIO_IRQ_ARG2: AtomicU32 = AtomicU32::new(0);

fn publish_irq_route(backend: &Backend) {
    match backend {
        Backend::Hda(v) => {
            let (mmio, stream) = v.irq_ack_cookie();
            AUDIO_IRQ_ARG0.store(mmio, Ordering::Relaxed);
            AUDIO_IRQ_ARG1.store(stream as u32, Ordering::Relaxed);
            AUDIO_IRQ_ARG2.store(0, Ordering::Relaxed);
            AUDIO_IRQ_ROUTE.store(IRQ_ROUTE_HDA, Ordering::Release);
        }
        Backend::Ich(v) => {
            AUDIO_IRQ_ARG0.store(v.irq_ack_port() as usize, Ordering::Relaxed);
            AUDIO_IRQ_ARG1.store(0, Ordering::Relaxed);
            AUDIO_IRQ_ARG2.store(0, Ordering::Relaxed);
            AUDIO_IRQ_ROUTE.store(IRQ_ROUTE_ICH, Ordering::Release);
        }
        Backend::Trident(v) => {
            let (iobase, aint, voice_mask) = v.irq_ack_cookie();
            AUDIO_IRQ_ARG0.store(iobase as usize, Ordering::Relaxed);
            AUDIO_IRQ_ARG1.store(aint as u32, Ordering::Relaxed);
            AUDIO_IRQ_ARG2.store(voice_mask, Ordering::Relaxed);
            AUDIO_IRQ_ROUTE.store(IRQ_ROUTE_TRIDENT, Ordering::Release);
        }
    }
}

fn ack_irq_without_audio_lock() -> u32 {
    match AUDIO_IRQ_ROUTE.load(Ordering::Acquire) {
        IRQ_ROUTE_HDA => hda::Hda::ack_irq_raw(
            AUDIO_IRQ_ARG0.load(Ordering::Relaxed),
            AUDIO_IRQ_ARG1.load(Ordering::Relaxed) as usize,
        ),
        IRQ_ROUTE_ICH => ich_ac97::IchAc97::ack_irq_raw(
            AUDIO_IRQ_ARG0.load(Ordering::Relaxed) as u16,
        ),
        IRQ_ROUTE_TRIDENT => trident::Trident::ack_irq_raw(
            AUDIO_IRQ_ARG0.load(Ordering::Relaxed) as u16,
            AUDIO_IRQ_ARG1.load(Ordering::Relaxed) as u16,
            AUDIO_IRQ_ARG2.load(Ordering::Relaxed),
        ),
        _ => 0,
    }
}

pub struct AudioCharDevice;

impl CharDevice for AudioCharDevice {
    fn read(&self, _offset: u64, _buf: &mut [u8]) -> usize {
        0
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> usize {
        // PID is stable and monotonic in Felix, unlike the scheduler slot. This
        // prevents a newly-created process from inheriting a drained/playing
        // stream merely because its slot was reused.
        let pid = unsafe { crate::multitasking::task::TASK_MANAGER.current_pid() };
        write_stream(pid.max(0), buf)
    }
}

pub fn init() {
    let backend = match hda::probe_first() {
        Ok(Some(v)) => Some(Backend::Hda(v)),
        Ok(None) => match ich_ac97::probe_first() {
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
        },
        Err(e) => {
            crate::println!("[audio] Intel HDA probe failed: {}", e);
            match ich_ac97::probe_first() {
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
                    None
                }
            }
        }
    };

    let Some(mut backend) = backend else {
        crate::println!("[audio] no supported controller");
        return;
    };

    let irq = backend.irq();
    AUDIO_IRQ.store(irq, Ordering::Release);
    let name = backend.name();
    backend.set_irq_enabled(false);
    // Publish only immutable hardware coordinates to the hard-IRQ path. The
    // mutable mixer/backend remains exclusively protected by AUDIO.
    publish_irq_route(&backend);
    AUDIO.lock().backend = Some(backend);
    let irq_result = crate::drivers::shared_irq::register_named(irq, irq_entry, "audio");
    if let Some(backend) = AUDIO.lock().backend.as_mut() {
        backend.set_irq_enabled(irq_result.is_ok());
    }

    let inode = DevFS::register_char_global("audio", Box::new(AudioCharDevice));
    AUDIO_INODE.store(inode, Ordering::Release);

    match irq_result {
        Ok(()) => crate::println!(
            "[audio] {} ready: /dev/audio S16LE 48000 stereo; shared IRQ{}, deferred refill",
            name, irq
        ),
        Err(e) => crate::println!(
            "[audio] {} ready: /dev/audio S16LE 48000 stereo; polling fallback ({})",
            name, e
        ),
    }
}

pub fn is_available() -> bool {
    AUDIO
        .try_lock()
        .map(|g| g.backend.is_some())
        .unwrap_or(false)
}

pub fn device_inode() -> u32 {
    AUDIO_INODE.load(Ordering::Acquire)
}

pub fn is_audio_inode(global_inode: u32) -> bool {
    let local_inode = global_inode & 0x00FF_FFFF;
    let audio_inode = AUDIO_INODE.load(Ordering::Acquire);

    audio_inode != 0 && local_inode == audio_inode
}

pub fn write_stream(owner_pid: i32, bytes: &[u8]) -> usize {
    match try_write_stream(owner_pid, bytes) {
        StreamWrite::Written(written) => written,
        StreamWrite::WouldBlock | StreamWrite::Unavailable => 0,
    }
}

/// Result of one attempt to append PCM data to the software mixer.
///
/// Waiting is deliberately left to the syscall layer. Sleeping here would
/// keep the process-wide SMP kernel lock held and stall every CPU until the
/// PIT bottom half made room in the ring.
pub enum StreamWrite {
    Written(usize),
    WouldBlock,
    Unavailable,
}

pub fn stream_writable(owner_pid: i32) -> bool {
    let Some(audio) = AUDIO.try_lock() else {
        return false;
    };
    audio.backend.is_some() && audio.mixer.can_write(owner_pid)
}

pub fn try_write_stream(owner_pid: i32, bytes: &[u8]) -> StreamWrite {
    if bytes.is_empty() {
        return StreamWrite::Written(0);
    }
    if bytes.len() < 4 {
        return StreamWrite::Written(0);
    }

    let mut audio = AUDIO.lock();
    if audio.backend.is_none() {
        return StreamWrite::Unavailable;
    }
    let written = audio.mixer.write_pcm16le(owner_pid, bytes);
    if written != 0 {
        let AudioManager { mixer, backend } = &mut *audio;
        if let Some(backend) = backend.as_mut() {
            if let Err(e) = backend.kick(mixer) {
                crate::println!("[audio] start failed: {}", e);
            }
        }
    }
    if written == 0 {
        StreamWrite::WouldBlock
    } else {
        StreamWrite::Written(written)
    }
}

fn irq_entry(irq: u8) -> bool {
    if AUDIO_IRQ.load(Ordering::Acquire) != irq {
        return false;
    }
    // Never take AUDIO from hard IRQ context: another CPU may hold it while
    // programming/refilling the same backend. Acknowledge only through the
    // immutable route published before device interrupts were enabled.
    let status = ack_irq_without_audio_lock();
    if status != 0 {
        AUDIO_IRQ_STATUS.fetch_or(status, Ordering::AcqRel);
        AUDIO_IRQ_PENDING.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

/// Interrupt-driven refill with a low-rate polling safety net for hardware
/// that loses an edge or is temporarily inaccessible from another CPU.
pub fn poll_due() -> bool {
    if AUDIO_IRQ_PENDING.swap(false, Ordering::AcqRel) {
        return true;
    }
    AUDIO_FALLBACK_TICK.fetch_add(1, Ordering::Relaxed) & 3 == 0
}

pub fn poll() -> bool {
    let Some(mut audio) = AUDIO.try_lock() else {
        return false;
    };
    let AudioManager { mixer, backend } = &mut *audio;
    if let Some(backend) = backend.as_mut() {
        let irq_status = AUDIO_IRQ_STATUS.swap(0, Ordering::AcqRel);
        if irq_status != 0 {
            match backend {
                Backend::Ich(v) => v.note_irq_status(irq_status as u16),
                Backend::Trident(v) => v.note_irq(),
                Backend::Hda(_) => {}
            }
        }
        let _ = backend.ack_irq();
        backend.poll(mixer)
    } else {
        false
    }
}

#[inline]
fn wav_u16(data: &[u8], off: usize) -> Result<u16, &'static str> {
    if off + 2 > data.len() {
        return Err("truncated WAV");
    }
    Ok(u16::from_le_bytes([data[off], data[off + 1]]))
}

#[inline]
fn wav_u32(data: &[u8], off: usize) -> Result<u32, &'static str> {
    if off + 4 > data.len() {
        return Err("truncated WAV");
    }
    Ok(u32::from_le_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
    ]))
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
            if chunk.len() < 16 {
                return Err("invalid WAV fmt chunk");
            }
            format = Some(wav_u16(chunk, 0)?);
            channels = Some(wav_u16(chunk, 2)?);
            rate = Some(wav_u32(chunk, 4)?);
            bits = Some(wav_u16(chunk, 14)?);
        } else if id == b"data" {
            pcm = Some(chunk);
        }
        pos += size + (size & 1);
    }

    if format != Some(1) {
        return Err("WAV must be uncompressed PCM");
    }
    if channels != Some(CHANNELS as u16) {
        return Err("WAV must be stereo");
    }
    if rate != Some(SAMPLE_RATE) {
        return Err("WAV must be 48000 Hz");
    }
    if bits != Some(BITS as u16) {
        return Err("WAV must be 16-bit");
    }
    let pcm = pcm.ok_or("WAV has no data chunk")?;
    if pcm.len() & 3 != 0 {
        return Err("WAV data is not whole stereo PCM16 frames");
    }

    Ok(write_stream(0, pcm))
}
