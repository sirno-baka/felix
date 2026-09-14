use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
};

use rhai::{CustomType, Dynamic, Engine, EvalAltResult, ImmutableString, Map, TypeBuilder};
use rustls::{
    pki_types::ServerName, ClientConfig, ClientConnection, RootCertStore, StreamOwned,
};
use url::Url;

use crate::runtime_error;

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "Http", extra = Self::build_rhai_api)]
pub struct HttpClient;

impl HttpClient {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_fn("get", http_get)
            .with_fn("post", http_post)
            .with_fn("put", http_put)
            .with_fn("patch", http_patch)
            .with_fn("delete", http_delete)
            .with_fn("request", http_request)
            .on_print(|_| "Http".into())
            .on_debug(|_| "Http".into());
    }
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "HttpRequest", extra = Self::build_rhai_api)]
pub struct HttpRequest {
    #[rhai_type(skip)] method: String,
    #[rhai_type(skip)] url: String,
    #[rhai_type(skip)] headers: Map,
    #[rhai_type(skip)] body: Option<Vec<u8>>,
}

impl HttpRequest {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_fn("header", request_header)
            .with_fn("query", request_query)
            .with_fn("json", request_json)
            .with_fn("body", request_text_body)
            .with_fn("body", request_bytes_body)
            .with_fn("send", request_send)
            .on_print(|request| format!("HttpRequest({} {})", request.method, request.url))
            .on_debug(|request| format!("HttpRequest({} {})", request.method, request.url));
    }
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "HttpResponse", extra = Self::build_rhai_api)]
pub struct HttpResponse {
    #[rhai_type(readonly)]
    pub status: i32,
    #[rhai_type(readonly)]
    pub body: String,
    #[rhai_type(readonly)]
    pub bytes: Vec<u8>,
    #[rhai_type(readonly)]
    pub headers: Map,
}

impl HttpResponse {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_get("ok", |response: &mut Self| (200..300).contains(&response.status))
            .with_fn("json", response_json)
            .on_print(|response| format!("HttpResponse({})", response.status))
            .on_debug(|response| format!("HttpResponse(status={}, bytes={})", response.status, response.body.len()));
    }
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "Json", extra = Self::build_rhai_api)]
pub struct JsonCodec;

impl JsonCodec {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_fn("parse", json_parse)
            .with_fn("stringify", json_stringify)
            .with_fn("pretty", json_pretty)
            .on_print(|_| "Json".into())
            .on_debug(|_| "Json".into());
    }
}

pub fn register(engine: &mut Engine) {
    engine
        .build_type::<HttpClient>()
        .build_type::<HttpRequest>()
        .build_type::<HttpResponse>()
        .build_type::<JsonCodec>();
}

fn http_get(_: &mut HttpClient, url: &str) -> Result<HttpResponse, Box<EvalAltResult>> {
    request("GET", url, None, None).map_err(runtime_error)
}

fn http_post(_: &mut HttpClient, url: &str, value: Dynamic) -> Result<HttpResponse, Box<EvalAltResult>> {
    let value: serde_json::Value = rhai::serde::from_dynamic(&value).map_err(runtime_error)?;
    let body = serde_json::to_vec(&value).map_err(runtime_error)?;
    request("POST", url, Some(body), None).map_err(runtime_error)
}

fn http_put(_: &mut HttpClient, url: &str, value: Dynamic) -> Result<HttpResponse, Box<EvalAltResult>> {
    send_json("PUT", url, value)
}

fn http_patch(_: &mut HttpClient, url: &str, value: Dynamic) -> Result<HttpResponse, Box<EvalAltResult>> {
    send_json("PATCH", url, value)
}

fn http_delete(_: &mut HttpClient, url: &str) -> Result<HttpResponse, Box<EvalAltResult>> {
    request("DELETE", url, None, None).map_err(runtime_error)
}

fn send_json(method: &str, url: &str, value: Dynamic) -> Result<HttpResponse, Box<EvalAltResult>> {
    let value: serde_json::Value = rhai::serde::from_dynamic(&value).map_err(runtime_error)?;
    let body = serde_json::to_vec(&value).map_err(runtime_error)?;
    request(method, url, Some(body), None).map_err(runtime_error)
}

fn http_request(_: &mut HttpClient, method: &str, url: &str) -> HttpRequest {
    HttpRequest { method: method.to_ascii_uppercase(), url: url.into(), headers: Map::new(), body: None }
}

fn request_header(request: &mut HttpRequest, name: &str, value: &str) -> HttpRequest {
    request.headers.insert(name.to_ascii_lowercase().into(), value.into());
    request.clone()
}

fn request_query(request: &mut HttpRequest, name: &str, value: Dynamic) -> Result<HttpRequest, Box<EvalAltResult>> {
    let mut url = Url::parse(&request.url).map_err(runtime_error)?;
    url.query_pairs_mut().append_pair(name, &value.to_string());
    request.url = url.to_string();
    Ok(request.clone())
}

