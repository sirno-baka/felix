use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub sequence: u64,
    pub uri: String,
}

pub fn audio_variant(master: &str) -> Option<String> {
    let mut pending = None;
    let mut fallback = None;

    for raw in master.lines() {
        let line = raw.trim();
        if line.starts_with("#EXT-X-STREAM-INF:") {
            let attrs = parse_attributes(line.trim_start_matches("#EXT-X-STREAM-INF:"));
            let audio_only = attrs.values().any(|value| {
                let value = value.to_ascii_lowercase();
                value == "audio_only" || value == "audio only"
            });
            let score = if audio_only {
                2
            } else {
                1
            };
            pending = Some(score);
        } else if !line.is_empty() && !line.starts_with('#') {
            match pending.take() {
                Some(2) => return Some(line.to_string()),
                Some(1) if fallback.is_none() => fallback = Some(line.to_string()),
                _ => {}
            }
        }
    }
    fallback
}

pub fn media_segments(playlist: &str) -> Vec<Segment> {
    let mut sequence = playlist
        .lines()
        .find_map(|line| line.trim().strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let mut result = Vec::new();

    for raw in playlist.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        result.push(Segment { sequence, uri: line.to_string() });
        sequence = sequence.saturating_add(1);
    }
    result
}

pub fn parse_attributes(line: &str) -> BTreeMap<String, String> {
    let mut attrs = BTreeMap::new();
    let mut start = 0;
    let mut quoted = false;
    let bytes = line.as_bytes();

    for i in 0..=bytes.len() {
        if i < bytes.len() && bytes[i] == b'"' {
            quoted = !quoted;
        }
        if i == bytes.len() || (bytes[i] == b',' && !quoted) {
            if let Some((key, value)) = line[start..i].split_once('=') {
                attrs.insert(key.trim().to_string(), value.trim().trim_matches('"').to_string());
            }
            start = i + 1;
        }
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_audio_only_variant() {
        let input = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=3000000,VIDEO=\"chunked\"\nhttps://x/source.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=160000,VIDEO=\"audio_only\"\nhttps://x/audio.m3u8\n";
        assert_eq!(audio_variant(input).as_deref(), Some("https://x/audio.m3u8"));
    }

    #[test]
    fn assigns_media_sequences() {
        let input = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:41\n#EXTINF:2.0,\na.ts\n#EXTINF:2.0,\nb.ts\n";
        assert_eq!(media_segments(input), vec![
            Segment { sequence: 41, uri: "a.ts".into() },
            Segment { sequence: 42, uri: "b.ts".into() },
        ]);
    }

    #[test]
    fn parses_quoted_attributes() {
        let attrs = parse_attributes("TYPE=VIDEO,NAME=\"Audio Only\",CODECS=\"mp4a.40.2\"");
        assert_eq!(attrs["NAME"], "Audio Only");
        assert_eq!(attrs["CODECS"], "mp4a.40.2");
    }
}
