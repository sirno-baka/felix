use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Write};
#[cfg(target_os = "popugos")]
use std::os::popugos::fs::{OpenOptionsExt, O_NONBLOCK};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
#[cfg(target_os = "popugos")]
use tokio::io::{popugos::AsyncFd, Interest};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecParameters, Decoder, DecoderOptions, CODEC_TYPE_AAC};
use symphonia::core::formats::Packet;

const OUTPUT_RATE: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdtsConfig {
    object_type: u8,
    frequency_index: u8,
    sample_rate: u32,
    channels: u8,
}

enum AudioOutput {
    File(File),
    #[cfg(target_os = "popugos")]
    Device(AsyncFd<File>),
}

pub struct AudioPlayer {
    output: AudioOutput,
    decoder: Option<Box<dyn Decoder>>,
    config: Option<AdtsConfig>,
    volume: Arc<AtomicU16>,
    timestamp: u64,
}

impl AudioPlayer {
    pub fn open(volume: Arc<AtomicU16>, output_path: Option<&str>) -> Result<Self, Box<dyn Error>> {
        let output = if let Some(path) = output_path {
            AudioOutput::File(
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(path)?,
            )
        } else {
            #[cfg(target_os = "popugos")]
            {
                let mut options = OpenOptions::new();
                options.write(true);
                options.custom_flags(O_NONBLOCK);
                let file = options.open("/dev/audio")?;
                AudioOutput::Device(AsyncFd::with_interest(file, Interest::WRITABLE)?)
            }
            #[cfg(not(target_os = "popugos"))]
            {
                AudioOutput::File(OpenOptions::new().write(true).open("/dev/audio")?)
            }
        };
        Ok(Self { output, decoder: None, config: None, volume, timestamp: 0 })
    }

    pub async fn decode_adts(&mut self, bytes: &[u8]) -> Result<usize, Box<dyn Error>> {
        let mut pos = 0usize;
        let mut decoded_frames = 0usize;
        while pos + 7 <= bytes.len() {
            if bytes[pos] != 0xff || bytes[pos + 1] & 0xf6 != 0xf0 {
                pos += 1;
                continue;
            }
            let protection_absent = bytes[pos + 1] & 1 != 0;
            let header_len = if protection_absent { 7 } else { 9 };
            let frame_len = (((bytes[pos + 3] & 0x03) as usize) << 11)
                | ((bytes[pos + 4] as usize) << 3)
                | ((bytes[pos + 5] as usize) >> 5);
            if frame_len < header_len || pos + frame_len > bytes.len() {
                break;
            }
            let config = parse_config(&bytes[pos..pos + 7])?;
            self.ensure_decoder(config)?;
            let payload = &bytes[pos + header_len..pos + frame_len];
            let packet = Packet::new_from_slice(0, self.timestamp, 1024, payload);
            self.timestamp = self.timestamp.saturating_add(1024);

            // Do not hold a borrow of the decoder across the async device
            // write. Convert this AAC frame to an owned PCM buffer first.
            let (decoded_rate, channels, sample_count, pcm) = {
                let decoded = self.decoder.as_mut().unwrap().decode(&packet)?;
                if decoded.spec().rate != OUTPUT_RATE {
                    return Err(format!(
                        "unsupported AAC sample rate {}; expected {OUTPUT_RATE}",
                        decoded.spec().rate
                    )
                    .into());
                }
                let channels = decoded.spec().channels.count();
                if channels == 0 || channels > 2 {
                    return Err(format!("unsupported AAC channel count {channels}").into());
                }
                let decoded_rate = decoded.spec().rate;
                let mut samples =
                    SampleBuffer::<i16>::new(decoded.capacity() as u64, *decoded.spec());
                samples.copy_interleaved_ref(decoded);
                let sample_count = samples.samples().len();
                let pcm = samples_to_pcm(
                    samples.samples(),
                    channels,
                    self.volume.load(Ordering::Relaxed) as i32,
                );
                (decoded_rate, channels, sample_count, pcm)
            };

            if decoded_frames == 0 {
                println!(
                    "[audio] first AAC frame decoded rate={} channels={} samples={}; writing PCM",
                    decoded_rate,
                    channels,
                    sample_count
                );
            }
            self.write_pcm(&pcm).await?;
            if decoded_frames == 0 {
                println!("[audio] first PCM frame write complete");
            }
            decoded_frames += 1;
            pos += frame_len;
        }
        Ok(decoded_frames)
    }

    fn ensure_decoder(&mut self, config: AdtsConfig) -> Result<(), Box<dyn Error>> {
        if self.config == Some(config) {
            return Ok(());
        }
        if config.sample_rate != OUTPUT_RATE {
            return Err(format!("AAC is {} Hz; this version supports only {OUTPUT_RATE} Hz", config.sample_rate).into());
        }
        let asc = vec![
            (config.object_type << 3) | (config.frequency_index >> 1),
            ((config.frequency_index & 1) << 7) | (config.channels << 3),
        ];
        let mut params = CodecParameters::new();
        params.codec = CODEC_TYPE_AAC;
        params.sample_rate = Some(config.sample_rate);
        params.extra_data = Some(asc.into_boxed_slice());
        self.decoder = Some(symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?);
        self.config = Some(config);
        self.timestamp = 0;
        Ok(())
    }

    async fn write_pcm(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.output {
            AudioOutput::File(output) => output.write_all(bytes),
            #[cfg(target_os = "popugos")]
            AudioOutput::Device(output) => {
                let mut offset = 0usize;
                while offset < bytes.len() {
                    let mut ready = output.writable_mut().await?;
                    match ready.try_io(|async_fd| {
                        async_fd.get_mut().write(&bytes[offset..])
                    }) {
                        Ok(Ok(0)) => {
                            return Err(io::Error::new(
                                ErrorKind::WriteZero,
                                "/dev/audio returned a zero-length write",
                            ));
                        }
                        Ok(Ok(written)) => offset += written,
                        Ok(Err(error)) => return Err(error),
                        Err(_) => continue,
                    }
                }
                Ok(())
            }
        }
    }
}

fn parse_config(header: &[u8]) -> Result<AdtsConfig, Box<dyn Error>> {
    const RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];
    let frequency_index = (header[2] >> 2) & 0x0f;
    let sample_rate = *RATES.get(frequency_index as usize).ok_or("invalid ADTS frequency index")?;
    let channels = ((header[2] & 1) << 2) | (header[3] >> 6);
    if channels == 0 {
        return Err("AAC program-config-element channels are not supported".into());
    }
    Ok(AdtsConfig {
        object_type: ((header[2] >> 6) & 0x03) + 1,
        frequency_index,
        sample_rate,
        channels,
    })
}

fn samples_to_pcm(input: &[i16], channels: usize, volume: i32) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(if channels == 1 { input.len() * 4 } else { input.len() * 2 });
    for frame in input.chunks_exact(channels) {
        let left = ((frame[0] as i32 * volume) / 100)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let right_source = if channels == 1 { frame[0] } else { frame[1] };
        let right = ((right_source as i32 * volume) / 100)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        pcm.extend_from_slice(&left.to_le_bytes());
        pcm.extend_from_slice(&right.to_le_bytes());
    }
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_twitch_style_adts_header() {
        let header = [0xff, 0xf1, 0x4c, 0x80, 0x00, 0x1f, 0xfc];
        let config = parse_config(&header).unwrap();
        assert_eq!(config.object_type, 2);
        assert_eq!(config.sample_rate, 48_000);
        assert_eq!(config.channels, 2);
    }
}
