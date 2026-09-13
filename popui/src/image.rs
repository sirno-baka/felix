use std::{fs, io, vec, vec::Vec};

const DEFAULT_ALPHA_THRESHOLD: u8 = 127;

#[derive(Clone, Debug)]
enum Storage<T: 'static> {
    Static(&'static [T]),
    Owned(Vec<T>),
}

impl<T> Storage<T> {
    fn as_slice(&self) -> &[T] {
        match self {
            Self::Static(v) => v,
            Self::Owned(v) => v.as_slice(),
        }
    }
}

#[derive(Debug)]
pub enum ImageError {
    Io(io::Error),
    PngDecode,
    DimensionsTooLarge,
    InvalidPixelCount,
}

impl From<io::Error> for ImageError {
    fn from(value: io::Error) -> Self { Self::Io(value) }
}

/// RGB565 image used by PopUI widgets. PNG loading uses ordinary `std::fs`.
#[derive(Clone, Debug)]
pub struct Image {
    pub width: u16,
    pub height: u16,
    pixels: Storage<u16>,
    mask: Option<Storage<u8>>,
}

impl Image {
    pub const fn from_static(width: u16, height: u16, pixels: &'static [u16]) -> Self {
        Self { width, height, pixels: Storage::Static(pixels), mask: None }
    }

    pub const fn from_static_masked(
        width: u16,
        height: u16,
        pixels: &'static [u16],
        mask: &'static [u8],
    ) -> Self {
        Self { width, height, pixels: Storage::Static(pixels), mask: Some(Storage::Static(mask)) }
    }

    pub fn from_png_bytes(bytes: &[u8]) -> Result<Self, ImageError> {
        Self::from_png_bytes_with_alpha_threshold(bytes, DEFAULT_ALPHA_THRESHOLD)
    }

    pub fn from_png_bytes_with_alpha_threshold(bytes: &[u8], threshold: u8) -> Result<Self, ImageError> {
        let (header, rgba) = png_decoder::decode(bytes).map_err(|_| ImageError::PngDecode)?;
        if header.width == 0 || header.height == 0
            || header.width > u16::MAX as u32 || header.height > u16::MAX as u32
        {
            return Err(ImageError::DimensionsTooLarge);
        }
        let count = (header.width as usize)
            .checked_mul(header.height as usize)
            .ok_or(ImageError::DimensionsTooLarge)?;
        if rgba.len() != count { return Err(ImageError::InvalidPixelCount); }

        let mut pixels = Vec::with_capacity(count);
        let mut mask = vec![0u8; (count + 7) / 8];
        let mut needs_mask = false;
        for (index, [r, g, b, a]) in rgba.into_iter().enumerate() {
            pixels.push(rgb888_to_rgb565(r, g, b));
            if a > threshold {
                mask[index >> 3] |= 1u8 << (7 - (index & 7));
            } else {
                needs_mask = true;
            }
        }
        Ok(Self {
            width: header.width as u16,
            height: header.height as u16,
            pixels: Storage::Owned(pixels),
            mask: needs_mask.then_some(Storage::Owned(mask)),
        })
    }

    pub fn from_png_file(path: impl AsRef<std::path::Path>) -> Result<Self, ImageError> {
        let bytes = fs::read(path)?;
        Self::from_png_bytes(&bytes)
    }

    pub fn pixels(&self) -> &[u16] { self.pixels.as_slice() }
    pub fn mask(&self) -> Option<&[u8]> { self.mask.as_ref().map(Storage::as_slice) }
}

#[inline]
fn rgb888_to_rgb565(r: u8, g: u8, b: u8) -> u16 {
    (((r as u16) >> 3) << 11) | (((g as u16) >> 2) << 5) | ((b as u16) >> 3)
}
