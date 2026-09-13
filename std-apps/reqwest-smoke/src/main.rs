use getrandom::register_custom_getrandom;
use rustls::{ClientConfig, RootCertStore};
use std::error::Error;
use std::net::ToSocketAddrs;
use std::time::Duration;

const URL: &str = "https://one.one.one.one/";
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

fn tls_config() -> Result<ClientConfig, Box<dyn Error>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();

    // Первый Reqwest-тест оставляем HTTP/1-only, чтобы менять только верхний HTTP-клиент.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

async fn run() -> Result<(), Box<dyn Error>> {
    println!("reqwest: resolving one.one.one.one via std::net::ToSocketAddrs");
    let resolved: Vec<_> = ("one.one.one.one", 443).to_socket_addrs()?.collect();
    println!("reqwest: DNS -> {resolved:?}");

    println!("reqwest: creating client (no DNS override)");
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls_config()?)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()?;

    println!("reqwest: GET {URL}");
    let response = client.get(URL).send().await?;

    println!("reqwest: status={}", response.status());
    println!("reqwest: version={:?}", response.version());

    let body = response.bytes().await?;
    println!("reqwest: body={} bytes", body.len());

    let preview_len = body.len().min(512);
    match std::str::from_utf8(&body[..preview_len]) {
        Ok(text) => println!("reqwest: body preview:\n{text}"),
        Err(_) => println!("reqwest: body preview is not UTF-8"),
    }

    println!("reqwest: PASS");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("reqwest: creating Tokio runtime");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    println!("reqwest: runtime ready");
    runtime.block_on(run())
}
