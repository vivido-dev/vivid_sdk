//! Shared allocation limits for CPU-readable GUI frames.

// Rust guideline compliant 2026-10-07
use crate::{DevicePixels, Size};

// A 4K RGBA frame fits comfortably; larger captures require explicit tiling.
// This bounds each image, not the aggregate resources of an application host.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Validates an offscreen extent and returns its packed RGBA byte length.
///
/// Call before allocating textures or readback buffers. Driver texture limits
/// may impose a smaller maximum. Each frame is limited to 64 MiB.
///
/// # Errors
/// Returns an error for nonpositive, overflowing, or oversized dimensions.
pub fn validate_offscreen_size(size: Size<DevicePixels>) -> anyhow::Result<usize> {
    let bytes = usize::try_from(size.width.0)
        .ok()
        .filter(|width| *width > 0)
        .zip(
            usize::try_from(size.height.0)
                .ok()
                .filter(|height| *height > 0),
        )
        .and_then(|(width, height)| width.checked_mul(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
        .ok_or_else(|| anyhow::anyhow!("offscreen extent exceeds the RGBA frame limits"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_before_allocation() {
        let size = |width, height| Size::new(DevicePixels(width), DevicePixels(height));
        assert_eq!(
            validate_offscreen_size(size(3840, 2160)).unwrap(),
            33_177_600
        );
        assert_eq!(
            validate_offscreen_size(size(4096, 4096)).unwrap(),
            MAX_FRAME_BYTES
        );
        for dimensions in [
            size(0, 1),
            size(1, 0),
            size(-1, 2),
            size(4097, 4096),
            size(i32::MAX, i32::MAX),
        ] {
            assert!(validate_offscreen_size(dimensions).is_err());
        }
    }
}
