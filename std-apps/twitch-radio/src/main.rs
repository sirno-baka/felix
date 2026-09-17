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
use std::error::Error;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

async fn play_once(
    client: &Client,
    channel: &str,
    volume: Arc<AtomicU16>,
    stop: &AtomicBool,
) -> Result<(), Box<dyn Error>> {
    println!("connecting to twitch.tv/{channel} ...");
    let master = twitch::master_playlist(client, channel).await?;
    let media_uri = hls::audio_variant(&master).ok_or("Twitch returned no playable HLS variant")?;
    let master_url = Url::parse(&format!("https://usher.ttvnw.net/api/channel/hls/{channel}.m3u8"))?;
    let media_url = twitch::resolve_url(&master_url, &media_uri)?;
    println!("playing audio; commands: +  -  q");

    let mut player = AudioPlayer::open(volume)?;
    let mut last_sequence = None;
    while !stop.load(Ordering::Relaxed) {
        let playlist = client.get(media_url.clone()).send().await?.error_for_status()?.text().await?;
        let segments = hls::media_segments(&playlist);
        if segments.is_empty() {
            return Err("empty media playlist".into());
        }
        if last_sequence.is_none() {
            last_sequence = segments.last().map(|segment| segment.sequence.saturating_sub(1));
        }
        let mut played_any = false;
        for segment in segments {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if last_sequence.is_some_and(|last| segment.sequence <= last) {
                continue;
            }
            let url = twitch::resolve_url(&media_url, &segment.uri)?;
            let bytes = client.get(url).send().await?.error_for_status()?.bytes().await?;
            let aac = mpegts::extract_aac(&bytes)?;
            let frames = player.decode_adts(&aac)?;
            if frames == 0 {
                return Err("segment contained no complete ADTS frames".into());
            }
            last_sequence = Some(segment.sequence);
            played_any = true;
        }
        if !played_any {
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
    }
    Ok(())
}

async fn run(channel: String) -> Result<(), Box<dyn Error>> {
    let client = client()?;
    let volume = Arc::new(AtomicU16::new(80));
    let stop = Arc::new(AtomicBool::new(false));
    controls(volume.clone(), stop.clone());

    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = play_once(&client, &channel, volume.clone(), &stop).await {
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
    let channel = std::env::args().nth(1).ok_or("usage: twitch-radio <channel>")?;
    if !channel.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return Err("channel must contain only letters, digits, and underscores".into());
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?
        .block_on(run(channel.to_ascii_lowercase()))
}
