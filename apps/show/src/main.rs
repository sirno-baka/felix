#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libfelix::async_rt::yield_now;
use libfelix::embedded_graphics::mono_font::MonoTextStyle;
use libfelix::embedded_graphics::pixelcolor::Rgb888;
use libfelix::embedded_graphics::prelude::*;
use libfelix::embedded_graphics::text::{Baseline, Text};
use libfelix::fs;
use libfelix::prelude::*;
use libfelix::wm::{rgb, screen_size, Window, EV_CLOSE, EV_KEY_DOWN};
use embedded_graphics_unicodefonts::mono_9x18_atlas;
use zune_core::colorspace::ColorSpace;
use zune_jpeg::JpegDecoder;
use zune_png::PngDecoder;

const SCAN_ESC: u8 = 0x01;
const SCAN_Q: u8 = 0x10;
const BG: u32 = 0x0010_1820;
const MAX_WIN_W: u32 = 720;
const MAX_WIN_H: u32 = 520;

struct RgbImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn usage() {
    println!("usage: show <file.jpg|file.png>");
}

fn ext_of(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((_, e)) => e,
        None => "",
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(path)
}

fn looks_png(data: &[u8], path: &str) -> bool {
    data.starts_with(&[0x89, b'P', b'N', b'G']) || ext_of(path).eq_ignore_ascii_case("png")
}

fn looks_jpeg(data: &[u8], path: &str) -> bool {
    data.starts_with(&[0xFF, 0xD8])
        || ext_of(path).eq_ignore_ascii_case("jpg")
        || ext_of(path).eq_ignore_ascii_case("jpeg")
}

fn to_rgb8(src: &[u8], w: u32, h: u32, space: ColorSpace) -> Option<RgbImage> {
    let n = (w as usize).saturating_mul(h as usize);
    let mut pixels = Vec::with_capacity(n.saturating_mul(3));
    match space {
        ColorSpace::RGB => {
            if src.len() < n * 3 {
                return None;
            }
            pixels.extend_from_slice(&src[..n * 3]);
        }
        ColorSpace::RGBA => {
            if src.len() < n * 4 {
                return None;
            }
            for chunk in src.chunks_exact(4).take(n) {
                pixels.extend_from_slice(&chunk[..3]);
            }
        }
        ColorSpace::Luma => {
            if src.len() < n {
                return None;
            }
            for &y in src.iter().take(n) {
                pixels.push(y);
                pixels.push(y);
                pixels.push(y);
            }
        }
        ColorSpace::LumaA => {
            if src.len() < n * 2 {
                return None;
            }
            for chunk in src.chunks_exact(2).take(n) {
                pixels.push(chunk[0]);
                pixels.push(chunk[0]);
                pixels.push(chunk[0]);
            }
        }
        ColorSpace::BGR => {
            if src.len() < n * 3 {
                return None;
            }
            for chunk in src.chunks_exact(3).take(n) {
                pixels.push(chunk[2]);
                pixels.push(chunk[1]);
                pixels.push(chunk[0]);
            }
        }
        ColorSpace::BGRA => {
            if src.len() < n * 4 {
                return None;
            }
            for chunk in src.chunks_exact(4).take(n) {
                pixels.push(chunk[2]);
                pixels.push(chunk[1]);
                pixels.push(chunk[0]);
            }
        }
        _ => return None,
    }
    Some(RgbImage {
        width: w,
        height: h,
        pixels,
    })
}

fn decode_jpeg(data: &[u8]) -> Result<RgbImage, &'static str> {
    let mut dec = JpegDecoder::new(data);
    let raw = dec.decode().map_err(|_| "jpeg decode failed")?;
    let info = dec.info().ok_or("jpeg has no info")?;
    let space = dec.get_output_colorspace().unwrap_or(ColorSpace::RGB);
    to_rgb8(&raw, info.width as u32, info.height as u32, space).ok_or("jpeg colorspace")
}

fn decode_png(data: &[u8]) -> Result<RgbImage, &'static str> {
    let mut dec = PngDecoder::new(data);
    let decoded = dec.decode().map_err(|_| "png decode failed")?;
    let (w, h) = dec.get_dimensions().ok_or("png has no size")?;
    let space = dec.get_colorspace().unwrap_or(ColorSpace::RGB);
    let raw = match decoded {
        zune_core::result::DecodingResult::U8(v) => v,
        _ => return Err("png bit depth"),
    };
    to_rgb8(&raw, w as u32, h as u32, space).ok_or("png colorspace")
}

