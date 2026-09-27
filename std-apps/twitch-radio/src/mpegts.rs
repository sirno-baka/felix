use std::collections::BTreeMap;

#[derive(Clone, Copy)]
enum PesKind {
    Audio,
    Video,
}

#[derive(Default)]
struct PesStream {
    selected: bool,
    bytes: Vec<u8>,
}

fn flush(stream: &mut PesStream, output: &mut Vec<u8>) {
    if stream.selected {
        output.append(&mut stream.bytes);
    } else {
        stream.bytes.clear();
    }
    stream.selected = false;
}

/// Stateful MPEG-TS -> H.264 PES demuxer for live HLS.
///
/// HLS segment boundaries are not treated as PES boundaries. A PES packet may
/// start in one .ts segment and continue in the next one.
#[derive(Default)]
pub struct H264Demuxer {
    streams: BTreeMap<u16, PesStream>,
}

impl H264Demuxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one complete MPEG-TS HLS segment. The currently open PES is kept
    /// for the next segment instead of being flushed at the HLS boundary.
    pub fn push_segment(&mut self, segment: &[u8]) -> Result<Vec<u8>, String> {
        extract_pes_chunk(segment, PesKind::Video, &mut self.streams)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        for stream in self.streams.values_mut() {
            flush(stream, &mut output);
        }
        output
    }
}

/// Extracts AAC elementary-stream bytes from one standalone MPEG-TS blob.
pub fn extract_aac(segment: &[u8]) -> Result<Vec<u8>, String> {
    extract_pes(segment, PesKind::Audio, "AAC audio")
}

/// Extracts H.264 elementary-stream bytes from one standalone MPEG-TS blob.
/// Live playback should use H264Demuxer so PES state survives HLS boundaries.
pub fn extract_h264(segment: &[u8]) -> Result<Vec<u8>, String> {
    extract_pes(segment, PesKind::Video, "H.264 video")
}

fn extract_pes(segment: &[u8], kind: PesKind, label: &str) -> Result<Vec<u8>, String> {
    let mut streams = BTreeMap::new();
    let mut output = extract_pes_chunk(segment, kind, &mut streams)?;
    for stream in streams.values_mut() {
        flush(stream, &mut output);
    }

    if output.is_empty() {
        Err(format!("no {label} PES packets in segment"))
    } else {
        Ok(output)
    }
}

fn extract_pes_chunk(
    segment: &[u8],
    kind: PesKind,
    streams: &mut BTreeMap<u16, PesStream>,
) -> Result<Vec<u8>, String> {
    let offset = (0..segment.len().min(188))
        .find(|&i| segment[i] == 0x47 && segment.get(i + 188).is_none_or(|b| *b == 0x47))
        .ok_or_else(|| "MPEG-TS sync byte not found".to_string())?;

    let mut output = Vec::new();
    let mut pos = offset;

    while pos + 188 <= segment.len() {
        let packet = &segment[pos..pos + 188];
        pos += 188;

        if packet[0] != 0x47 || packet[1] & 0x80 != 0 {
            continue;
        }

        let payload_start = packet[1] & 0x40 != 0;
        let pid = (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16;
        let control = (packet[3] >> 4) & 0x03;
        if control == 0 || control == 2 {
            continue;
        }

        let mut p = 4usize;
        if control == 3 {
            p += 1 + packet[4] as usize;
        }
        if p >= 188 {
            continue;
        }

        let stream = streams.entry(pid).or_default();
        if payload_start {
            // New PES start is the reliable boundary for the previous PES.
            flush(stream, &mut output);

            let payload = &packet[p..];
            if payload.len() < 9 || payload[..3] != [0, 0, 1] {
                continue;
            }

            let stream_id = payload[3];
            stream.selected = match kind {
                PesKind::Audio => (0xc0..=0xdf).contains(&stream_id),
                PesKind::Video => (0xe0..=0xef).contains(&stream_id),
            };

            let header_len = 9 + payload[8] as usize;
            if stream.selected && header_len < payload.len() {
                stream.bytes.extend_from_slice(&payload[header_len..]);
            }
        } else if stream.selected {
            // May be continuation from the previous HLS segment.
            stream.bytes.extend_from_slice(&packet[p..]);
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_ts_data() {
        assert!(extract_aac(b"not a transport stream").is_err());
    }
}
