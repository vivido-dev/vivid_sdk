//! Immutable RGBA storage with bounded decoder-buffer reuse.

// Rust guideline compliant 2026-10-07
use std::{
    ops::Deref,
    sync::{Mutex, Weak},
};

/// Retained RGBA pixels with optional bounded return to their decoder's free-buffer pool.
pub struct RgbaBuffer {
    bytes: Option<Vec<u8>>,
    decoder_pool: Option<Weak<Mutex<Vec<Vec<u8>>>>>,
}
impl std::fmt::Debug for RgbaBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RgbaBuffer")
            .field("bytes", &self.len())
            .finish_non_exhaustive()
    }
}

impl RgbaBuffer {
    /// Retains owned pixels without a decoder pool.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Some(bytes),
            decoder_pool: None,
        }
    }

    pub(crate) fn pooled(bytes: Vec<u8>, decoder_pool: Weak<Mutex<Vec<Vec<u8>>>>) -> Self {
        Self {
            bytes: Some(bytes),
            decoder_pool: Some(decoder_pool),
        }
    }
}

impl Deref for RgbaBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.bytes.as_deref().unwrap_or_default()
    }
}

impl AsRef<[u8]> for RgbaBuffer {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl AsMut<[u8]> for RgbaBuffer {
    fn as_mut(&mut self) -> &mut [u8] {
        self.bytes.as_deref_mut().expect("live RGBA storage")
    }
}

impl Drop for RgbaBuffer {
    fn drop(&mut self) {
        let Some(pool) = self.decoder_pool.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        let Some(bytes) = self.bytes.take() else {
            return;
        };
        let mut free = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if free.len() < 2 {
            free.push(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn rgba_buffers_return_to_a_bounded_decoder_pool() {
        let pool = Arc::new(Mutex::new(Vec::new()));
        for value in 0..3 {
            drop(RgbaBuffer::pooled(vec![value; 16], Arc::downgrade(&pool)));
        }
        let free = pool.lock().unwrap();
        assert_eq!(free.len(), 2, "a decoder retains at most two free frames");
        assert!(free.iter().all(|buffer| buffer.len() == 16));
    }
}
