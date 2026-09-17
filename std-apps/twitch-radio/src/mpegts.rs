use std::collections::BTreeMap;

#[derive(Default)]
struct PesStream {
    audio: bool,
    bytes: Vec<u8>,
}

fn flush(stream: &mut PesStream, output: &mut Vec<u8>) {
    if stream.audio {
        output.append(&mut stream.bytes);
    } else {
        stream.bytes.clear();
    }
    stream.audio = false;
}

/// Extracts AAC elementary-stream bytes from MPEG-TS PES packets.
/// Twitch audio-only HLS currently carries ADTS AAC in 188-byte TS packets.
pub fn extract_aac(segment: &[u8]) -> Result<Vec<u8>, String> {
    let offset = (0..segment.len().min(188))
        .find(|&i| segment[i] == 0x47 && segment.get(i + 188).is_none_or(|b| *b == 0x47))
        .ok_or_else(|| "MPEG-TS sync byte not found".to_string())?;
    let mut streams: BTreeMap<u16, PesStream> = BTreeMap::new();
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
            flush(stream, &mut output);
            let payload = &packet[p..];
            if payload.len() < 9 || payload[..3] != [0, 0, 1] {
                continue;
            }
            let stream_id = payload[3];
            stream.audio = (0xc0..=0xdf).contains(&stream_id);
            let header_len = 9 + payload[8] as usize;
            if stream.audio && header_len < payload.len() {
                stream.bytes.extend_from_slice(&payload[header_len..]);
            }
        } else if stream.audio {
            stream.bytes.extend_from_slice(&packet[p..]);
        }
    }
    for stream in streams.values_mut() {
        flush(stream, &mut output);
    }
    if output.is_empty() {
        Err("no AAC audio PES packets in segment".to_string())
    } else {
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_ts_data() {
        assert!(extract_aac(b"not a transport stream").is_err());
    }
}
