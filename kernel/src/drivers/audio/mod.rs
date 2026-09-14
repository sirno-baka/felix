pub mod ich_ac97;
pub mod trident;

use alloc::vec::Vec;
use crate::println;

pub fn init() {
    ich_ac97::init();
    trident::init();
}

fn read_u16_le(data: &[u8], off: usize) -> Result<u16, &'static str> {
    if off + 2 > data.len() {
        return Err("unexpected end of WAV");
    }

    Ok(u16::from_le_bytes([
        data[off],
        data[off + 1],
    ]))
}

fn read_u32_le(data: &[u8], off: usize) -> Result<u32, &'static str> {
    if off + 4 > data.len() {
        return Err("unexpected end of WAV");
    }

    Ok(u32::from_le_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
    ]))
}

/// Play RIFF/WAVE PCM 16-bit mono/stereo.
///
/// Automatically reads:
/// - sample rate
/// - channel count
/// - PCM data chunk
pub fn play_wav(data: &[u8]) -> Result<(), &'static str> {
    if data.len() < 12 {
        return Err("WAV too small");
    }

    if &data[0..4] != b"RIFF" {
        return Err("not RIFF");
    }

    if &data[8..12] != b"WAVE" {
        return Err("not WAVE");
    }

    let mut pos = 12usize;

    let mut format = None;
    let mut channels = None;
    let mut sample_rate = None;
    let mut bits_per_sample = None;

    let mut pcm_data: Option<&[u8]> = None;

    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = read_u32_le(data, pos + 4)? as usize;

        pos += 8;

        if pos + size > data.len() {
            return Err("invalid WAV chunk");
        }

        let chunk = &data[pos..pos + size];

        if id == b"fmt " {
            if chunk.len() < 16 {
                return Err("invalid WAV fmt chunk");
            }

            format = Some(read_u16_le(chunk, 0)?);
            channels = Some(read_u16_le(chunk, 2)?);
            sample_rate = Some(read_u32_le(chunk, 4)?);
            bits_per_sample = Some(read_u16_le(chunk, 14)?);
        } else if id == b"data" {
            pcm_data = Some(chunk);
        }

        // RIFF chunks are aligned to 2 bytes.
        pos += size + (size & 1);
    }

    if format != Some(1) {
        return Err("WAV compression is not PCM");
    }

    if bits_per_sample != Some(16) {
        return Err("WAV is not PCM16");
    }

    let channels = channels.ok_or("WAV has no channel count")?;

    if channels != 1 && channels != 2 {
        return Err("only mono/stereo WAV supported");
    }

    let sample_rate =
        sample_rate.ok_or("WAV has no sample rate")?;

    let pcm =
        pcm_data.ok_or("WAV has no data chunk")?;

    if pcm.len() & 1 != 0 {
        return Err("invalid PCM16 byte count");
    }

    let mut samples = Vec::with_capacity(pcm.len() / 2);

    for b in pcm.chunks_exact(2) {
        samples.push(i16::from_le_bytes([
            b[0],
            b[1],
        ]));
    }

    crate::println!(
        "[audio] WAV: {} Hz, {} ch, {} samples",
        sample_rate,
        channels,
        samples.len(),
    );

    // Prefer Intel ICH AC'97 if present.
    if ich_ac97::is_available() {
        return ich_ac97::play_pcm16(
            &samples,
            sample_rate,
            channels as u8,
        );
    }

    // Otherwise try Trident/SiS/ALi.
    if trident::is_available() {
        return trident::play_pcm16(
            &samples,
            sample_rate,
            channels as u8,
        );
    }

    Err("no supported audio device")
}