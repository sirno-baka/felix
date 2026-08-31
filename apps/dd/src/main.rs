#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use libfelix::prelude::*;
use libfelix::syscall::{O_RDWR, O_WRONLY};

fn usage() {
    println!("usage: dd if=<src> of=<dst> [bs=N] [count=N] [skip=N]");
    println!("   or: dd <src> <dst>");
    println!("copy raw bytes between files or block devices");
    println!("example: dd if=/dev/ram0 of=/dev/sda bs=4096");
}

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Some(rest) = s.strip_suffix('k').or_else(|| s.strip_suffix('K')) {
        return rest.parse::<u32>().ok().map(|n| n.saturating_mul(1024));
    }
    if let Some(rest) = s.strip_suffix('m').or_else(|| s.strip_suffix('M')) {
        return rest
            .parse::<u32>()
            .ok()
            .map(|n| n.saturating_mul(1024 * 1024));
    }
    s.parse().ok()
}

fn kv(arg: &str) -> Option<(&str, &str)> {
    arg.split_once('=')
}

fn open_out(path: &str) -> Result<File, IoError> {
    if path.starts_with("/dev/") {
        File::open_flags(path, O_WRONLY).or_else(|_| File::open_flags(path, O_RDWR))
    } else {
        File::create(path)
    }
}

fn discard(f: &mut File, bytes: u32, chunk: usize) -> Result<(), IoError> {
    if bytes == 0 {
        return Ok(());
    }
    let mut left = bytes as usize;
    let mut buf = vec![0u8; chunk.max(1)];
    while left > 0 {
        let n = chunk.min(left);
        let got = f.read(&mut buf[..n])?;
        if got == 0 {
            break;
        }
        left -= got;
    }
    Ok(())
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let raw: Vec<&str> = args().collect();
    let mut src: Option<&str> = None;
    let mut dst: Option<&str> = None;
    let mut bs: u32 = 4096;
    let mut count: Option<u32> = None;
    let mut skip: u32 = 0;

    for (i, a) in raw.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if *a == "-h" || *a == "--help" {
            usage();
            return 0;
        }
        if let Some((k, v)) = kv(a) {
            match k {
                "if" => src = Some(v),
                "of" => dst = Some(v),
                "bs" => {
                    if let Some(n) = parse_u32(v) {
                        if n > 0 {
                            bs = n;
                        }
                    }
                }
                "count" => count = parse_u32(v),
                "skip" => skip = parse_u32(v).unwrap_or(0),
                _ => {}
            }
            continue;
        }
        if src.is_none() {
            src = Some(*a);
        } else if dst.is_none() {
            dst = Some(*a);
        }
    }

    let (Some(src), Some(dst)) = (src, dst) else {
        usage();
        return 1;
    };
    if src == dst {
        println!("dd: if and of are the same");
        return 1;
    }

    let mut inf = match File::open_ro(src) {
        Ok(f) => f,
        Err(e) => {
            println!("dd: open {}: {:?}", src, e);
            return 1;
        }
    };
    let mut outf = match open_out(dst) {
        Ok(f) => f,
        Err(e) => {
            println!("dd: open {}: {:?}", dst, e);
            return 1;
        }
    };

    let chunk = (bs as usize).clamp(512, 64 * 1024);
    if skip > 0 {
        if let Err(e) = discard(&mut inf, skip.saturating_mul(bs), chunk) {
            println!("dd: skip failed: {:?}", e);
            return 1;
        }
    }

    let mut buf = vec![0u8; chunk];
    let mut copied: u64 = 0;
    let mut blocks: u32 = 0;
    loop {
        if let Some(max) = count {
            if blocks >= max {
                break;
            }
        }
        let n = match inf.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                println!("dd: read {}: {:?}", src, e);
                break;
            }
        };
        let mut off = 0;
        while off < n {
            match outf.write(&buf[off..n]) {
                Ok(0) => {
                    println!("dd: write {}: short", dst);
                    println!("{}+0 records in", blocks);
                    println!("{} bytes copied", copied);
                    return 1;
                }
                Ok(w) => {
                    off += w;
                    copied += w as u64;
                }
                Err(e) => {
                    println!("dd: write {}: {:?}", dst, e);
                    println!("{} bytes copied", copied);
                    return 1;
                }
            }
        }
        blocks += 1;
    }

    println!("{}+0 records in", blocks);
    println!("{} bytes copied", copied);
    0
}
