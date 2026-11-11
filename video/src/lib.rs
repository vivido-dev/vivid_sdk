//! Native elementary-video decoding without a GUI, presenter, or playback clock.
//!
//! Hosts validate and authenticate their media first, then use one decoder on its
//! owning media worker. Returned RGBA frames retain their timestamps and recycle
//! at most two free allocations after all scene references release them. This
//! library does not schedule presentation, grant input, or acknowledge display.
//!
//! The optional SDK `video-decode` feature exposes this crate as `vivid_sdk::video`.
//! Native FFmpeg development libraries are required only when that feature is used.

// Rust guideline compliant 2026-10-07
mod buffer;
mod decoder;

/// Verified low-level ABI for the existing native audio adapter.
///
/// This is an unsafe integration escape hatch; applications should use [`Decoder`].
#[doc(hidden)]
pub mod ffi;

pub use buffer::RgbaBuffer;
pub use decoder::{DecodedFrame, Decoder, Limits};
