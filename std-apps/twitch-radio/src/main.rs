#[cfg(target_os = "popugos")]
mod allocator;
mod hls;
mod mpegts;
mod time;
mod twitch;
mod video;

#[cfg(target_os = "popugos")]
use getrandom::register_custom_getrandom;
use reqwest::{Client, Url};
use rustls::{ClientConfig, RootCertStore};
use std::error::Error;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use video::VideoChunk;

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

fn tls_config(
    time_provider: Arc<dyn rustls::time_provider::TimeProvider>,
) -> Result<ClientConfig, Box<dyn Error>> {
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
    Ok(Client::builder()
        .use_preconfigured_tls(tls_config(resolved_time.provider)?)
        // Felix/smoltcp gives every TCP socket two 64 KiB buffers and polls
        // every live socket frequently. Reqwest otherwise keeps idle HTTP/1
        // connections for 90 seconds with no per-host pool limit, which is far
        // too expensive for this stack during frequent HLS playlist requests.
        .pool_max_idle_per_host(1)
        .pool_idle_timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
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

fn controls(stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            match line.trim() {
                "q" | "quit" | "stop" => {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                "" => {}
                _ => println!("commands: q"),
            }
        }
    });
}

// Keep enough compressed HLS video queued that short decoder/render stalls
// do not stall the downloader until segments fall out of Twitch's live window.
// The queue is bounded for memory safety, but send().await applies backpressure;
// it never drops a queued segment.
const SEGMENT_QUEUE_CAPACITY: usize = 16;
const SEGMENT_DOWNLOAD_CONCURRENCY: usize = 4;

struct DownloadedSegment {
    segment: hls::Segment,
    ts: Vec<u8>,
    download_ms: u128,
}

async fn download_segment(
    client: Client,
    media_url: Url,
    segment: hls::Segment,
) -> Result<DownloadedSegment, String> {
    let url = twitch::resolve_url(&media_url, &segment.uri).map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let response = response
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let ts = response
        .bytes()
        .await
        .map_err(|error| error.to_string())?
        .to_vec();

    Ok(DownloadedSegment {
        segment,
        ts,
        download_ms: started.elapsed().as_millis(),
    })
}

