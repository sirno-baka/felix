#[cfg(target_os = "popugos")]
mod allocator;
mod audio;
mod hls;
mod math;
mod mpegts;
mod time;
mod twitch;

use audio::AudioPlayer;
#[cfg(target_os = "popugos")]
use getrandom::register_custom_getrandom;
use reqwest::{Client, Url};
use rustls::{ClientConfig, RootCertStore};
use std::collections::VecDeque;
use std::error::Error;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;

#[cfg(target_os = "popugos")]
const SYS_GETRANDOM: u32 = 355;

#[cfg(target_os = "popugos")]
fn popugos_getrandom(dest: &mut [u8]) -> Result<(), getrandom::Error> {
    let mut offset = 0usize;
    while offset < dest.len() {
        let ret: i32;
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("eax") SYS_GETRANDOM => ret,
                in("ebx") dest.as_mut_ptr().add(offset),
                in("ecx") dest.len() - offset,
                in("edx") 0u32,
                options(nostack, preserves_flags)
            );
        }
        if ret <= 0 {
            return Err(getrandom::Error::UNSUPPORTED);
        }
        offset += ret as usize;
    }
    Ok(())
}

#[cfg(target_os = "popugos")]
register_custom_getrandom!(popugos_getrandom);

fn tls_config(time_provider: Arc<dyn rustls::time_provider::TimeProvider>) -> Result<ClientConfig, Box<dyn Error>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    let mut config = ClientConfig::builder_with_details(provider, time_provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    // The signed Twitch media-playlist URL makes a much larger HTTP request
    // than the GQL and usher requests. Small TLS records avoid relying on IP
    // fragmentation and work around short-frame/DMA issues on legacy NICs.
    config.max_fragment_size = Some(512);
    Ok(config)
}

fn client() -> Result<Client, Box<dyn Error>> {
    let resolved_time = time::resolve();
    println!("TLS time: {}", resolved_time.description);
    Ok(Client::builder()
        .use_preconfigured_tls(tls_config(resolved_time.provider)?)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(90))
        .build()?)
}

fn print_error(error: &(dyn Error + 'static)) {
    eprintln!("stream error: {error}");
    let mut source = error.source();
    let mut level = 1;
    while let Some(cause) = source {
        eprintln!("  cause {level}: {cause}");
        source = cause.source();
        level += 1;
    }
}

fn controls(volume: Arc<AtomicU16>, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            match line.trim() {
                "+" => {
                    let old = volume.load(Ordering::Relaxed);
                    let new = old.saturating_add(10).min(100);
                    volume.store(new, Ordering::Relaxed);
                    println!("volume: {new}%");
                }
                "-" => {
                    let new = volume.load(Ordering::Relaxed).saturating_sub(10);
                    volume.store(new, Ordering::Relaxed);
                    println!("volume: {new}%");
                }
                "q" | "quit" | "stop" => {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                "" => {}
                _ => println!("commands: +  -  q"),
            }
        }
    });
}

const SEGMENT_QUEUE_CAPACITY: usize = 4;
const PREBUFFER_SEGMENTS: usize = 2;

struct DownloadedSegment {
    sequence: u64,
    aac: Vec<u8>,
    download_ms: u128,
    chunks: u64,
}

enum DownloadMessage {
    Segment(DownloadedSegment),
    Error(String),
}

