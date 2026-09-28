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
    last_cc: Option<u8>,
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


/// Estimate video frame rate from PES presentation timestamps in one MPEG-TS
/// segment. PTS uses a 90 kHz clock and is the authoritative playback timing,
/// unlike the master-playlist FRAME-RATE hint.
///
/// Twitch normally places one video access unit in each video PES packet. We
/// collect all video PES PTS values, sort/deduplicate them in presentation
/// order, and derive the average cadence across the segment.
pub fn video_fps_from_pts(segment: &[u8]) -> Option<f64> {
    let offset = (0..segment.len().min(188))
        .find(|&i| segment[i] == 0x47 && segment.get(i + 188).is_none_or(|b| *b == 0x47))?;

    let mut pts = Vec::new();
    let mut pos = offset;
    while pos + 188 <= segment.len() {
        let packet = &segment[pos..pos + 188];
        pos += 188;

        if packet[0] != 0x47 || packet[1] & 0x80 != 0 || packet[1] & 0x40 == 0 {
            continue;
        }

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

        let payload = &packet[p..];
        if payload.len() < 14 || payload[..3] != [0, 0, 1] {
            continue;
        }

        let stream_id = payload[3];
        if !(0xe0..=0xef).contains(&stream_id) {
            continue;
        }

        let pts_dts_flags = (payload[7] >> 6) & 0x03;
        if pts_dts_flags != 0b10 && pts_dts_flags != 0b11 {
            continue;
        }

        let b = &payload[9..14];
        let value =
            (((b[0] >> 1) as u64 & 0x07) << 30)
            | ((b[1] as u64) << 22)
            | (((b[2] >> 1) as u64 & 0x7f) << 15)
            | ((b[3] as u64) << 7)
            | ((b[4] >> 1) as u64 & 0x7f);
        pts.push(value);
    }

    if pts.len() < 3 {
        return None;
    }

    // PTS are presentation timestamps; with B-frames they may arrive out of
    // order in the transport stream. Sorting recovers presentation order for
    // cadence measurement. A HLS segment is far shorter than the 33-bit wrap.
    pts.sort_unstable();
    pts.dedup();
    if pts.len() < 3 {
        return None;
    }

    let first = *pts.first()?;
    let last = *pts.last()?;
    let span = last.checked_sub(first)?;
    if span == 0 {
        return None;
    }

    let fps = (pts.len().saturating_sub(1) as f64) * 90_000.0 / span as f64;
    if (10.0..=120.0).contains(&fps) {
        Some(fps)
    } else {
        None
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

        let cc = packet[3] & 0x0f;
        let discontinuity = control == 3
            && packet[4] > 0
            && packet.get(5).is_some_and(|flags| flags & 0x80 != 0);

        let mut p = 4usize;
        if control == 3 {
            p += 1 + packet[4] as usize;
        }
        if p >= 188 {
            continue;
        }

        let stream = streams.entry(pid).or_default();

        // MPEG-TS continuity counters continue across HLS media-segment
        // boundaries. A mismatch on an active video PES means packets or whole
        // HLS segments were skipped. Repeated CC is allowed for a retransmitted
        // duplicate packet; explicit discontinuity resets the expectation.
        if discontinuity {
            stream.last_cc = None;
        }
        if stream.selected {
            if let Some(last_cc) = stream.last_cc {
                let expected = last_cc.wrapping_add(1) & 0x0f;
                if cc != expected && cc != last_cc {
                    eprintln!(
                        "[ts] *** CONTINUITY GAP pid={:#x} expected={} got={} ***",
                        pid,
                        expected,
                        cc
                    );
                }
            }
        }
        stream.last_cc = Some(cc);

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
