//! HTTP header helpers (reqwless-style).

/// Well-known Content-Type / Accept values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    TextHtml,
    TextPlain,
    ApplicationJson,
    ApplicationFormUrlEncoded,
    ApplicationOctetStream,
    ApplicationXml,
    MultipartFormData,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::TextHtml => "text/html",
            ContentType::TextPlain => "text/plain",
            ContentType::ApplicationJson => "application/json",
            ContentType::ApplicationFormUrlEncoded => "application/x-www-form-urlencoded",
            ContentType::ApplicationOctetStream => "application/octet-stream",
            ContentType::ApplicationXml => "application/xml",
            ContentType::MultipartFormData => "multipart/form-data",
        }
    }
}

impl From<&[u8]> for ContentType {
    fn from(value: &[u8]) -> Self {
        match value {
            b"application/json" => ContentType::ApplicationJson,
            b"application/x-www-form-urlencoded" => ContentType::ApplicationFormUrlEncoded,
            b"text/html" => ContentType::TextHtml,
            b"text/plain" => ContentType::TextPlain,
            b"application/xml" | b"text/xml" => ContentType::ApplicationXml,
            b"multipart/form-data" => ContentType::MultipartFormData,
            _ => ContentType::ApplicationOctetStream,
        }
    }
}

pub(crate) fn write_header(out: &mut alloc::vec::Vec<u8>, name: &str, value: &str) {
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(b"\r\n");
}
