use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub sequence: u64,
    pub uri: String,
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoVariant {
    pub uri: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<f64>,
    pub bandwidth: Option<u64>,
    pub codecs: Option<String>,
    pub name: Option<String>,
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

pub fn video_variant(master: &str, preferred_height: u32) -> Option<VideoVariant> {
    let mut pending: Option<BTreeMap<String, String>> = None;
    let mut candidates = Vec::new();

    for raw in master.lines() {
        let line = raw.trim();
        if line.starts_with("#EXT-X-STREAM-INF:") {
            pending = Some(parse_attributes(line.trim_start_matches("#EXT-X-STREAM-INF:")));
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some(attrs) = pending.take() else { continue };
        let audio_only = attrs.values().any(|value| {
            let value = value.to_ascii_lowercase();
            value == "audio_only" || value == "audio only"
        });
        if audio_only {
            continue;
        }
        if attrs
            .get("CODECS")
            .is_some_and(|codecs| !codecs.to_ascii_lowercase().contains("avc1"))
        {
            continue;
        }

        let dimensions = attrs
            .get("RESOLUTION")
            .and_then(|value| value.split_once('x'))
            .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
            .or_else(|| {
                attrs
                    .get("VIDEO")
                    .or_else(|| attrs.get("NAME"))
                    .and_then(|value| {
                        height_from_label(value).map(|height| {
                            let width = ((height.saturating_mul(16) / 9) + 1) & !1;
                            (width, height)
                        })
                    })
            });
        let Some((width, height)) = dimensions else { continue };

        candidates.push(VideoVariant {
            uri: line.to_string(),
            width,
            height,
            frame_rate: attrs.get("FRAME-RATE").and_then(|value| value.parse().ok()),
            bandwidth: attrs.get("BANDWIDTH").and_then(|value| value.parse().ok()),
            codecs: attrs.get("CODECS").cloned(),
            name: attrs.get("VIDEO").cloned().or_else(|| attrs.get("NAME").cloned()),
        });
    }

    candidates.into_iter().min_by(|a, b| {
        let score = |variant: &VideoVariant| {
            let height_penalty = if variant.height == preferred_height {
                0u64
            } else if variant.height < preferred_height {
                (preferred_height - variant.height) as u64 + 1_000
            } else {
                (variant.height - preferred_height) as u64 + 10_000
            };
            let fps_penalty = variant
                .frame_rate
                .map(|fps| ((fps - 30.0).abs() * 10.0) as u64)
                .unwrap_or(500);
            (height_penalty, fps_penalty, variant.bandwidth.unwrap_or(u64::MAX))
        };
        score(a).cmp(&score(b))
    })
}

fn height_from_label(value: &str) -> Option<u32> {
    let lower = value.to_ascii_lowercase();
    let p = lower.find('p')?;
    let digits = lower[..p]
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    digits.parse().ok()
}

pub fn media_segments(playlist: &str) -> Vec<Segment> {
    let mut sequence = playlist
        .lines()
        .find_map(|line| line.trim().strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let mut result = Vec::new();
    let mut pending_duration = None;

    for raw in playlist.lines() {
        let line = raw.trim();
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            pending_duration = value
                .split(',')
                .next()
                .and_then(|value| value.trim().parse::<f64>().ok());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        result.push(Segment {
            sequence,
            uri: line.to_string(),
            duration: pending_duration.take(),
        });
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
    fn selects_480p_30_video_variant() {
        let input = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,FRAME-RATE=60.000,VIDEO=\"720p60\",CODECS=\"avc1.4D401F,mp4a.40.2\"\nhttps://x/720.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=1400000,RESOLUTION=852x480,FRAME-RATE=30.000,VIDEO=\"480p30\",CODECS=\"avc1.4D401F,mp4a.40.2\"\nhttps://x/480.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=160000,VIDEO=\"audio_only\",CODECS=\"mp4a.40.2\"\nhttps://x/audio.m3u8\n";
        let variant = video_variant(input, 480).unwrap();
        assert_eq!(variant.uri, "https://x/480.m3u8");
        assert_eq!((variant.width, variant.height), (852, 480));
        assert_eq!(variant.frame_rate, Some(30.0));
    }

    #[test]
    fn assigns_media_sequences() {
        let input = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:41\n#EXTINF:2.0,\na.ts\n#EXTINF:2.0,\nb.ts\n";
        assert_eq!(media_segments(input), vec![
            Segment { sequence: 41, uri: "a.ts".into(), duration: Some(2.0) },
            Segment { sequence: 42, uri: "b.ts".into(), duration: Some(2.0) },
        ]);
    }

    #[test]
    fn parses_quoted_attributes() {
        let attrs = parse_attributes("TYPE=VIDEO,NAME=\"Audio Only\",CODECS=\"mp4a.40.2\"");
        assert_eq!(attrs["NAME"], "Audio Only");
        assert_eq!(attrs["CODECS"], "mp4a.40.2");
    }
}
