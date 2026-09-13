use bytes::Bytes;
use getrandom::register_custom_getrandom;
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1;
use hyper::header::{CONNECTION, HOST};
use hyper::Request;
use hyper_util::rt::TokioIo;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

const HTTP_SERVER: &str = "10.0.2.2:18081";
const HTTPS_SERVER: &str = "1.1.1.1:443";
const HTTPS_HOST: &str = "one.one.one.one";

const SYS_GETRANDOM: u32 = 355;

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

async fn request_over_io<I>(io: I, host: &str, label: &str) -> Result<(), Box<dyn Error>>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    println!("{label}: starting HTTP/1 handshake");
    let (mut sender, connection) = http1::handshake(io).await?;
    println!("{label}: HTTP/1 handshake ready");

    tokio::spawn(async move {
        if let Err(error) = connection.await {
            println!("hyper: connection task error: {error:?}");
        }
    });

    let request = Request::builder()
        .method("GET")
        .uri("/")
        .header(HOST, host)
        .header(CONNECTION, "close")
        .body(Empty::<Bytes>::new())?;

    println!("{label}: sending GET /");
    let response = tokio::time::timeout(Duration::from_secs(15), sender.send_request(request))
        .await??;

    println!("{label}: status={}", response.status());

    let body = tokio::time::timeout(Duration::from_secs(15), response.into_body().collect())
        .await??
        .to_bytes();

    println!("{label}: body={} bytes", body.len());
    let preview_len = body.len().min(512);
    match std::str::from_utf8(&body[..preview_len]) {
        Ok(text) => println!("{label}: body preview:\n{text}"),
        Err(_) => println!("{label}: body preview is not UTF-8"),
    }

    println!("{label}: PASS");
    Ok(())
}

async fn run_http() -> Result<(), Box<dyn Error>> {
    println!("hyper: connecting to {HTTP_SERVER}");

    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(HTTP_SERVER),
    )
    .await??;

    println!(
        "hyper: tcp connected local={} peer={}",
        stream.local_addr()?,
        stream.peer_addr()?
    );

    request_over_io(TokioIo::new(stream), "10.0.2.2:18081", "hyper").await
}

fn tls_config() -> Result<Arc<ClientConfig>, Box<dyn Error>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // Pure-Rust provider: unlike ring/aws-lc-rs it does not depend on a
    // platform-specific C/assembly backend, which is important for PopugOS.
    let provider = Arc::new(rustls_rustcrypto::provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

async fn run_https() -> Result<(), Box<dyn Error>> {
    println!("https: connecting TCP to {HTTPS_SERVER}");
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(HTTPS_SERVER),
    )
    .await??;

    println!(
        "https: tcp connected local={} peer={}",
        stream.local_addr()?,
        stream.peer_addr()?
    );

    println!("https: creating rustls config (RustCrypto provider)");
    let connector = TlsConnector::from(tls_config()?);
    let server_name = ServerName::try_from(HTTPS_HOST)?.to_owned();

    println!("https: TLS handshake SNI={HTTPS_HOST}");
    let tls = tokio::time::timeout(
        Duration::from_secs(15),
        connector.connect(server_name, stream),
    )
    .await??;
    println!("https: TLS handshake complete");

    request_over_io(TokioIo::new(tls), HTTPS_HOST, "https").await
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("hyper: creating Tokio runtime");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    println!("hyper: runtime ready");

    if std::env::args().nth(1).as_deref() == Some("https") {
        runtime.block_on(run_https())
    } else {
        runtime.block_on(run_http())
    }
}
