//! HTTP/HTTPS client for Felix OS
//!
//! # Example
//! ```no_run
//! use libfelix::net::http_client::{fetch, HttpMethod};
//!
//! let response = fetch("https://example.com/").await?;
//! println!("Status: {}", response.status);
//! ```

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::str::{from_utf8, FromStr};
use embedded_io_async::{Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use rand_core::{CryptoRng, RngCore, Error};


use super::edge_adapter::FelixStack;
use edge_nal::TcpConnect;
use crate::net::dns;
use crate::net::dns::DnsError;
use crate::println;

/// HTTP methods
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Head,
}

impl HttpMethod {
    fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
        }
    }
}

/// HTTP request configuration
pub struct HttpRequest<'a> {
    pub url: &'a str,
    pub method: HttpMethod,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
}

impl<'a> HttpRequest<'a> {
    pub fn get(url: &'a str) -> Self {
        Self {
            url,
            method: HttpMethod::Get,
            headers: &[],
            body: None,
        }
    }

    pub fn post(url: &'a str, body: &'a [u8]) -> Self {
        Self {
            url,
            method: HttpMethod::Post,
            headers: &[("Content-Type", "application/x-www-form-urlencoded")],
            body: Some(body),
        }
    }
}

/// HTTP response
pub struct HttpResponse {
    pub status: u16,
    pub reason: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// HTTP client errors
#[derive(Debug, Clone, Copy)]
pub enum HttpError {
    InvalidUrl,
    DnsError,
    TcpConnectFailed,
    TlsHandshakeFailed,
    WriteFailed,
    ReadFailed,
    ParseFailed,
}

/// Simple PRNG for TLS (xorshift64)
struct SimpleRng(u64);

impl RngCore for SimpleRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            let len = chunk.len().min(8);
            chunk[..len].copy_from_slice(&bytes[..len]);
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for SimpleRng {}

/// Parsed URL components
struct ParsedUrl<'a> {
    scheme: &'a str,
    host: &'a str,
    port: u16,
    path: &'a str,
}

fn parse_url(url: &str) -> Option<ParsedUrl<'_>> {
    let (scheme, rest) = if url.starts_with("https://") {
        ("https", &url[8..])
    } else if url.starts_with("http://") {
        ("http", &url[7..])
    } else {
        ("http", url)
    };

    let path_start = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..path_start];
    let path = if path_start < rest.len() {
        &rest[path_start..]
    } else {
        "/"
    };

    let (host, port) = if let Some(colon) = authority.rfind(':') {
        let h = &authority[..colon];
        let p: u16 = authority[colon + 1..].parse().ok()?;
        (h, p)
    } else {
        (authority, if scheme == "https" { 443 } else { 80 })
    };

    Some(ParsedUrl {
        scheme,
        host,
        port,
        path,
    })
}

/// Fetch a URL with GET request
pub async fn fetch(url: &str) -> Result<HttpResponse, HttpError> {
    request(HttpRequest::get(url)).await
}

/// Send an HTTP request
pub async fn request(req: HttpRequest<'_>) -> Result<HttpResponse, HttpError> {
    let parsed = parse_url(req.url).ok_or(HttpError::InvalidUrl)?;

    // Resolve host to IP
    let ip = match core::net::Ipv4Addr::from_str(parsed.host) {
        Ok(ip) => ip,
        Err(_) => {
            let octets = crate::net::dns::resolve(parsed.host).map_err(|_| HttpError::DnsError)?;
            core::net::Ipv4Addr::from_octets(octets)
        }
    };

    let addr = core::net::SocketAddr::V4(core::net::SocketAddrV4::new(ip, parsed.port));

    // TCP connect
    let stack = FelixStack;
    println!("[http] tcp connect {}:{}", ip, parsed.port);
    let mut tcp_stream = stack.connect(addr).await.map_err(|_| HttpError::TcpConnectFailed)?;
    println!("[http] tcp connected");

    if parsed.scheme == "https" {
        let mut read_buf = [0u8; 16640];
        let mut write_buf = [0u8; 16640];
        let mut rng = SimpleRng(0xDEAD_BEEF);

        let config = TlsConfig::new()
            .with_server_name(parsed.host)
            .enable_rsa_signatures();

        let mut tls = TlsConnection::new(tcp_stream, &mut read_buf, &mut write_buf);

        println!("[http] tls handshake start (host={})", parsed.host);
        if let Err(e) = tls
            .open(TlsContext::new(
                &config,
                UnsecureProvider::new::<Aes128GcmSha256>(&mut rng),
            ))
            .await
        {
            println!("[http] tls handshake failed: {:?}", e);
            return Err(HttpError::TlsHandshakeFailed);
        }
        println!("[http] tls handshake ok");

        do_http_request(&mut tls, parsed.host, parsed.path, &req).await
    } else {
        do_http_request(&mut tcp_stream, parsed.host, parsed.path, &req).await
    }
}

