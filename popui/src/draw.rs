use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::{Rgb888, RgbColor},
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
    Pixel,
};
use embedded_graphics_unicodefonts::MONO_9X18;
use popugos::window::Window;

use crate::{Color, Point as UiPoint, Rect};

#[inline]
pub(crate) fn rgb(color: Color) -> Rgb888 {
    Rgb888::new(color.r, color.g, color.b)
}

pub(crate) fn set_clip(window: &mut Window, clip: Option<Rect>) {
    window.set_clip(clip.map(to_rectangle));
}

pub(crate) fn fill_rect(window: &mut Window, rect: Rect, color: Color) {
    if rect.w == 0 || rect.h == 0 { return; }
    let _ = to_rectangle(rect)
        .into_styled(PrimitiveStyle::with_fill(rgb(color)))
        .draw(window);
}

pub(crate) fn stroke_rect(window: &mut Window, rect: Rect, color: Color, width: u32) {
    if rect.w == 0 || rect.h == 0 || width == 0 { return; }
    let _ = to_rectangle(rect)
        .into_styled(PrimitiveStyle::with_stroke(rgb(color), width))
        .draw(window);
}

pub(crate) fn line(window: &mut Window, from: UiPoint, to: UiPoint, color: Color, width: u32) {
    let _ = Line::new(Point::new(from.x, from.y), Point::new(to.x, to.y))
        .into_styled(PrimitiveStyle::with_stroke(rgb(color), width.max(1)))
        .draw(window);
}

pub(crate) fn text(window: &mut Window, at: UiPoint, value: &str, color: Color) {
    let style = MonoTextStyle::new(&MONO_9X18, rgb(color));
    let _ = Text::with_baseline(value, Point::new(at.x, at.y), style, Baseline::Top).draw(window);
}

pub(crate) fn blit_rgb565(
    window: &mut Window,
    at: UiPoint,
    width: u16,
    height: u16,
    pixels: &[u16],
    mask: Option<&[u8]>,
) {
    blit_rgb565_cropped(window, at, width, height, pixels, mask, width as i32, height as i32);
}

/// Draw an RGB565 image centered inside a box, cropping the source when the
/// image is larger than the requested box. There is intentionally no scaling.
pub(crate) fn blit_rgb565_cropped(
    window: &mut Window,
    at: UiPoint,
    width: u16,
    height: u16,
    pixels: &[u16],
    mask: Option<&[u8]>,
    max_w: i32,
    max_h: i32,
) {
    let source_w = width as usize;
    let source_h = height as usize;
    if source_w == 0 || source_h == 0 || max_w <= 0 || max_h <= 0 { return; }

    let draw_w = source_w.min(max_w as usize);
    let draw_h = source_h.min(max_h as usize);
    let src_x = (source_w - draw_w) / 2;
    let src_y = (source_h - draw_h) / 2;
    let dst_x = at.x + (max_w - draw_w as i32) / 2;
    let dst_y = at.y + (max_h - draw_h as i32) / 2;

    let iter = (0..draw_h).flat_map(|dy| {
        (0..draw_w).filter_map(move |dx| {
            let index = (src_y + dy).checked_mul(source_w)?.checked_add(src_x + dx)?;
            let raw = *pixels.get(index)?;
            if !mask_opaque(mask, index) { return None; }
            Some(Pixel(
                Point::new(dst_x + dx as i32, dst_y + dy as i32),
                rgb565_to_rgb888(raw),
            ))
        })
    });
    let _ = window.draw_iter(iter);
}

#[inline]
pub(crate) fn to_rectangle(rect: Rect) -> Rectangle {
    Rectangle::new(Point::new(rect.x, rect.y), embedded_graphics::geometry::Size::new(rect.w, rect.h))
}

fn mask_opaque(mask: Option<&[u8]>, index: usize) -> bool {
    let Some(mask) = mask else { return true; };
    let byte = index >> 3;
    if byte >= mask.len() { return false; }
    let bit = 7 - (index & 7);
    mask[byte] & (1u8 << bit) != 0
}

fn rgb565_to_rgb888(raw: u16) -> Rgb888 {
    let r5 = ((raw >> 11) & 0x1f) as u8;
    let g6 = ((raw >> 5) & 0x3f) as u8;
    let b5 = (raw & 0x1f) as u8;
    Rgb888::new(
        (r5 << 3) | (r5 >> 2),
        (g6 << 2) | (g6 >> 4),
        (b5 << 3) | (b5 >> 2),
    )
}
