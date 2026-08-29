//! HTTP/HTTPS client for Felix OS.
//!
//! ```no_run
//! use libfelix::net::{get, post, ContentType, HttpRequest};
//!
//! let page = get("https://example.com/").await?;
//! let created = post("http://10.0.2.2:8899/api", br#"{"a":1}"#, ContentType::ApplicationJson).await?;
//! let custom = HttpRequest::get("https://example.com/search")
//!     .accept(ContentType::ApplicationJson)
//!     .headers(&[("User-Agent", "Felix/0.1"), ("X-Token", "abc")])
//!     .send()
//!     .await?;
//! ```

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::str::FromStr;
use embedded_io_async::{Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use rand_core::{CryptoRng, RngCore};

use super::edge_adapter::FelixStack;
use super::headers::{write_header, ContentType};
use edge_nal::TcpConnect;

pub use super::headers::ContentType as HttpContentType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Head,
    Patch,
}

impl HttpMethod {
    fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
            HttpMethod::Patch => "PATCH",
        }
    }
}

/// HTTP request. Built via `get`/`post`/`HttpRequest::get` and sent with `.send()`.
pub struct HttpRequest<'a> {
    pub url: &'a str,
    pub method: HttpMethod,
    pub content_type: Option<ContentType>,
    pub accept: Option<ContentType>,
    pub extra_headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
}

impl<'a> HttpRequest<'a> {
    pub fn new(method: HttpMethod, url: &'a str) -> Self {
        Self {
            url,
            method,
            content_type: None,
            accept: None,
            extra_headers: &[],
            body: None,
        }
    }

    pub fn get(url: &'a str) -> Self {
        Self::new(HttpMethod::Get, url)
    }

    pub fn post(url: &'a str, body: &'a [u8]) -> Self {
        Self::new(HttpMethod::Post, url)
            .body(body)
            .content_type(ContentType::ApplicationFormUrlEncoded)
    }

    pub fn put(url: &'a str, body: &'a [u8]) -> Self {
        Self::new(HttpMethod::Put, url).body(body)
    }

    pub fn delete(url: &'a str) -> Self {
        Self::new(HttpMethod::Delete, url)
    }

    pub fn head(url: &'a str) -> Self {
        Self::new(HttpMethod::Head, url)
    }

    pub fn patch(url: &'a str, body: &'a [u8]) -> Self {
        Self::new(HttpMethod::Patch, url).body(body)
    }

    pub fn content_type(mut self, content_type: ContentType) -> Self {
        self.content_type = Some(content_type);
        self
    }

    pub fn accept(mut self, accept: ContentType) -> Self {
        self.accept = Some(accept);
        self
    }

    pub fn headers(mut self, headers: &'a [(&'a str, &'a str)]) -> Self {
        self.extra_headers = headers;
        self
    }

    pub fn body(mut self, body: &'a [u8]) -> Self {
        self.body = Some(body);
        self
    }

    pub async fn send(self) -> Result<HttpResponse, HttpError> {
        request(self).await
    }

    fn write_header(&self, host: &str, path: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.method.as_str().as_bytes());
        out.extend_from_slice(b" ");
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(b" HTTP/1.1\r\n");

        write_header(&mut out, "Host", host);

        if let Some(ct) = self.content_type {
            write_header(&mut out, "Content-Type", ct.as_str());
        }
        if let Some(accept) = self.accept {
            write_header(&mut out, "Accept", accept.as_str());
        }
        if let Some(body) = self.body {
            write_header(&mut out, "Content-Length", &body.len().to_string());
        }

        let mut has_ua = false;
        let mut has_connection = false;
        for (name, value) in self.extra_headers {
            if name.eq_ignore_ascii_case("host")
                || name.eq_ignore_ascii_case("content-length")
                || name.eq_ignore_ascii_case("content-type") && self.content_type.is_some()
                || name.eq_ignore_ascii_case("accept") && self.accept.is_some()
            {
                continue;
            }
            if name.eq_ignore_ascii_case("user-agent") {
                has_ua = true;
            }
            if name.eq_ignore_ascii_case("connection") {
                has_connection = true;
            }
            write_header(&mut out, name, value);
        }
        if !has_ua {
            write_header(&mut out, "User-Agent", "Felix/0.1");
        }
        if !has_connection {
            write_header(&mut out, "Connection", "close");
        }

        out.extend_from_slice(b"\r\n");
        if let Some(body) = self.body {
            out.extend_from_slice(body);
        }
        out
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub reason: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_value(&self.headers, name)
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }
}

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
        if h.is_empty() || authority[colon + 1..].bytes().any(|b| !b.is_ascii_digit()) {
            (authority, if scheme == "https" { 443 } else { 80 })
        } else {
            let p: u16 = authority[colon + 1..].parse().ok()?;
            (h, p)
        }
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

pub async fn fetch(url: &str) -> Result<HttpResponse, HttpError> {
    get(url).await
}

pub async fn get(url: &str) -> Result<HttpResponse, HttpError> {
    HttpRequest::get(url).send().await
}

pub async fn post(
    url: &str,
    body: &[u8],
    content_type: ContentType,
) -> Result<HttpResponse, HttpError> {
    HttpRequest::post(url, body)
        .content_type(content_type)
        .send()
        .await
}

pub async fn put(
    url: &str,
    body: &[u8],
    content_type: ContentType,
) -> Result<HttpResponse, HttpError> {
    HttpRequest::put(url, body)
        .content_type(content_type)
        .send()
        .await
}

pub async fn delete(url: &str) -> Result<HttpResponse, HttpError> {
    HttpRequest::delete(url).send().await
}

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
        https_request(tcp_stream, parsed.host, parsed.path, &req).await
    } else {
        do_http_request(&mut tcp_stream, parsed.host, parsed.path, &req).await
    }
}

async fn https_request<S: Read + Write>(
    tcp_stream: S,
    host: &str,
    path: &str,
    req: &HttpRequest<'_>,
) -> Result<HttpResponse, HttpError> {
    // Heap: these are 16KiB each and must not live in the async state machine.
    let mut read_buf = alloc::vec![0u8; 16640];
    let mut write_buf = alloc::vec![0u8; 16640];
    let mut rng = SimpleRng(0xDEAD_BEEF);

    let config = TlsConfig::new()
        .with_server_name(host)
        .enable_rsa_signatures();

    let mut tls = TlsConnection::new(tcp_stream, &mut read_buf, &mut write_buf);
    tls.open(TlsContext::new(
        &config,
        UnsecureProvider::new::<Aes128GcmSha256>(&mut rng),
    ))
    .await
    .map_err(|_| HttpError::TlsHandshakeFailed)?;

    do_http_request(&mut tls, host, path, req).await
}

async fn do_http_request<S: Read + Write>(
    stream: &mut S,
    host: &str,
    path: &str,
    req: &HttpRequest<'_>,
) -> Result<HttpResponse, HttpError> {
    let wire = req.write_header(host, path);
    stream
        .write_all(&wire)
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

    let content_length =
        header_value(&headers, "content-length").and_then(|v| v.trim().parse().ok());
    let chunked = header_value(&headers, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);

    let mut raw_body = response[header_end..].to_vec();

    if req.method == HttpMethod::Head {
        raw_body.clear();
    } else if let Some(len) = content_length {
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
    data.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| data.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

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
