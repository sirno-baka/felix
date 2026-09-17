use getrandom::register_custom_getrandom;
use rustls::{ClientConfig, RootCertStore};
use serde::Deserialize;
use std::error::Error;
use std::net::ToSocketAddrs;
use std::time::Duration;

const BOT_TOKEN: &str = "7012180076:AAFf6y2LbRvygdwhDVWfKBxbmjNP1DSVTaQ";
const SYS_GETRANDOM: u32 = 355;

// -----------------------------------------------------------------------------
// PopugOS getrandom
// -----------------------------------------------------------------------------

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

register_custom_getrandom!(popugos_getrandom);

// -----------------------------------------------------------------------------
// TLS
// -----------------------------------------------------------------------------

fn tls_config() -> Result<ClientConfig, Box<dyn Error>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());

    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();

    config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(config)
}

// -----------------------------------------------------------------------------
// Telegram JSON structures
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: T,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    message_id: i64,

    document: Option<Document>,
    video: Option<MediaFile>,
    audio: Option<MediaFile>,
    voice: Option<MediaFile>,
    animation: Option<MediaFile>,

    // Telegram присылает несколько размеров фотографии.
    photo: Option<Vec<Photo>>,
}

#[derive(Debug, Deserialize)]
struct Document {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct MediaFile {
    file_id: String,

    #[serde(default)]
    file_name: Option<String>,

    #[serde(default)]
    mime_type: Option<String>,

