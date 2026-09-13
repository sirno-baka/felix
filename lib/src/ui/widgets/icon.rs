use crate::fs::{File, IoError};
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use alloc::{vec, vec::Vec};
use embedded_graphics::{
    pixelcolor::Rgb888,
    prelude::*,
    Pixel,
};
use taffy::geometry::Size as TSize;

const DEFAULT_ALPHA_THRESHOLD: u8 = 127;

#[derive(Clone, Debug)]
enum ImageStorage<T: 'static> {
    Static(&'static [T]),
    Owned(Vec<T>),
}

impl<T> ImageStorage<T> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        match self {
            Self::Static(v) => v,
            Self::Owned(v) => v.as_slice(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconImageError {
    Io(IoError),
    PngDecode,
    DimensionsTooLarge,
    InvalidPixelCount,
}

/// RGB565 image used by [`Icon`].
///
/// Images may either reference static RGB565 data or own pixels decoded from a PNG.
/// Transparency is stored as a packed 1-bit mask: bit 1 = draw pixel,
/// bit 0 = transparent. Bits are consumed from MSB to LSB in each byte.
#[derive(Clone, Debug)]
pub struct IconImage {
    pub width: u16,
    pub height: u16,
    pixels: ImageStorage<u16>,
    mask: Option<ImageStorage<u8>>,
}

impl IconImage {
    /// Create an icon from static RGB565 pixels without transparency.
    pub const fn new(width: u16, height: u16, pixels: &'static [u16]) -> Self {
        Self {
            width,
            height,
            pixels: ImageStorage::Static(pixels),
            mask: None,
        }
    }

    /// Create an icon from static RGB565 pixels and a packed 1-bit opacity mask.
    pub const fn with_mask(
        width: u16,
        height: u16,
        pixels: &'static [u16],
        mask: &'static [u8],
    ) -> Self {
        Self {
            width,
            height,
            pixels: ImageStorage::Static(pixels),
            mask: Some(ImageStorage::Static(mask)),
        }
    }

    /// Decode a PNG already present in memory.
    ///
    /// This is intended for embedded resources:
    ///
    /// ```ignore
    /// let image = IconImage::from_png_bytes(include_bytes!("folder.png"))?;
    /// let icon = ui.icon_image(parent, image);
    /// ```
    ///
    /// PNG pixels are converted to RGB565 once during decoding. Since the current
    /// Felix icon renderer has binary transparency, PNG alpha values > 127 are
    /// treated as opaque and values <= 127 as transparent.
    pub fn from_png_bytes(bytes: &[u8]) -> Result<Self, IconImageError> {
        Self::from_png_bytes_with_alpha_threshold(bytes, DEFAULT_ALPHA_THRESHOLD)
    }

    /// Same as [`Self::from_png_bytes`], but allows choosing the alpha cutoff.
    /// Pixels with alpha strictly greater than `alpha_threshold` are visible.
    pub fn from_png_bytes_with_alpha_threshold(
        bytes: &[u8],
        alpha_threshold: u8,
    ) -> Result<Self, IconImageError> {
        let (header, rgba) = png_decoder::decode(bytes).map_err(|_| IconImageError::PngDecode)?;

        if header.width == 0
            || header.height == 0
            || header.width > u16::MAX as u32
            || header.height > u16::MAX as u32
        {
            return Err(IconImageError::DimensionsTooLarge);
        }

        let pixel_count = (header.width as usize)
            .checked_mul(header.height as usize)
            .ok_or(IconImageError::DimensionsTooLarge)?;

        if rgba.len() != pixel_count {
            return Err(IconImageError::InvalidPixelCount);
        }

        let mut pixels = Vec::with_capacity(pixel_count);
        let mut mask = vec![0u8; (pixel_count + 7) / 8];
        let mut needs_mask = false;

        for (index, [r, g, b, a]) in rgba.into_iter().enumerate() {
            pixels.push(rgb888_to_rgb565(r, g, b));

            if a > alpha_threshold {
                let byte = index >> 3;
                let bit = 7 - (index & 7);
                mask[byte] |= 1u8 << bit;
            } else {
                needs_mask = true;
            }
        }

        Ok(Self {
            width: header.width as u16,
            height: header.height as u16,
            pixels: ImageStorage::Owned(pixels),
            mask: if needs_mask {
                Some(ImageStorage::Owned(mask))
            } else {
                None
            },
        })
    }

    /// Read and decode a PNG from the Felix VFS.
    ///
    /// ```ignore
    /// let image = IconImage::from_png_file("/icons/folder.png")?;
    /// let icon = ui.icon_image(parent, image);
    /// ```
    pub fn from_png_file(path: &str) -> Result<Self, IconImageError> {
        Self::from_png_file_with_alpha_threshold(path, DEFAULT_ALPHA_THRESHOLD)
    }

    /// Read a PNG from the Felix VFS with a custom binary alpha cutoff.
    pub fn from_png_file_with_alpha_threshold(
        path: &str,
        alpha_threshold: u8,
    ) -> Result<Self, IconImageError> {
        let mut file = File::open_ro(path).map_err(IconImageError::Io)?;
        let bytes = file.read_to_end().map_err(IconImageError::Io)?;
        Self::from_png_bytes_with_alpha_threshold(&bytes, alpha_threshold)
    }

    #[inline]
    pub const fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    #[inline]
    pub fn pixels(&self) -> &[u16] {
        self.pixels.as_slice()
    }

    #[inline]
    pub fn mask(&self) -> Option<&[u8]> {
        self.mask.as_ref().map(ImageStorage::as_slice)
    }

    #[inline]
    fn opaque(&self, index: usize) -> bool {
        let Some(mask) = self.mask() else {
            return true;
        };
        let byte = index >> 3;
        if byte >= mask.len() {
            return false;
        }
        let bit = 7 - (index & 7);
        (mask[byte] & (1u8 << bit)) != 0
    }
}

enum IconSource {
    Static(&'static IconImage),
    Owned(IconImage),
}

impl IconSource {
    #[inline]
    fn image(&self) -> &IconImage {
        match self {
            Self::Static(image) => image,
            Self::Owned(image) => image,
        }
    }
}

/// Lightweight image widget intended for small UI icons (16x16, 24x24, 32x32).
///
/// Static images can be shared without allocation. PNG-decoded images may instead
/// be moved into the widget with [`Icon::from_image`].
pub struct Icon {
    image: IconSource,
    rect: Rect,
    dirty: bool,
}

impl Icon {
    pub const fn new(image: &'static IconImage) -> Self {
        Self {
            image: IconSource::Static(image),
            rect: Rect::new(0, 0, 0, 0),
            dirty: true,
        }
    }

    pub fn from_image(image: IconImage) -> Self {
        Self {
            image: IconSource::Owned(image),
            rect: Rect::new(0, 0, 0, 0),
            dirty: true,
        }
    }

    #[inline]
    pub fn image(&self) -> &IconImage {
        self.image.image()
    }

    pub fn set_image(&mut self, image: &'static IconImage) {
        let changed = match &self.image {
            IconSource::Static(old) => !core::ptr::eq(*old, image),
            IconSource::Owned(_) => true,
        };

        if changed {
            self.image = IconSource::Static(image);
            self.dirty = true;
        }
    }

    pub fn set_owned_image(&mut self, image: IconImage) {
        self.image = IconSource::Owned(image);
        self.dirty = true;
    }
}

impl Widget for Icon {
    fn measure(&self, constraints: Constraints) -> TSize<f32> {
        let image = self.image();
        constraints.clamp(TSize {
            width: image.width as f32,
            height: image.height as f32,
        })
    }

    fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn draw(&self, win: &mut crate::wm::Window) {
        let image = self.image();
        if self.rect.w == 0 || self.rect.h == 0 || image.width == 0 || image.height == 0 {
            return;
        }

        let width = image.width as usize;
        let height = image.height as usize;
        let source_pixels = image.pixels();
        let count = image.pixel_count().min(source_pixels.len());

        let origin_x = self.rect.x + (self.rect.w as i32 - image.width as i32) / 2;
        let origin_y = self.rect.y + (self.rect.h as i32 - image.height as i32) / 2;

        let pixels = source_pixels
            .iter()
            .copied()
            .take(count)
            .enumerate()
            .filter_map(|(index, raw)| {
                if !image.opaque(index) {
                    return None;
                }

                let x = index % width;
                let y = index / width;
                if y >= height {
                    return None;
                }

                Some(Pixel(
                    Point::new(origin_x + x as i32, origin_y + y as i32),
                    rgb565_to_rgb888(raw),
                ))
            });

        let _ = win.draw_iter(pixels);
    }

    fn event(&mut self, _event: &UiEvent, _focused: bool) -> EventResult {
        EventResult::Ignored
    }

    fn dirty(&self) -> bool {
        self.dirty
    }

    fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

#[inline]
fn rgb888_to_rgb565(r: u8, g: u8, b: u8) -> u16 {
    (((r as u16) >> 3) << 11) | (((g as u16) >> 2) << 5) | ((b as u16) >> 3)
}

#[inline]
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