async fn segment_downloader(
    client: Client,
    media_url: Url,
    tx: mpsc::Sender<DownloadMessage>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut last_sequence = None;
    let mut playlist_polls = 0u64;

    while !stop.load(Ordering::Relaxed) {
        playlist_polls = playlist_polls.saturating_add(1);
        println!(
            "[download] media playlist poll={} GET host={} path={}",
            playlist_polls,
            media_url.host_str().unwrap_or("?"),
            media_url.path()
        );
        let request_started = std::time::Instant::now();
        let response = client
            .get(media_url.clone())
            .send()
            .await
            .map_err(|error| error.to_string())?;
        println!(
            "[download] media headers poll={} status={} elapsed={}ms",
            playlist_polls,
            response.status(),
            request_started.elapsed().as_millis()
        );
        let response = response.error_for_status().map_err(|error| error.to_string())?;
        let bytes = response.bytes().await.map_err(|error| error.to_string())?;
        let playlist = String::from_utf8(bytes.to_vec()).map_err(|error| error.to_string())?;
        let segments = hls::media_segments(&playlist);
        if segments.is_empty() {
            return Err("empty media playlist".to_string());
        }
        println!(
            "[download] parsed {} segment(s), sequence {}..{}",
            segments.len(),
            segments.first().map(|segment| segment.sequence).unwrap_or(0),
            segments.last().map(|segment| segment.sequence).unwrap_or(0)
        );

        // Start at the live edge. The first playlist contributes only its
        // newest complete segment; from then on every unseen sequence is
        // downloaded in order and buffered for the playback task.
        if last_sequence.is_none() {
            last_sequence = segments.last().map(|segment| segment.sequence.saturating_sub(1));
        }

        let mut downloaded_any = false;
        for segment in segments {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if last_sequence.is_some_and(|last| segment.sequence <= last) {
                continue;
            }

            let url = twitch::resolve_url(&media_url, &segment.uri).map_err(|error| error.to_string())?;
            println!(
                "[download] segment seq={} GET host={} path={}",
                segment.sequence,
                url.host_str().unwrap_or("?"),
                url.path()
            );
            let segment_started = std::time::Instant::now();
            let response = client
                .get(url)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            println!(
                "[download] segment seq={} headers status={} content_length={:?} elapsed={}ms",
                segment.sequence,
                response.status(),
                response.content_length(),
                segment_started.elapsed().as_millis()
            );
            let mut response = response.error_for_status().map_err(|error| error.to_string())?;
            let mut ts = Vec::new();
            let mut chunk_index = 0u64;
            let mut last_chunk_at = segment_started;
            while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
                let now = std::time::Instant::now();
                chunk_index = chunk_index.saturating_add(1);
                ts.extend_from_slice(&chunk);
                println!(
                    "[download] segment seq={} chunk={} +{} total={} delta={}ms elapsed={}ms",
                    segment.sequence,
                    chunk_index,
                    chunk.len(),
                    ts.len(),
                    now.duration_since(last_chunk_at).as_millis(),
                    now.duration_since(segment_started).as_millis()
                );
                last_chunk_at = now;
            }

            let download_ms = segment_started.elapsed().as_millis();
            println!(
                "[download] segment seq={} complete bytes={} chunks={} elapsed={}ms; MPEG-TS -> AAC",
                segment.sequence,
                ts.len(),
                chunk_index,
                download_ms
            );
            let aac = mpegts::extract_aac(&ts)?;
            println!(
                "[download] segment seq={} AAC bytes={} queued for playback",
                segment.sequence,
                aac.len()
            );

            if tx
                .send(DownloadMessage::Segment(DownloadedSegment {
                    sequence: segment.sequence,
                    aac,
                    download_ms,
                    chunks: chunk_index,
                }))
                .await
                .is_err()
            {
                return Ok(());
            }

            last_sequence = Some(segment.sequence);
            downloaded_any = true;
        }

        if !downloaded_any {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    Ok(())
}

async fn play_once(
    client: &Client,
    channel: &str,
    volume: Arc<AtomicU16>,
    stop: Arc<AtomicBool>,
    output_path: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    println!("connecting to twitch.tv/{channel} ...");
    println!("[stage 1] requesting Twitch playback token + master playlist");
    let master = twitch::master_playlist(client, channel).await?;
    println!("[stage 1] master playlist ready bytes={}", master.len());
    println!("[stage 2] selecting audio-only HLS variant");
    let media_uri = hls::audio_variant(&master).ok_or("Twitch returned no playable HLS variant")?;
    let master_url = Url::parse(&format!("https://usher.ttvnw.net/api/v2/channel/hls/{channel}.m3u8"))?;
    let media_url = twitch::resolve_url(&master_url, &media_uri)?;
    println!(
        "[stage 2] media playlist selected host={} path={}",
        media_url.host_str().unwrap_or("?"),
        media_url.path()
    );

    let (segment_tx, mut segment_rx) = mpsc::channel::<DownloadMessage>(SEGMENT_QUEUE_CAPACITY);
    let error_tx = segment_tx.clone();
    let downloader_client = client.clone();
    let downloader_url = media_url.clone();
    let downloader_stop = stop.clone();
    let download_task = tokio::spawn(async move {
        if let Err(error) = segment_downloader(
            downloader_client,
            downloader_url,
            segment_tx,
            downloader_stop,
        )
        .await
        {
            let _ = error_tx.send(DownloadMessage::Error(error)).await;
        }
    });

    if let Some(path) = output_path {
        println!("[stage 3] opening PCM output file {path}");
    } else {
        println!("[stage 3] opening /dev/audio");
    }
    let mut player = AudioPlayer::open(volume.clone(), output_path)?;
    if let Some(path) = output_path {
        println!("[stage 3] PCM output file opened: {path} (s16le stereo 48000 Hz)");
    } else {
        println!("[stage 3] /dev/audio opened O_NONBLOCK; async POLLOUT enabled");
    }
    println!(
        "buffering {} segment(s); downloader and playback run concurrently",
        PREBUFFER_SEGMENTS
    );

    let playback_result: Result<(), Box<dyn Error>> = async {
        let mut prebuffer = VecDeque::with_capacity(PREBUFFER_SEGMENTS);
        while prebuffer.len() < PREBUFFER_SEGMENTS && !stop.load(Ordering::Relaxed) {
            match segment_rx.recv().await {
                Some(DownloadMessage::Segment(segment)) => {
                    println!(
                        "[buffer] seq={} ready ({}/{}) download={}ms chunks={}",
                        segment.sequence,
                        prebuffer.len() + 1,
                        PREBUFFER_SEGMENTS,
                        segment.download_ms,
                        segment.chunks
                    );
                    prebuffer.push_back(segment);
                }
                Some(DownloadMessage::Error(error)) => {
                    return Err(io::Error::new(io::ErrorKind::Other, error).into());
                }
                None => return Err("segment downloader stopped".into()),
            }
        }

        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        println!("playing audio; commands: +  -  q");
        let started_at = tokio::time::Instant::now();
        let mut last_heartbeat = started_at;
        let mut played_segments = 0u64;
        let mut decoded_frames = 0u64;

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            let segment = if let Some(segment) = prebuffer.pop_front() {
                segment
            } else {
                match segment_rx.recv().await {
                    Some(DownloadMessage::Segment(segment)) => segment,
                    Some(DownloadMessage::Error(error)) => {
                        return Err(io::Error::new(io::ErrorKind::Other, error).into());
                    }
                    None => return Err("segment downloader stopped".into()),
                }
            };

            println!(
                "[play] segment seq={} AAC bytes={} download={}ms chunks={} decode/write start",
                segment.sequence,
                segment.aac.len(),
                segment.download_ms,
                segment.chunks
            );
            let decode_started = std::time::Instant::now();
            let frames = player.decode_adts(&segment.aac).await?;
            println!(
                "[play] segment seq={} complete frames={} elapsed={}ms",
                segment.sequence,
                frames,
                decode_started.elapsed().as_millis()
            );
            if frames == 0 {
                return Err("segment contained no complete ADTS frames".into());
            }

            played_segments = played_segments.saturating_add(1);
            decoded_frames = decoded_frames.saturating_add(frames as u64);
            let now = tokio::time::Instant::now();
            if played_segments == 1 || now.duration_since(last_heartbeat) >= Duration::from_secs(5) {
                let audio_seconds = decoded_frames.saturating_mul(1024) / 48_000;
                println!(
                    "alive: playing audio channel={channel} uptime={}s audio={}s segments={} frames={} volume={}% sequence={}",
                    now.duration_since(started_at).as_secs(),
                    audio_seconds,
                    played_segments,
                    decoded_frames,
                    volume.load(Ordering::Relaxed),
                    segment.sequence,
                );
                last_heartbeat = now;
            }
        }
        Ok(())
    }
    .await;

    download_task.abort();
    playback_result
}

async fn run(channel: String, output_path: Option<String>) -> Result<(), Box<dyn Error>> {
    let client = client()?;
    let volume = Arc::new(AtomicU16::new(80));
    let stop = Arc::new(AtomicBool::new(false));
    controls(volume.clone(), stop.clone());

    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = play_once(
            &client,
            &channel,
            volume.clone(),
            stop.clone(),
            output_path.as_deref(),
        )
        .await
        {
            print_error(error.as_ref());
            if !stop.load(Ordering::Relaxed) {
                println!("retrying in 3 seconds ...");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    println!("stopped");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let channel = args
        .next()
        .ok_or("usage: twitch-radio <channel> [--output <pcm-file>]")?;
    if !channel.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return Err("channel must contain only letters, digits, and underscores".into());
    }

    let mut output_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" | "-o" => {
                if output_path.is_some() {
                    return Err("--output specified more than once".into());
                }
                output_path = Some(args.next().ok_or("--output requires a file path")?);
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }

    if let Some(path) = output_path.as_deref() {
        println!("audio output: raw PCM file {path} (s16le stereo 48000 Hz)");
    } else {
        println!("audio output: /dev/audio");
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?
        .block_on(run(channel.to_ascii_lowercase(), output_path))
}
