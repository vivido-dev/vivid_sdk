//! Exact source positions for a bounded PCM output queue.

// Rust guideline compliant 2026-10-07
use std::io;

#[derive(Default)]
pub(super) struct Timeline {
    anchor: Option<i128>,
    queued: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Placement {
    pub gap: u64,
    pub skip: u64,
    pub first: bool,
}
impl Timeline {
    // Positions use microseconds times device rate. Keep the frame offset
    // exact, including a fractional-microsecond trimmed first sample.
    pub fn place(&mut self, origin: Option<i64>, offset: u64, rate: u32) -> io::Result<Placement> {
        let origin = origin.ok_or_else(|| super::invalid("native audio requires a decoded PTS"))?;
        let position = i128::from(origin) * i128::from(rate) + i128::from(offset) * 1_000_000;
        let first = self.anchor.is_none();
        let anchor = *self.anchor.get_or_insert(position);
        let delta = position - anchor;
        // Packet PTS has microsecond precision; rounding to the nearest device
        // frame avoids manufacturing a gap from a one-microsecond boundary.
        let frame = if delta >= 0 {
            (delta + 500_000) / 1_000_000
        } else {
            -((-delta + 500_000) / 1_000_000)
        };
        let distance = frame - i128::from(self.queued);
        let count = u64::try_from(distance.abs())
            .map_err(|_| super::invalid("audio placement exceeds frame budget"))?;
        Ok(Placement {
            gap: if distance > 0 { count } else { 0 },
            skip: if distance < 0 { count } else { 0 },
            first,
        })
    }
    // Advance only after the output channel has accepted these whole frames.
    pub fn advance(&mut self, frames: u64) -> io::Result<()> {
        self.queued = self
            .queued
            .checked_add(frames)
            .ok_or_else(|| super::invalid("audio queue timeline exhausted"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placements_preserve_gaps_and_skip_only_overlapping_new_samples() {
        let mut t = Timeline::default();
        assert!(t.place(Some(10_000), 0, 48_000).unwrap().first);
        t.advance(960).unwrap();
        assert_eq!(t.place(Some(50_000), 0, 48_000).unwrap().gap, 960);
        // A partial write advances only the actual prefix, not the whole gap.
        t.advance(240).unwrap();
        assert_eq!(t.place(Some(50_000), 0, 48_000).unwrap().gap, 720);
        t.advance(720 + 960).unwrap();
        assert_eq!(t.place(Some(60_000), 0, 48_000).unwrap().skip, 480);
        assert_eq!(t.place(Some(0), 0, 48_000).unwrap().skip, 3360);
    }

    #[test]
    fn exact_trim_offsets_and_timestamp_rounding_do_not_accumulate_drift() {
        let mut t = Timeline::default();
        t.place(Some(-1000), 3, 44_100).unwrap();
        for offset in 3..10_000 {
            let p = t.place(Some(-1000), offset, 44_100).unwrap();
            assert_eq!((p.gap, p.skip), (0, 0));
            t.advance(1).unwrap();
        }
        let mut t = Timeline::default();
        t.place(Some(0), 0, 44_100).unwrap();
        t.advance(127).unwrap();
        assert_eq!(
            t.place(Some(127_000_000 / 44_100), 0, 44_100).unwrap().gap,
            0
        );
    }

    #[test]
    fn unknown_timestamps_and_counter_exhaustion_are_explicit_errors() {
        let mut t = Timeline::default();
        assert!(t.place(None, 0, 48_000).is_err());
        assert!(t.anchor.is_none());
        t.advance(u64::MAX).unwrap();
        assert!(t.advance(1).is_err());
    }
}