fn request_json(request: &mut HttpRequest, value: Dynamic) -> Result<HttpRequest, Box<EvalAltResult>> {
    let value: serde_json::Value = rhai::serde::from_dynamic(&value).map_err(runtime_error)?;
    request.body = Some(serde_json::to_vec(&value).map_err(runtime_error)?);
    request.headers.insert("content-type".into(), "application/json".into());
    Ok(request.clone())
}

fn request_text_body(request: &mut HttpRequest, value: &str) -> HttpRequest {
    request.body = Some(value.as_bytes().to_vec());
    request.clone()
}

fn request_bytes_body(request: &mut HttpRequest, value: Vec<u8>) -> HttpRequest {
    request.body = Some(value);
    request.clone()
}

fn request_send(builder: &mut HttpRequest) -> Result<HttpResponse, Box<EvalAltResult>> {
    request(&builder.method, &builder.url, builder.body.clone(), Some(&builder.headers)).map_err(runtime_error)
}

fn response_json(response: &mut HttpResponse) -> Result<Dynamic, Box<EvalAltResult>> {
    json_parse(&mut JsonCodec, &response.body)
}

fn json_parse(_: &mut JsonCodec, text: &str) -> Result<Dynamic, Box<EvalAltResult>> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(runtime_error)?;
    rhai::serde::to_dynamic(value).map_err(runtime_error)
}

fn json_stringify(_: &mut JsonCodec, value: Dynamic) -> Result<String, Box<EvalAltResult>> {
    let value: serde_json::Value = rhai::serde::from_dynamic(&value).map_err(runtime_error)?;
    serde_json::to_string(&value).map_err(runtime_error)
}

fn json_pretty(_: &mut JsonCodec, value: Dynamic) -> Result<String, Box<EvalAltResult>> {
    let value: serde_json::Value = rhai::serde::from_dynamic(&value).map_err(runtime_error)?;
    serde_json::to_string_pretty(&value).map_err(runtime_error)
}

fn request(method: &str, input_url: &str, body: Option<Vec<u8>>, headers: Option<&Map>) -> Result<HttpResponse, String> {
    let mut url = Url::parse(input_url).map_err(|error| format!("invalid URL: {error}"))?;
    let mut method = method;
    let mut body = body;

    for redirect in 0..=MAX_REDIRECTS {
        let response = request_once(method, &url, body.as_deref(), headers)?;
        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        if redirect == MAX_REDIRECTS {
            return Err(format!("too many redirects (>{MAX_REDIRECTS})"));
        }
        let location = response.headers.get("location")
            .and_then(|value| value.clone().try_cast::<ImmutableString>())
            .map(ImmutableString::into_owned)
            .ok_or_else(|| format!("redirect {} has no Location header", response.status))?;
        url = url.join(&location).map_err(|error| format!("invalid redirect URL: {error}"))?;
        if response.status == 303 || ((response.status == 301 || response.status == 302) && method == "POST") {
            method = "GET";
            body = None;
        }
    }
    unreachable!()
}

fn request_once(method: &str, url: &Url, body: Option<&[u8]>, headers: Option<&Map>) -> Result<HttpResponse, String> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported URL scheme: {scheme}"));
    }
    let host = url.host_str().ok_or_else(|| "URL has no host".to_string())?;
    let port = url.port_or_known_default().ok_or_else(|| "URL has no port".to_string())?;
    let stream = connect(host, port)?;
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase() || b == b'-') {
        return Err("invalid HTTP method".into());
    }
    if let Some(headers) = headers {
        for (name, value) in headers {
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || value.to_string().contains(['\r', '\n']) {
                return Err("invalid HTTP header".into());
            }
            if matches!(name.as_str(), "host" | "content-length" | "transfer-encoding" | "connection") {
                return Err(format!("HTTP header is managed by the client: {name}"));
            }
        }
    }
    let request = build_request(method, url, host, port, body, headers);

    let raw = if scheme == "https" {
        let server_name = ServerName::try_from(host.to_owned()).map_err(|error| format!("invalid TLS server name: {error}"))?;
        let connection = ClientConnection::new(tls_config()?, server_name).map_err(|error| format!("TLS setup failed: {error}"))?;
        let mut stream = StreamOwned::new(connection, stream);
        exchange(&mut stream, &request)?
    } else {
        let mut stream = stream;
        exchange(&mut stream, &request)?
    };
    parse_response(raw)
}

fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let mut last_error = None;
    for address in (host, port).to_socket_addrs().map_err(|error| format!("DNS failed: {error}"))? {
        match TcpStream::connect(address) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.map(|error| format!("connection failed: {error}")).unwrap_or_else(|| "DNS returned no addresses".into()))
}