fn decode_image(path: &str, data: &[u8]) -> Result<RgbImage, &'static str> {
    if looks_png(data, path) {
        decode_png(data)
    } else if looks_jpeg(data, path) {
        decode_jpeg(data)
    } else {
        Err("not jpeg/png")
    }
}

fn fit_window(img_w: u32, img_h: u32) -> (u32, u32) {
    let (sw, sh) = screen_size();
    let max_w = MAX_WIN_W.min(sw.saturating_sub(40)).max(160);
    let max_h = MAX_WIN_H.min(sh.saturating_sub(60)).max(120);
    if img_w == 0 || img_h == 0 {
        return (320, 200);
    }
    let mut w = img_w.max(160);
    let mut h = img_h.max(120);
    if w > max_w || h > max_h {
        let sx = (max_w as u64 * 1000) / img_w as u64;
        let sy = (max_h as u64 * 1000) / img_h as u64;
        let s = sx.min(sy).max(1);
        w = ((img_w as u64 * s) / 1000).max(1) as u32;
        h = ((img_h as u64 * s) / 1000).max(1) as u32;
    }
    (w.max(160), h.max(80))
}

fn blit_fit(win: &mut Window, img: &RgbImage) {
    win.fill(BG);
    let cw = win.client_width();
    let ch = win.client_height();
    if img.width == 0 || img.height == 0 || cw == 0 || ch == 0 {
        return;
    }
    let sx = (cw as u64 * 1000) / img.width as u64;
    let sy = (ch as u64 * 1000) / img.height as u64;
    let s = sx.min(sy).max(1);
    let dw = ((img.width as u64 * s) / 1000).max(1) as u32;
    let dh = ((img.height as u64 * s) / 1000).max(1) as u32;
    let ox = cw.saturating_sub(dw) / 2;
    let oy = ch.saturating_sub(dh) / 2;
    for dy in 0..dh {
        let sy = ((dy as u64 * img.height as u64) / dh as u64) as u32;
        if sy >= img.height {
            break;
        }
        let src_row = (sy as usize) * (img.width as usize) * 3;
        for dx in 0..dw {
            let sx = ((dx as u64 * img.width as u64) / dw as u64) as u32;
            if sx >= img.width {
                break;
            }
            let i = src_row + (sx as usize) * 3;
            let r = img.pixels[i];
            let g = img.pixels[i + 1];
            let b = img.pixels[i + 2];
            win.put_pixel(ox + dx, oy + dy, rgb(r, g, b));
        }
    }
}

fn draw_message(win: &mut Window, msg: &str) {
    win.fill(BG);
    let atlas = mono_9x18_atlas();
    let style = MonoTextStyle::new(&atlas, Rgb888::new(0xF0, 0xF0, 0xF0));
    let _ = Text::with_baseline(msg, Point::new(12, 16), style, Baseline::Top).draw(win);
    let _ = win.flip();
}

fn event_loop(win: &mut Window) {
    let mut events = [libfelix::wm::WmEvent::default(); 32];
    loop {
        let n = win.poll_events(&mut events);
        for ev in events.iter().take(n) {
            match ev.kind {
                EV_CLOSE => return,
                EV_KEY_DOWN => {
                    let scan = ev.a as u8;
                    if scan == SCAN_ESC || scan == SCAN_Q {
                        return;
                    }
                }
                _ => {}
            }
        }
        yield_now();
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = Args::parse();
    let path = match args.get(0) {
        Some(p) => p,
        None => {
            usage();
            return 1;
        }
    };

    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            println!("show: cannot read {}: {:?}", path, e);
            return 1;
        }
    };

    let title = {
        let mut t = String::from("show: ");
        t.push_str(basename(path));
        t
    };

    match decode_image(path, &data) {
        Ok(img) => {
            println!("show: {}x{} {}", img.width, img.height, path);
            let (ww, wh) = fit_window(img.width, img.height);
            let mut win = match Window::create(40, 40, ww, wh, &title) {
                Some(w) => w,
                None => {
                    println!("show: cannot create window");
                    return 1;
                }
            };
            blit_fit(&mut win, &img);
            let _ = win.flip();
            event_loop(&mut win);
        }
        Err(msg) => {
            println!("show: {}", msg);
            let mut win = match Window::create(80, 80, 360, 80, "show") {
                Some(w) => w,
                None => return 1,
            };
            draw_message(&mut win, &format!("error: {}", msg));
            event_loop(&mut win);
            return 1;
        }
    }
    0
}
