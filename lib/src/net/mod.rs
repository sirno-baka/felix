pub mod edge_adapter;
pub mod dns;
pub mod client;
pub mod headers;

pub use client::{
    delete, fetch, get, post, put, request, HttpError, HttpMethod, HttpRequest, HttpResponse,
};
pub use headers::ContentType;

use edge_adapter::*;
