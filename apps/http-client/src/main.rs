#![no_std]
#![no_main]
extern crate alloc;

use libfelix::prelude::*;
use libfelix::async_rt::block_on;
use libfelix::net::client::{fetch, request, HttpMethod, HttpRequest};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let url = arg(1).unwrap_or("http://10.0.2.2:6666/");
    println!("Fetching: {}", url);

    block_on(async {
        // Простой GET запрос
        match fetch(url).await {
            Ok(response) => {
                println!("=== HTTP Response ===");
                println!("Status: {}", response.status);
                if let Some(reason) = &response.reason {
                    println!("Reason: {}", reason);
                }
                println!("--- Headers ---");
                for (key, value) in &response.headers {
                    println!("{}: {}", key, value);
                }
                println!("--- Body ({} bytes) ---", response.body.len());
                if let Ok(s) = core::str::from_utf8(&response.body) {
                    println!("{}", s);
                } else {
                    println!("[Binary data]");
                }
            }
            Err(e) => {
                println!("Error: {:?}", e);
            }
        }

        // Пример POST запроса
        // let req = HttpRequest::post("http://10.0.2.2:6666/api", b"data=test");
        // let response = request(req).await;
    });

    0
}