    #[serde(default)]
    file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Photo {
    file_id: String,
    width: u32,
    height: u32,

    #[serde(default)]
    file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TelegramFile {
    file_id: String,
    file_path: Option<String>,
    file_size: Option<u64>,
}

// -----------------------------------------------------------------------------
// Наше представление пришедшего файла
// -----------------------------------------------------------------------------

struct IncomingFile {
    file_id: String,
    file_name: String,
}

fn get_file_from_message(message: &Message) -> Option<IncomingFile> {
    // Обычный отправленный файл/document
    if let Some(document) = &message.document {
        return Some(IncomingFile {
            file_id: document.file_id.clone(),
            file_name: document
                .file_name
                .clone()
                .unwrap_or_else(|| "document.bin".to_string()),
        });
    }

    if let Some(video) = &message.video {
        return Some(IncomingFile {
            file_id: video.file_id.clone(),
            file_name: video
                .file_name
                .clone()
                .unwrap_or_else(|| "video.mp4".to_string()),
        });
    }

    if let Some(audio) = &message.audio {
        return Some(IncomingFile {
            file_id: audio.file_id.clone(),
            file_name: audio
                .file_name
                .clone()
                .unwrap_or_else(|| "audio.bin".to_string()),
        });
    }

    if let Some(voice) = &message.voice {
        return Some(IncomingFile {
            file_id: voice.file_id.clone(),
            file_name: "voice.ogg".to_string(),
        });
    }

    if let Some(animation) = &message.animation {
        return Some(IncomingFile {
            file_id: animation.file_id.clone(),
            file_name: animation
                .file_name
                .clone()
                .unwrap_or_else(|| "animation.mp4".to_string()),
        });
    }

    // Берём последнюю фотографию — обычно это максимальный размер.
    if let Some(photos) = &message.photo {
        if let Some(photo) = photos.last() {
            return Some(IncomingFile {
                file_id: photo.file_id.clone(),
                file_name: format!("photo_{}.jpg", message.message_id),
            });
        }
    }

    None
}

// -----------------------------------------------------------------------------
// getFile
// -----------------------------------------------------------------------------

async fn telegram_get_file(
    client: &reqwest::Client,
    file_id: &str,
) -> Result<TelegramFile, Box<dyn Error>> {
    let url = format!(
        "https://api.telegram.org/bot{}/getFile?file_id={}",
        BOT_TOKEN,
        file_id
    );

    println!("telegram: getFile");

    let response = client.get(&url).send().await?;

    println!("telegram: getFile status={}", response.status());

    if !response.status().is_success() {
        let text = response.text().await?;
        return Err(format!("Telegram getFile failed: {text}").into());
    }

    let text = response.text().await?;

    let response: TelegramResponse<TelegramFile> =
        serde_json::from_str(&text)?;

    if !response.ok {
        return Err("Telegram returned ok=false".into());
    }

    Ok(response.result)
}

// -----------------------------------------------------------------------------
// Скачать содержимое файла
// -----------------------------------------------------------------------------

async fn download_file(
    client: &reqwest::Client,
    incoming: &IncomingFile,
) -> Result<(), Box<dyn Error>> {
    let tg_file = telegram_get_file(client, &incoming.file_id).await?;

    let file_path = tg_file
        .file_path
        .ok_or("Telegram did not return file_path")?;

    println!("telegram: file_path={}", file_path);

    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        BOT_TOKEN,
        file_path
    );

    println!("telegram: downloading {}", incoming.file_name);

    let response = client.get(&url).send().await?;

    println!("telegram: download status={}", response.status());

    if !response.status().is_success() {
        return Err(
            format!("file download failed: {}", response.status()).into()
        );
    }

    let bytes = response.bytes().await?;

    println!(
        "telegram: received {} bytes, saving as {}",
        bytes.len(),
        incoming.file_name
    );

    std::fs::write(&incoming.file_name, &bytes)?;

    println!("telegram: saved {}", incoming.file_name);

    Ok(())
}

// -----------------------------------------------------------------------------
// getUpdates
// -----------------------------------------------------------------------------

async fn get_updates(
    client: &reqwest::Client,
    offset: i64,
) -> Result<Vec<Update>, Box<dyn Error>> {
    let url = format!(
        "https://api.telegram.org/bot{}/getUpdates?timeout=30&offset={}",
        BOT_TOKEN,
        offset
    );

    println!("telegram: waiting for update... offset={offset}");

    let response = client.get(&url).send().await?;

    println!("telegram: getUpdates status={}", response.status());

    let text = response.text().await?;

    let response: TelegramResponse<Vec<Update>> =
        serde_json::from_str(&text)?;

    if !response.ok {
        return Err("Telegram getUpdates returned ok=false".into());
    }

    Ok(response.result)
}

// -----------------------------------------------------------------------------
// Main
// -----------------------------------------------------------------------------

async fn run() -> Result<(), Box<dyn Error>> {
    println!("telegram: resolving api.telegram.org");

    let resolved: Vec<_> = ("api.telegram.org", 443)
        .to_socket_addrs()?
        .collect();

    println!("telegram: DNS -> {resolved:?}");

    let telegram_addr = resolved
        .iter()
        .copied()
        .find(|addr| addr.is_ipv4())
        .ok_or("api.telegram.org: no IPv4 address")?;

    println!("telegram: using {telegram_addr}");

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls_config()?)
        .resolve("api.telegram.org", telegram_addr)
        .connect_timeout(Duration::from_secs(10))

        // getUpdates использует long polling до 30 секунд,
        // поэтому общий timeout должен быть больше.
        .timeout(Duration::from_secs(45))
        .build()?;

    println!("telegram: client ready");

    let mut offset: i64 = 0;

    loop {
        match get_updates(&client, offset).await {
            Ok(updates) => {
                for update in updates {
                    println!("telegram: update {}", update.update_id);

                    // ВАЖНО: после обработки больше этот update не получать.
                    offset = update.update_id + 1;

                    let Some(message) = &update.message else {
                        continue;
                    };

                    let Some(file) = get_file_from_message(message) else {
                        println!("telegram: update has no file");
                        continue;
                    };

                    println!(
                        "telegram: found file: {}",
                        file.file_name
                    );

                    match download_file(&client, &file).await {
                        Ok(()) => {
                            println!(
                                "telegram: download complete: {}",
                                file.file_name
                            );
                        }

                        Err(e) => {
                            println!(
                                "telegram: download error for {}: {:?}",
                                file.file_name,
                                e
                            );
                        }
                    }
                }
            }

            Err(e) => {
                // Не завершаем программу при временной ошибке сети.
                println!("telegram: getUpdates error: {:?}", e);

                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}


fn main() -> Result<(), Box<dyn Error>> {
    println!("telegram: creating Tokio runtime");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    runtime.block_on(run())
}