async fn do_http_request<S: Read + Write>(
    stream: &mut S,
    host: &str,
    path: &str,
    req: &HttpRequest<'_>,
) -> Result<HttpResponse, HttpError> {
    // Build HTTP request
    let mut request = Vec::new();
    request.extend_from_slice(req.method.as_str().as_bytes());
    request.extend_from_slice(b" ");
    request.extend_from_slice(path.as_bytes());
    request.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(b"\r\n");

    // Add custom headers
    for (key, value) in req.headers {
        request.extend_from_slice(key.as_bytes());
        request.extend_from_slice(b": ");
        request.extend_from_slice(value.as_bytes());
        request.extend_from_slice(b"\r\n");
    }

    // Add Content-Length if body exists
    if let Some(body) = req.body {
        request.extend_from_slice(b"Content-Length: ");
        let len_str = body.len().to_string();
        request.extend_from_slice(len_str.as_bytes());
        request.extend_from_slice(b"\r\n");
    }

    request.extend_from_slice(b"Connection: close\r\n\r\n");

    // Add body if present
    if let Some(body) = req.body {
        request.extend_from_slice(body);
    }

    // Send request
    stream.write_all(&request).await.map_err(|_| HttpError::WriteFailed)?;

    let mut header_end = 0;


    let mut response = Vec::new();
    loop {
        let old_len = response.len();
        response.resize(old_len + 16384, 0); // расширяем на 16 КБ

        match stream.read(&mut response[old_len..]).await {
            Ok(0) => {
                response.truncate(old_len);
                break;
            }
            Ok(n) => {
                response.truncate(old_len + n);
                if let Some(pos) = find_header_end(&response) {
                    header_end = pos;
                    break;
                }
            }
            Err(_) => return Err(HttpError::ReadFailed),
        }
    }


    // Парсим заголовки
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut resp = httparse::Response::new(&mut headers);

    let status = match resp.parse(&response) {
        Ok(httparse::Status::Complete(_)) => {
            let status = resp.code.unwrap_or(0);
            let reason = resp.reason.map(String::from);
            let headers: Vec<(String, String)> = resp.headers.iter().map(|h| {
                (String::from(h.name), core::str::from_utf8(h.value).unwrap_or("").into())
            }).collect();
            (status, reason, headers)
        }
        _ => return Err(HttpError::ParseFailed),
    };

    // Ищем Content-Length
    let content_length = find_content_length(status.2.clone());

    // Читаем тело
    let body_start = header_end;
    let mut body = Vec::new();

    if let Some(len) = content_length {
        let total_needed = header_end + len;
        response.reserve(total_needed);

        while response.len() < total_needed {
            let old_len = response.len();
            response.resize(response.len() + 16384, 0);

            match stream.read(&mut response[old_len..]).await {
                Ok(0) => { response.truncate(old_len); break; }
                Ok(n) => { response.truncate(old_len + n); }
                Err(_) => return Err(HttpError::ReadFailed),
            }
        }
        response.truncate(total_needed);
        body = response[header_end..].to_vec();
        // Тело — это &response[header_end..]
    } else {
        // Нет Content-Length — читаем до закрытия соединения
        body.extend_from_slice(&response[body_start..]);

        // Агрессивное чтение прямо в Vec
        loop {
            let old_len = body.len();
            body.resize(body.len() + 16384, 0); // расширяем на 16 КБ

            match stream.read(&mut body[old_len..]).await {
                Ok(0) => {
                    body.truncate(old_len);
                    break;
                }
                Ok(n) => {
                    body.truncate(old_len + n);
                }
                Err(_) => {
                    body.truncate(old_len);
                    break;
                }
            }
        }
    }

    Ok(HttpResponse {
        status: status.0,
        reason: status.1,
        headers: status.2,
        body,
    })
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i+4] == b"\r\n\r\n" {
            return Some(i + 4);
        }
    }
    None
}

fn find_content_length(headers: Vec<(String, String)>) -> Option<usize> {
    for (name, value) in headers {
        println!("{} = {:.10}", name, value);
        if name.eq_ignore_ascii_case("content-length") {
            return value.trim().parse().ok();
        }
    }
    None
}