//! Explicit device-free fixtures using the production callback.

// Rust guideline compliant 2026-10-07
use super::*;
use ringbuf::HeapCons;
use std::sync::Weak;

/// A manually driven device callback; retaining it grants no output ownership.
pub struct Renderer {
    consumer: HeapCons<f32>,
    shared: Weak<Shared>,
    channels: usize,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl Renderer {
    /// Renders whole frames, returning false after output retirement or drop.
    ///
    /// # Errors
    /// Returns an error when the buffer ends within an interleaved frame.
    pub fn render(&mut self, samples: &mut [f32]) -> io::Result<bool> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "callback requires whole frames",
            ));
        }
        let Some(shared) = self.shared.upgrade() else {
            samples.fill(0.);
            return Ok(false);
        };
        render_audio(samples, &mut self.consumer, &shared);
        Ok(!shared.stopped.load(Ordering::SeqCst))
    }
}

/// Creates a bounded virtual device, initially paused with no prebuffer requirement.
///
/// # Errors
/// Returns an error for invalid format, zero generation, excessive storage, or
/// a ring that cannot hold an integral number of complete frames.
pub fn manual(
    format: Format,
    generation: u64,
    capacity: usize,
) -> io::Result<(Arc<Output>, Renderer)> {
    format.validate()?;
    if generation == 0
        || !(1..=MAX_RING_SAMPLES).contains(&capacity)
        || !capacity.is_multiple_of(usize::from(format.channels))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid virtual device capacity",
        ));
    }
    let mut output = Arc::try_unwrap(Output::test_output()).expect("new fixture is uniquely owned");
    let shared = Arc::get_mut(&mut output.shared).expect("fixture has no device callback");
    shared.format = format;
    shared
        .channel_generation
        .store(generation, Ordering::SeqCst);
    shared.ready_generation.store(generation, Ordering::SeqCst);
    shared.ring_capacity_samples = capacity as u64;
    shared.prebuffer_step_samples =
        samples_for_duration(format.sample_rate, format.channels, LIVE_PREBUFFER_STEP_US)
            .min(capacity as u64);
    shared.prebuffer_ceiling_samples = samples_for_duration(
        format.sample_rate,
        format.channels,
        PREBUFFER_MILLISECONDS * 1_000,
    )
    .min(capacity as u64);
    let (producer, consumer) = HeapRb::new(capacity).split();
    output.producer = Mutex::new(producer);
    output.sample_rate = format.sample_rate;
    output.channels = format.channels;
    let renderer = Renderer {
        consumer,
        shared: Arc::downgrade(&output.shared),
        channels: usize::from(format.channels),
    };
    Ok((Arc::new(output), renderer))
}
