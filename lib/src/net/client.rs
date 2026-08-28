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
use core::str::FromStr;
use embedded_io_async::{Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use rand_core::{CryptoRng, RngCore};

use super::edge_adapter::FelixStack;
use edge_nal::TcpConnect;

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

    let ip = match core::net::Ipv4Addr::from_str(parsed.host) {
        Ok(ip) => ip,
        Err(_) => {
            let octets = crate::net::dns::resolve(parsed.host).map_err(|_| HttpError::DnsError)?;
            core::net::Ipv4Addr::from_octets(octets)
        }
    };

    let addr = core::net::SocketAddr::V4(core::net::SocketAddrV4::new(ip, parsed.port));

    let stack = FelixStack;
    let mut tcp_stream = stack
        .connect(addr)
        .await
        .map_err(|_| HttpError::TcpConnectFailed)?;

    if parsed.scheme == "https" {
        let mut read_buf = [0u8; 16640];
        let mut write_buf = [0u8; 16640];
        let mut rng = SimpleRng(0xDEAD_BEEF);

        let config = TlsConfig::new()
            .with_server_name(parsed.host)
            .enable_rsa_signatures();

        let mut tls = TlsConnection::new(tcp_stream, &mut read_buf, &mut write_buf);

        tls.open(TlsContext::new(
            &config,
            UnsecureProvider::new::<Aes128GcmSha256>(&mut rng),
        ))
        .await
        .map_err(|_| HttpError::TlsHandshakeFailed)?;

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
    let mut request = Vec::new();
    request.extend_from_slice(req.method.as_str().as_bytes());
    request.extend_from_slice(b" ");
    request.extend_from_slice(path.as_bytes());
    request.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(b"\r\n");

    for (key, value) in req.headers {
        request.extend_from_slice(key.as_bytes());
        request.extend_from_slice(b": ");
        request.extend_from_slice(value.as_bytes());
        request.extend_from_slice(b"\r\n");
    }

    if let Some(body) = req.body {
        request.extend_from_slice(b"Content-Length: ");
        let len_str = body.len().to_string();
        request.extend_from_slice(len_str.as_bytes());
        request.extend_from_slice(b"\r\n");
    }

    request.extend_from_slice(b"Connection: close\r\n\r\n");

    if let Some(body) = req.body {
        request.extend_from_slice(body);
    }

    stream
        .write_all(&request)
        .await
        .map_err(|_| HttpError::WriteFailed)?;
    stream.flush().await.map_err(|_| HttpError::WriteFailed)?;

    let mut header_end = 0;
    let mut response = Vec::new();
    loop {
        if !read_more(stream, &mut response).await? {
            break;
        }
        if let Some(pos) = find_header_end(&response) {
            header_end = pos;
            break;
        }
    }
    if header_end == 0 {
        return Err(HttpError::ParseFailed);
    }

    let mut headers_buf = [httparse::EMPTY_HEADER; 128];
    let mut resp = httparse::Response::new(&mut headers_buf);

    let (status_code, reason, headers) = match resp.parse(&response[..header_end]) {
        Ok(httparse::Status::Complete(_)) => {
            let status = resp.code.unwrap_or(0);
            let reason = resp.reason.map(String::from);
            let headers: Vec<(String, String)> = resp
                .headers
                .iter()
                .filter(|h| !h.name.is_empty())
                .map(|h| {
                    (
                        String::from(h.name),
                        core::str::from_utf8(h.value).unwrap_or("").into(),
                    )
                })
                .collect();
            (status, reason, headers)
        }
        _ => return Err(HttpError::ParseFailed),
    };

    let content_length = header_value(&headers, "content-length").and_then(|v| v.trim().parse().ok());
    let chunked = header_value(&headers, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);

    let mut raw_body = response[header_end..].to_vec();

    if let Some(len) = content_length {
        while raw_body.len() < len {
            if !read_more(stream, &mut raw_body).await? {
                break;
            }
        }
        raw_body.truncate(len);
    } else if chunked {
        loop {
            if let Some(decoded) = decode_chunked(&raw_body) {
                raw_body = decoded;
                break;
            }
            if !read_more(stream, &mut raw_body).await? {
                raw_body = decode_chunked(&raw_body).unwrap_or(raw_body);
                break;
            }
        }
    } else {
        loop {
            if !read_more(stream, &mut raw_body).await? {
                break;
            }
        }
    }

    Ok(HttpResponse {
        status: status_code,
        reason,
        headers,
        body: raw_body,
    })
}

async fn read_more<S: Read>(stream: &mut S, buf: &mut Vec<u8>) -> Result<bool, HttpError> {
    let old_len = buf.len();
    buf.resize(old_len + 16384, 0);
    match stream.read(&mut buf[old_len..]).await {
        Ok(0) => {
            buf.truncate(old_len);
            Ok(false)
        }
        Ok(n) => {
            buf.truncate(old_len + n);
            Ok(true)
        }
        Err(_) => {
            buf.truncate(old_len);
            Err(HttpError::ReadFailed)
        }
    }
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
        .or_else(|| data.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Returns decoded body when the last `0\r\n\r\n` chunk is present.
fn decode_chunked(input: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < input.len() {
        let rest = &input[i..];
        let line_end = rest.windows(2).position(|w| w == b"\r\n")?;
        let line = core::str::from_utf8(&rest[..line_end]).ok()?.trim();
        if line.is_empty() {
            i += line_end + 2;
            continue;
        }
        let size_str = line.split(';').next()?.trim();
        let size = usize::from_str_radix(size_str, 16).ok()?;
        i += line_end + 2;
        if size == 0 {
            return Some(out);
        }
        if i + size > input.len() {
            return None;
        }
        out.extend_from_slice(&input[i..i + size]);
        i += size;
        if i + 2 <= input.len() && &input[i..i + 2] == b"\r\n" {
            i += 2;
        } else if i < input.len() && input[i] == b'\n' {
            i += 1;
        }
    }
    None
}
