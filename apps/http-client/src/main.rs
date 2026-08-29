#![no_std]
#![no_main]
extern crate alloc;

use libfelix::async_rt::block_on;
use libfelix::net::{get, post, ContentType, HttpResponse};
use libfelix::prelude::*;

fn print_response(response: &HttpResponse) {
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
}

fn is_method(s: &str) -> bool {
    s.eq_ignore_ascii_case("GET")
        || s.eq_ignore_ascii_case("POST")
        || s.eq_ignore_ascii_case("PUT")
        || s.eq_ignore_ascii_case("DELETE")
        || s.eq_ignore_ascii_case("HEAD")
        || s.eq_ignore_ascii_case("PATCH")
}

fn is_post(s: &str) -> bool {
    s.eq_ignore_ascii_case("POST")
}

fn guess_content_type(data: &str, json_flag: bool) -> ContentType {
    if json_flag {
        return ContentType::ApplicationJson;
    }
    let t = data.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        ContentType::ApplicationJson
    } else {
        ContentType::ApplicationFormUrlEncoded
    }
}

fn usage() {
    println!("usage:");
    println!("  http-client [GET] <url> [-o file]");
    println!("  http-client POST <url> -d|--data <body> [--json] [-o file]");
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = Args::parse_valued(&["d", "data", "o", "output", "X"]);

    if args.has("h") || args.has("help") {
        usage();
        return 0;
    }

    let first = args.get(0);
    let method = args.value("X").or_else(|| first.filter(|s| is_method(s)));
    let url = if first.map(is_method).unwrap_or(false) {
        args.get(1)
    } else {
        first
    }
    .unwrap_or("http://10.0.2.2:8899/");

    let data = args.value("data").or_else(|| args.value("d"));
    let out = args.value("output").or_else(|| args.value("o"));
    let json = args.has("json");
    let do_post = method.map(is_post).unwrap_or(false);

    if do_post && data.is_none() {
        println!("POST requires -d/--data <body>");
        usage();
        return 1;
    }

    if do_post {
        println!("POST {}", url);
    } else {
        println!("GET {}", url);
    }

    block_on(async {
        let response = if do_post {
            let body = data.unwrap();
            post(url, body.as_bytes(), guess_content_type(body, json)).await
        } else {
            get(url).await
        };

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                println!("Error: {:?}", e);
                return;
            }
        };

        if let Some(filename) = out {
            match File::create(filename) {
                Ok(mut fd) => {
                    if let Err(e) = fd.write_all(response.body.as_slice()) {
                        println!("Error write file: {:?}", e);
                    }
                }
                Err(e) => println!("Error open file: {:?}", e),
            }
            return;
        }

        print_response(&response);
    });
    println!("Done");
    0
}