async fn segment_downloader(
    client: Client,
    media_url: Url,
    tx: mpsc::Sender<VideoChunk>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut last_sequence = None;
    let mut video_demuxer = mpegts::H264Demuxer::new();

    while !stop.load(Ordering::Relaxed) {
        let response = client
            .get(media_url.clone())
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let response = response
            .error_for_status()
            .map_err(|error| error.to_string())?;
        let bytes = response.bytes().await.map_err(|error| error.to_string())?;
        let playlist = String::from_utf8(bytes.to_vec()).map_err(|error| error.to_string())?;
        let segments = hls::media_segments(&playlist);
        if segments.is_empty() {
            return Err("empty media playlist".to_string());
        }
        let live_edge_sequence = segments.last().map(|segment| segment.sequence);

        // Do not start on the newest segment. At the exact live edge the
        // presenter inevitably reaches the end of each ~2 s segment before
        // Twitch has published the next one, which looks like a periodic
        // underrun even when downloads take only a few hundred milliseconds.
        //
        // Begin three already-published segments behind the edge. They can be
        // downloaded immediately and remain compressed in the upstream queue,
        // giving the decoder/presenter a stable reserve without consuming tens
        // of MiB of extra YUV memory.
        if last_sequence.is_none() {
            const STARTUP_SEGMENTS_BEHIND: usize = 6;
            let start = segments.len().saturating_sub(STARTUP_SEGMENTS_BEHIND);
            last_sequence = segments
                .get(start)
                .map(|segment| segment.sequence.saturating_sub(1));
            if let (Some(first), Some(last)) = (segments.get(start), segments.last()) {
                println!(
                    "[net] startup backlog seq={}..{} ({} segments)",
                    first.sequence,
                    last.sequence,
                    segments.len().saturating_sub(start)
                );
            }
        }

        let pending: Vec<_> = segments
            .into_iter()
            .filter(|segment| {
                !last_sequence.is_some_and(|last| segment.sequence <= last)
            })
            .collect();

        let mut downloaded_any = false;
        for batch in pending.chunks(SEGMENT_DOWNLOAD_CONCURRENCY) {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            // Start every segment in this batch concurrently. The previous code
            // used chunks(4) but only spawned pair[0] and pair[1], silently
            // skipping pair[2] and pair[3]. That created real holes in the
            // MPEG-TS/H.264 bitstream every batch.
            //
            // Await handles in the original playlist order because H264Demuxer
            // is stateful and PES/continuity state must see sequential segments.
            let mut tasks = Vec::with_capacity(batch.len());
            for segment in batch {
                tasks.push(tokio::spawn(download_segment(
                    client.clone(),
                    media_url.clone(),
                    segment.clone(),
                )));
            }

            for task in tasks {
                let downloaded = task
                    .await
                    .map_err(|error| format!("segment task: {error}"))??;
                let segment = downloaded.segment;

                if let Some(last) = last_sequence {
                    let expected = last.saturating_add(1);
                    if segment.sequence != expected {
                        eprintln!(
                            "[net] *** HLS SEQUENCE GAP expected={} got={} ***",
                            expected,
                            segment.sequence
                        );
                    }
                }
                let ts = downloaded.ts;
                let download_ms = downloaded.download_ms;
                let pts_fps = mpegts::video_fps_from_pts(&ts);
                let h264 = video_demuxer.push_segment(&ts)?;
                let queued = tx.max_capacity().saturating_sub(tx.capacity());
                let mbps = if download_ms > 0 {
                    (ts.len() as f64 * 8.0) / (download_ms as f64 * 1000.0)
                } else {
                    0.0
                };
                let (seg_ms, load_pct) = segment.duration
                    .filter(|duration| *duration > 0.0)
                    .map(|duration| {
                        let seg_ms = duration * 1000.0;
                        (seg_ms, download_ms as f64 * 100.0 / seg_ms)
                    })
                    .unwrap_or((0.0, 0.0));
                println!(
                    "[net] seq={} edge={} behind={} ts={:.1}KiB h264={:.1}KiB dl={}ms seg={:.0}ms load={:.0}% {:.2}Mbps pts_fps={:.2} segq={}/{} inflight={}",
                    segment.sequence,
                    live_edge_sequence.unwrap_or(segment.sequence),
                    live_edge_sequence
                        .unwrap_or(segment.sequence)
                        .saturating_sub(segment.sequence),
                    ts.len() as f64 / 1024.0,
                    h264.len() as f64 / 1024.0,
                    download_ms,
                    seg_ms,
                    load_pct,
                    mbps,
                    pts_fps.unwrap_or(0.0),
                    queued,
                    tx.max_capacity(),
                    batch.len()
                );

                if !h264.is_empty()
                    && tx
                        .send(VideoChunk {
                            sequence: segment.sequence,
                            duration: segment.duration,
                            fps: pts_fps,
                            h264,
                        })
                        .await
                        .is_err()
                {
                    return Ok(());
                }

                last_sequence = Some(segment.sequence);
                downloaded_any = true;
            }
        }

        if !downloaded_any {
            // Poll close enough to the live edge that playlist publication jitter
            // does not consume the decoded presentation buffer. Segment GETs are
            // still only issued once per sequence; this only refreshes the small
            // media playlist while waiting for Twitch to publish the next segment.
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    Ok(())
}

async fn play_once(
    client: &Client,
    channel: &str,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn Error>> {
    let master = twitch::master_playlist(client, channel).await?;
    let variant =
        hls::video_variant(&master, 480).ok_or("Twitch returned no playable H.264 video variant")?;
    let master_url = Url::parse(&format!(
        "https://usher.ttvnw.net/api/v2/channel/hls/{channel}.m3u8"
    ))?;
    let media_url = twitch::resolve_url(&master_url, &variant.uri)?;
    println!(
        "[start] twitch/{channel} {}x{} fps={:.2} bw={:?} codec={:?} segq={}",
        variant.width,
        variant.height,
        variant.frame_rate.unwrap_or(30.0),
        variant.bandwidth,
        variant.codecs,
        SEGMENT_QUEUE_CAPACITY
    );

    let fps_hint = variant.frame_rate.unwrap_or(30.0);
    let (segment_tx, segment_rx) = mpsc::channel::<VideoChunk>(SEGMENT_QUEUE_CAPACITY);
    let video_task = video::spawn(
        segment_rx,
        stop.clone(),
        variant.width,
        variant.height,
        fps_hint,
    );

    let download_result =
        segment_downloader(client.clone(), media_url, segment_tx, stop.clone()).await;

    let video_result = video_task
        .join()
        .map_err(|_| io::Error::other("video worker panicked"))?;
    if let Err(error) = video_result {
        return Err(io::Error::other(error).into());
    }
    if let Err(error) = download_result {
        return Err(io::Error::other(error).into());
    }

    Ok(())
}

async fn run(channel: String) -> Result<(), Box<dyn Error>> {
    let client = client()?;
    let stop = Arc::new(AtomicBool::new(false));
    controls(stop.clone());

    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = play_once(&client, &channel, stop.clone()).await {
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
    let channel = args.next().ok_or("usage: twitch-radio <channel>")?;
    if !channel
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("channel must contain only letters, digits, and underscores".into());
    }
    if let Some(arg) = args.next() {
        return Err(format!("unknown argument: {arg}").into());
    }

    tokio::runtime::Builder::new_multi_thread().worker_threads(2)
        .enable_io()
        .enable_time()
        .build()?
        .block_on(run(channel.to_ascii_lowercase()))
}
