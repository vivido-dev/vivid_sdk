//! Owned PCM with bounded timing metadata from decoded frames.

// Rust guideline compliant 2026-10-07
use std::{io, ops::Range};

/// A decoded PCM range and its exact position within one conversion timeline.
///
/// The sample rate belongs to this span. Timing is an integer microsecond
/// origin plus a device-frame offset, avoiding cumulative duration rounding.
/// Timeline serials are decoder-local observations, never source authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedSpan {
    origin: Option<i64>,
    offset: u64,
    timeline: u64,
    sample_offset: usize,
    sample_count: usize,
    rate: u32,
    channels: u16,
}

impl DecodedSpan {
    /// Returns this span's interleaved sample range within its batch.
    pub fn sample_range(&self) -> Range<usize> {
        self.sample_offset..self.sample_offset + self.sample_count
    }
    /// Returns the decoded timeline's source-timebase-us origin, when known.
    pub fn origin_pts_us(&self) -> Option<i64> {
        self.origin
    }
    /// Returns the exact device-frame offset from the timeline origin.
    pub fn frame_offset(&self) -> u64 {
        self.offset
    }
    /// Returns the number of complete device frames in this span.
    pub fn frames(&self) -> u64 {
        (self.sample_count / usize::from(self.channels)) as u64
    }
    /// Returns the decoder-local conversion timeline serial.
    pub fn timeline(&self) -> u64 {
        self.timeline
    }
    /// Returns the first sample's PTS, rounded down to microseconds.
    pub fn pts_us(&self) -> Option<i64> {
        position(self.origin, self.offset, self.rate).ok().flatten()
    }
    /// Returns the exclusive end PTS, rounded down to microseconds.
    pub fn end_pts_us(&self) -> Option<i64> {
        position(self.origin, self.offset + self.frames(), self.rate)
            .ok()
            .flatten()
    }
}

/// An owned interleaved PCM batch with complete, nonoverlapping timing ranges.
///
/// Ranges cover all returned samples in decode order. Unknown native timestamps
/// remain unknown until a later timeline obtains an anchor. Samples and timing
/// survive decoder retirement. Storage is bounded by the decoder's limits.
#[derive(Debug)]
pub struct DecodedBatch {
    pub(super) samples: Vec<f32>,
    spans: Vec<DecodedSpan>,
    rate: u32,
    channels: u16,
}
impl DecodedBatch {
    pub(super) fn new(rate: u32, channels: u16) -> Self {
        Self {
            samples: Vec::new(),
            spans: Vec::new(),
            rate,
            channels,
        }
    }
    /// Returns the batch's interleaved float samples.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    /// Returns bounded timing ranges covering the sample slice.
    pub fn spans(&self) -> &[DecodedSpan] {
        &self.spans
    }
    /// Returns device frames per second for this batch.
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    /// Returns the number of interleaved channels.
    pub fn channels(&self) -> u16 {
        self.channels
    }
    /// Discards timing metadata and transfers the sample allocation without copying.
    pub fn into_samples(self) -> Vec<f32> {
        self.samples
    }

    pub(super) fn append(
        &mut self,
        samples: &[f32],
        clock: &mut Clock,
        max_spans: usize,
    ) -> io::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        if !samples.len().is_multiple_of(usize::from(self.channels))
            || samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(super::invalid("decoded audio is not finite whole frames"));
        }
        if self.spans.len() >= max_spans {
            return Err(super::invalid("audio timing exceeds frame budget"));
        }
        let frames = (samples.len() / usize::from(self.channels)) as u64;
        let end = clock
            .output_frames
            .checked_add(frames)
            .ok_or_else(|| super::invalid("audio timeline exhausted"))?;
        position(clock.origin, end, self.rate)?;
        self.spans.try_reserve_exact(1).map_err(io::Error::other)?;
        self.samples
            .try_reserve_exact(samples.len())
            .map_err(io::Error::other)?;
        self.spans.push(DecodedSpan {
            origin: clock.origin,
            offset: clock.output_frames,
            timeline: clock.timeline,
            sample_offset: self.samples.len(),
            sample_count: samples.len(),
            rate: self.rate,
            channels: self.channels,
        });
        self.samples.extend_from_slice(samples);
        clock.output_frames = end;
        Ok(())
    }

    pub(super) fn trim(&mut self, start: &mut usize, end: &mut usize) -> io::Result<()> {
        self.trim_from(0, start, end)
    }

    // A previous filter tail in the prefix belongs to the previous packet;
    // current packet trim must not remove or retimestamp those samples.
    pub(super) fn trim_from(
        &mut self,
        prefix: usize,
        start: &mut usize,
        end: &mut usize,
    ) -> io::Result<()> {
        let head = (*start).min(self.samples.len() - prefix);
        let tail = (*end).min(self.samples.len() - prefix - head);
        let through = self.samples.len() - tail;
        let mut written = 0;
        for index in 0..self.spans.len() {
            let mut span = self.spans[index];
            let protected = span.sample_offset + span.sample_count <= prefix;
            let from = if protected {
                span.sample_offset
            } else {
                span.sample_offset.max(prefix + head)
            };
            let to = (span.sample_offset + span.sample_count).min(through);
            if from >= to {
                continue;
            }
            let frames = (from - span.sample_offset) / usize::from(self.channels);
            span.offset = span
                .offset
                .checked_add(frames as u64)
                .ok_or_else(|| super::invalid("audio trim timeline exhausted"))?;
            span.sample_offset = from - if protected { 0 } else { head };
            span.sample_count = to - from;
            self.spans[written] = span;
            written += 1;
        }
        self.spans.truncate(written);
        self.samples.truncate(through);
        self.samples.drain(prefix..prefix + head);
        *start -= head;
        *end -= tail;
        Ok(())
    }
}

pub(super) struct Clock {
    pub timeline: u64,
    pub origin: Option<i64>,
    pub input_frames: u64,
    pub output_frames: u64,
}
impl Clock {
    pub fn follows(&self, pts: Option<i64>, input_rate: u32) -> io::Result<bool> {
        match (self.origin, pts) {
            (Some(origin), Some(pts)) => Ok(position(Some(origin), self.input_frames, input_rate)?
                .is_some_and(|expected| expected.abs_diff(pts) <= 1)),
            (None, Some(_)) => Ok(false),
            (_, None) => Ok(true),
        }
    }
}

fn position(origin: Option<i64>, frames: u64, rate: u32) -> io::Result<Option<i64>> {
    origin
        .map(|origin| {
            let elapsed = i128::from(frames) * 1_000_000 / i128::from(rate);
            i64::try_from(i128::from(origin) + elapsed)
                .map_err(|_| super::invalid("decoded audio timestamp exceeds source timebase"))
        })
        .transpose()
}
