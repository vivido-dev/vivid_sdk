//! Bounded raster clipboard data for an explicit user paste.

// Rust guideline compliant 2026-11-19
use crate::{ClipboardReadError, ImageFormat};
use image::{DynamicImage, ImageDecoder, ImageEncoder};
use std::{
    fmt,
    io::{self, Cursor, Write},
};

/// Maximum compressed input or normalized PNG bytes for one hosted image.
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 32 * 1024 * 1024;

/// Media carried by a single native clipboard offer.
pub enum ClipboardPasteMedia {
    /// Canonical local file URI list.
    Files(String),
    /// A bounded, newly encoded PNG with no external references.
    Image(ClipboardPng),
}

/// Owned pixel data normalized to PNG following size and geometry validation.
pub struct ClipboardPng(crate::Image);
impl fmt::Debug for ClipboardPng {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardPng")
            .field("bytes", &self.0.bytes.len())
            .finish()
    }
}
impl ClipboardPng {
    /// Decodes an explicitly typed bitmap and encodes its pixels into a bounded PNG.
    /// Call on a background worker.
    ///
    /// # Errors
    /// Rejects invalid images, formats and compressed or decoded size overruns.
    pub fn from_encoded(format: ImageFormat, bytes: &[u8]) -> Result<Self, ClipboardReadError> {
        let invalid = ClipboardReadError::UnsupportedContent;
        if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            return Err(invalid);
        }
        let format = match format {
            ImageFormat::Png => image::ImageFormat::Png,
            ImageFormat::Tiff => image::ImageFormat::Tiff,
            ImageFormat::Jpeg => image::ImageFormat::Jpeg,
            _ => return Err(invalid),
        };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(MAX_DECODED_BYTES);
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        reader.limits(limits);
        let mut decoder = reader.into_decoder().map_err(|_| invalid.clone())?;
        let (w, h) = decoder.dimensions();
        if w == 0
            || h == 0
            || u64::from(w) * u64::from(h) > MAX_DECODED_BYTES / 4
            || decoder.total_bytes() > MAX_DECODED_BYTES
        {
            return Err(invalid);
        }
        let orientation = decoder.orientation().map_err(|_| invalid.clone())?;
        let mut image = DynamicImage::from_decoder(decoder).map_err(|_| invalid.clone())?;
        image.apply_orientation(orientation);
        let pixels = image.into_rgba8();
        let mut output = LimitedPngWriter(Vec::new());
        image::codecs::png::PngEncoder::new(&mut output)
            .write_image(
                &pixels,
                pixels.width(),
                pixels.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|_| invalid)?;
        Ok(Self(crate::Image::from_bytes(ImageFormat::Png, output.0)))
    }
    /// Transfers the owned PNG bytes to the paste receiver.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0.bytes
    }
    /// Transfers this validated image to a native clipboard write.
    pub fn into_image(self) -> crate::Image {
        self.0
    }
}

struct LimitedPngWriter(Vec<u8>);
impl Write for LimitedPngWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES - self.0.len() {
            return Err(io::Error::other("clipboard image exceeds output limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