fn tls_config() -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls_rustcrypto::provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("TLS configuration failed: {error}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn build_request(method: &str, url: &Url, host: &str, port: u16, body: Option<&[u8]>, headers: Option<&Map>) -> Vec<u8> {
    let mut target = if url.path().is_empty() { "/".to_string() } else { url.path().to_string() };
    if let Some(query) = url.query() { target.push('?'); target.push_str(query); }
    let default_port = (url.scheme() == "https" && port == 443) || (url.scheme() == "http" && port == 80);
    let authority = if default_port { host.to_string() } else { format!("{host}:{port}") };
    let body = body.unwrap_or_default();
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: PopugOS-Rhai/0.1\r\nAccept: application/json, */*\r\nConnection: close\r\n"
    );
    if let Some(headers) = headers {
        for (name, value) in headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(&value.to_string());
            head.push_str("\r\n");
        }
    }
    if !body.is_empty() {
        if !headers.map(|values| values.contains_key("content-type")).unwrap_or(false) {
            head.push_str("Content-Type: application/json\r\n");
        }
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    request
}

fn exchange(stream: &mut (impl Read + Write), request: &[u8]) -> Result<Vec<u8>, String> {
    stream.write_all(request).map_err(|error| format!("request write failed: {error}"))?;
    stream.flush().map_err(|error| format!("request flush failed: {error}"))?;
    let mut raw = Vec::new();
    stream.take((MAX_RESPONSE_BYTES + 1) as u64).read_to_end(&mut raw)
        .map_err(|error| format!("response read failed: {error}"))?;
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(format!("response exceeds {MAX_RESPONSE_BYTES} bytes"));
    }
    Ok(raw)
}

fn parse_response(raw: Vec<u8>) -> Result<HttpResponse, String> {
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "invalid HTTP response: no header terminator".to_string())?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| "HTTP headers are not UTF-8".to_string())?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| "invalid HTTP status line".to_string())?;
    let status = status_line.split_whitespace().nth(1)
        .ok_or_else(|| "invalid HTTP status line".to_string())?
        .parse::<i32>().map_err(|_| "invalid HTTP status code".to_string())?;
    let mut headers = Map::new();
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue; };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") { chunked = true; }
        headers.insert(name.into(), value.into());
    }
    let bytes = &raw[split + 4..];
    let bytes = if chunked { decode_chunked(bytes)? } else {
        let len = headers.get("content-length")
            .and_then(|value| value.clone().try_cast::<ImmutableString>())
            .map(|value| value.parse::<usize>().map_err(|_| "invalid Content-Length".to_string()))
            .transpose()?;
        if let Some(len) = len {
            if bytes.len() < len { return Err("truncated HTTP response body".into()); }
            bytes[..len].to_vec()
        } else { bytes.to_vec() }
    };
    let header = |name: &str| headers.get(name)
        .and_then(|value| value.clone().try_cast::<ImmutableString>())
        .map(|value| value.to_string()).unwrap_or_default();
    let encoding = header("content-encoding").to_ascii_lowercase();
    let bytes = match encoding.trim() {
        "" | "identity" => bytes,
        "gzip" => bounded_decode(flate2::read::MultiGzDecoder::new(bytes.as_slice()))?,
        "deflate" => bounded_decode(flate2::read::ZlibDecoder::new(bytes.as_slice()))?,
        other => return Err(format!("unsupported Content-Encoding: {other}")),
    };
    let content_type = header("content-type");
    let charset = content_type.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.trim().split_once('=')?;
        key.eq_ignore_ascii_case("charset").then(|| value.trim().trim_matches('"'))
    });
    let codec = match charset {
        Some(label) => encoding_rs::Encoding::for_label(label.as_bytes())
            .ok_or_else(|| format!("unsupported charset: {label}"))?,
        None => encoding_rs::UTF_8,
    };
    let (body, _, _) = codec.decode(&bytes);
    let body = body.into_owned();
    Ok(HttpResponse { status, body, bytes, headers })
}

fn bounded_decode(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    reader.take((MAX_RESPONSE_BYTES + 1) as u64).read_to_end(&mut decoded)
        .map_err(|error| format!("invalid compressed response: {error}"))?;
    if decoded.len() > MAX_RESPONSE_BYTES { return Err("decoded response is too large".into()); }
    Ok(decoded)
}

fn decode_chunked(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut cursor = 0;
    loop {
        let line_end = input[cursor..].windows(2).position(|window| window == b"\r\n")
            .map(|offset| cursor + offset).ok_or_else(|| "invalid chunked response".to_string())?;
        let size_text = std::str::from_utf8(&input[cursor..line_end]).map_err(|_| "invalid chunk size".to_string())?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| "invalid chunk size".to_string())?;
        cursor = line_end + 2;
        if size == 0 { break; }
        let end = cursor.checked_add(size).ok_or_else(|| "chunk size overflow".to_string())?;
        let next = end.checked_add(2).ok_or_else(|| "chunk size overflow".to_string())?;
        if next > input.len() || &input[end..next] != b"\r\n" {
            return Err("truncated chunked response".into());
        }
        output.extend_from_slice(&input[cursor..end]);
        if output.len() > MAX_RESPONSE_BYTES { return Err("decoded response is too large".into()); }
        cursor = end + 2;
    }
    Ok(output)
}
