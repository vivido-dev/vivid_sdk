//! Exact PLAY placement for PCM queued before or after playback admission.

// Rust guideline compliant 2026-10-07
use super::*;

#[derive(Default)]
pub(super) struct QueueTimeline {
    // Microseconds times output rate, including the exact first frame offset.
    anchor: Option<i128>,
    start: Option<i64>,
}

fn invalid_queue(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn alignment(anchor: i128, start: i64, format: Format) -> io::Result<(u64, u64)> {
    let delta = i128::from(start) * i128::from(format.sample_rate) - anchor;
    let samples = ((delta.abs() + 999_999) / 1_000_000) * i128::from(format.channels);
    let samples =
        u64::try_from(samples).map_err(|_| invalid_queue("audio PLAY position exhausted"))?;
    Ok(if delta >= 0 {
        (samples, 0)
    } else {
        (0, samples)
    })
}

impl OutputControl {
    fn queue_plan(&self, queue: &QueueTimeline, start: i64, minimum: u64) -> io::Result<u64> {
        if self.shared.stopped.load(Ordering::SeqCst) {
            return Err(retired_channel());
        }
        let samples = (u128::from(self.shared.format.sample_rate) * u128::from(minimum))
            .div_ceil(1_000_000)
            * u128::from(self.shared.format.channels);
        if start == UNSET_PTS || samples > u128::from(self.shared.ring_capacity_samples) {
            return Err(invalid_queue("audio PLAY exceeds output capacity"));
        }
        if let Some(initial) = queue.start {
            let expected = self.clock_pts().unwrap_or(initial);
            if start != expected {
                return Err(invalid_queue(
                    "audio reposition requires a fresh output timeline",
                ));
            }
        } else if let Some(anchor) = queue.anchor {
            alignment(anchor, start, self.shared.format)?;
        }
        Ok(samples as u64)
    }

    /// Validates exact PLAY placement for a timestamped PCM queue.
    ///
    /// No device query or state mutation occurs. Initial PLAY may precede PCM;
    /// resume keeps the established timeline and requires its current clock PTS.
    /// A reposition requires retiring this output and creating a new timeline.
    ///
    /// # Errors
    /// Returns an error for retirement, unknown PTS, unreachable prebuffer,
    /// exhausted position arithmetic, or repositioning an established timeline.
    pub fn validate_timed_queue(
        &self,
        start_pts_us: i64,
        minimum_buffer_us: u64,
    ) -> io::Result<()> {
        let queue = self
            .shared
            .queue_timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.queue_plan(&queue, start_pts_us, minimum_buffer_us)
            .map(|_| ())
    }

    /// Configures exact timed playback of already staged PCM without starting it.
    ///
    /// Initial PLAY discards queued and future PCM before its exact start. Known
    /// leading silence contributes to the prebuffer horizon. Resume preserves
    /// both queued samples and the original clock; it needs no new media packet.
    /// This handle retains no device and invokes no native device operation.
    ///
    /// # Errors
    /// Returns validation errors before mutation, as [`Self::validate_timed_queue`].
    pub fn configure_timed_queue(
        &self,
        start_pts_us: i64,
        minimum_buffer_us: u64,
    ) -> io::Result<()> {
        let mut queue = self
            .shared
            .queue_timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let minimum = self.queue_plan(&queue, start_pts_us, minimum_buffer_us)?;
        self.pause();
        if queue.start.is_none() {
            queue.start = Some(start_pts_us);
            self.shared
                .timeline_origin_us
                .store(start_pts_us, Ordering::SeqCst);
            if let Some(anchor) = queue.anchor {
                self.align_queue(anchor, start_pts_us)?;
            }
        }
        self.shared.staged_timed.store(true, Ordering::SeqCst);
        self.shared.timed.store(true, Ordering::SeqCst);
        self.shared
            .requested_start_pts_us
            .store(start_pts_us, Ordering::SeqCst);
        self.shared
            .prebuffer_samples
            .store(minimum, Ordering::SeqCst);
        self.shared.prebuffered.store(false, Ordering::SeqCst);
        self.shared.trim_to_prebuffer.store(false, Ordering::SeqCst);
        self.shared
            .resume_waiting_for_audio
            .store(false, Ordering::SeqCst);
        Ok(())
    }

    fn align_queue(&self, anchor: i128, start: i64) -> io::Result<()> {
        let (skip, leading) = alignment(anchor, start, self.shared.format)?;
        self.shared
            .timeline_base_samples
            .store(skip, Ordering::SeqCst);
        self.shared
            .discard_through_samples
            .store(skip, Ordering::SeqCst);
        self.shared
            .leading_silence_samples
            .store(leading, Ordering::SeqCst);
        Ok(())
    }

    fn observe_timed_queue(&self, origin: i64, frame_offset: u64) -> io::Result<()> {
        if origin == UNSET_PTS {
            return Err(invalid_queue("audio queue requires a known origin"));
        }
        let rate = i128::from(self.shared.format.sample_rate);
        let anchor = i128::from(origin) * rate + i128::from(frame_offset) * 1_000_000;
        let pts = i64::try_from(anchor.div_euclid(rate))
            .map_err(|_| invalid_queue("audio queue position exhausted"))?;
        let mut queue = self
            .shared
            .queue_timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(old) = queue.anchor {
            return if old == anchor {
                Ok(())
            } else {
                Err(invalid_queue("audio queue origin cannot change"))
            };
        }
        if let Some(start) = queue.start {
            self.align_queue(anchor, start)?;
        }
        queue.anchor = Some(anchor);
        self.shared.staged_timed.store(true, Ordering::SeqCst);
        self.shared.timed.store(true, Ordering::SeqCst);
        // Publish after the exact alignment so callbacks never acquire a new
        // anchor with an old pre-roll or leading-silence count.
        self.shared.first_audio_pts_us.store(pts, Ordering::SeqCst);
        Ok(())
    }

    /// Returns actual PCM frames rendered, excluding underrun and leading silence.
    pub fn presented_frames(&self) -> u64 {
        self.shared.presented_samples.load(Ordering::SeqCst)
            / u64::from(self.shared.format.channels)
    }

    /// Returns the source PTS after the last actually rendered PCM frame.
    ///
    /// Leading/underrun silence and discarded PCM cannot advance this position.
    pub fn presented_pts(&self) -> Option<i64> {
        let origin = self.shared.timeline_origin_us.load(Ordering::SeqCst);
        (origin != UNSET_PTS && self.shared.presented_samples.load(Ordering::SeqCst) != 0).then(
            || {
                rendered_pts_us(
                    origin,
                    self.shared.last_presented_position.load(Ordering::SeqCst),
                    self.shared.format.sample_rate,
                    self.shared.format.channels,
                )
            },
        )
    }
}

impl Channel {
    /// Anchors a staged timed queue at an exact decoded output-frame position.
    ///
    /// Call once before its first PCM submission. The origin uses microseconds;
    /// the frame offset uses this output's rate. No cumulative rounding occurs.
    ///
    /// # Errors
    /// Returns an error for retirement, unknown/exhausted position, or a changed
    /// origin. Alignment is validated before publishing the first anchor.
    pub fn observe_timed_queue(&self, origin_pts_us: i64, frame_offset: u64) -> io::Result<()> {
        self.with_current(|output| {
            output
                .control()
                .observe_timed_queue(origin_pts_us, frame_offset)
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device() -> (Arc<Output>, testing::Renderer) {
        testing::manual(
            Format {
                sample_rate: 48_000,
                channels: 2,
            },
            1,
            12_000,
        )
        .unwrap()
    }
    fn pcm(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| [(n + 1) as f32 / (frames + 1) as f32; 2])
            .collect()
    }
    #[test]
    fn initial_play_discards_prequeued_samples_before_its_exact_start() {
        let (output, mut callback) = device();
        let channel = output.channel(1).unwrap();
        channel.observe_timed_queue(1_000_000, 0).unwrap();
        channel
            .try_push(&[0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4])
            .unwrap();
        channel.finish_decode().unwrap();
        channel.signal_eos().unwrap();
        output
            .control()
            .configure_timed_queue(1_000_041, 0)
            .unwrap();
        output.start();
        let mut rendered = [1.; 8];
        callback.render(&mut rendered).unwrap();
        assert_eq!(rendered, [0.3, 0.3, 0.4, 0.4, 0., 0., 0., 0.]);
        assert_eq!(output.clock_pts(), Some(1_000_082));
        assert_eq!(output.control().presented_frames(), 2);
        assert!(output.poll_drained().unwrap().is_ok());
    }
    #[test]
    fn leading_silence_is_prebuffer_horizon_in_both_play_media_orders() {
        for play_first in [true, false] {
            let (output, mut callback) = device();
            let channel = output.channel(1).unwrap();
            if play_first {
                output.control().configure_timed_queue(0, 20_000).unwrap();
            }
            channel.observe_timed_queue(20_000, 0).unwrap();
            channel.try_push(&[0.25; 2]).unwrap();
            if !play_first {
                output.control().configure_timed_queue(0, 20_000).unwrap();
            }
            assert!(output.ready_to_start());
            output.start();
            let mut silence = [1.; 1920];
            callback.render(&mut silence).unwrap();
            assert_eq!(silence, [0.; 1920]);
            assert_eq!(output.clock_pts(), Some(20_000));
            assert_eq!(output.control().presented_frames(), 0);
            channel.finish_decode().unwrap();
            channel.signal_eos().unwrap();
            let mut sample = [0.; 2];
            callback.render(&mut sample).unwrap();
            assert_eq!(sample, [0.25; 2]);
            assert_eq!(output.control().presented_frames(), 1);
        }
    }
    #[test]
    fn exact_decoded_frame_offsets_do_not_overtrim_a_fractional_microsecond() {
        let (output, mut callback) = device();
        let channel = output.channel(1).unwrap();
        channel.observe_timed_queue(1_000, 2).unwrap();
        channel.try_push(&[0.1, 0.1, 0.2, 0.2, 0.3, 0.3]).unwrap();
        output.control().configure_timed_queue(1_062, 0).unwrap();
        output.start();
        let mut rendered = [0.; 2];
        callback.render(&mut rendered).unwrap();
        assert_eq!(rendered, [0.2; 2]);
    }
    #[test]
    fn pause_resume_uses_retained_pcm_without_waiting_for_another_packet() {
        let (output, mut callback) = device();
        let channel = output.channel(1).unwrap();
        channel.observe_timed_queue(0, 0).unwrap();
        channel.try_push(&[0.25; 200]).unwrap();
        let control = output.control();
        control.configure_timed_queue(0, 0).unwrap();
        control.start();
        callback.render(&mut [0.; 20]).unwrap();
        let paused = control.clock_pts().unwrap();
        control.pause();
        let mut silent = [1.; 20];
        callback.render(&mut silent).unwrap();
        assert_eq!(silent, [0.; 20]);
        assert_eq!(control.clock_pts(), Some(paused));
        control.configure_timed_queue(paused, 100).unwrap();
        control.start();
        let mut retained = [0.; 20];
        callback.render(&mut retained).unwrap();
        assert_eq!(retained, [0.25; 20]);
        assert_eq!(control.clock_pts(), Some(416));
    }
    #[test]
    fn underrun_advances_the_clock_and_late_pcm_never_becomes_presentation() {
        let (output, mut callback) = device();
        let channel = output.channel(1).unwrap();
        let control = output.control();
        control.configure_timed_queue(0, 0).unwrap();
        control.start();
        callback.render(&mut [0.; 480]).unwrap();
        assert_eq!(control.clock_pts(), Some(5_000));
        assert_eq!(control.presented_frames(), 0);
        channel.observe_timed_queue(0, 0).unwrap();
        let samples = pcm(960);
        channel.try_push(&samples).unwrap();
        let mut rendered = [0.; 2];
        callback.render(&mut rendered).unwrap();
        assert_eq!(rendered, samples[480..482]);
        assert_eq!(control.presented_frames(), 1);
        callback.render(&mut [0.; 2000]).unwrap();
        assert_eq!(control.presented_frames(), 720);
        assert_eq!(control.presented_pts(), Some(20_000));
        assert_eq!(control.clock_pts(), Some(25_854));
    }
    #[test]
    fn pre_roll_before_start_does_not_satisfy_the_requested_buffer() {
        let (output, mut callback) = device();
        let channel = output.channel(1).unwrap();
        channel.observe_timed_queue(0, 0).unwrap();
        channel.try_push(&[0.25; 200]).unwrap();
        output
            .control()
            .configure_timed_queue(100_000, 1_000)
            .unwrap();
        assert!(!output.ready_to_start());
        output.start();
        let mut waiting = [1.; 2];
        callback.render(&mut waiting).unwrap();
        assert_eq!(waiting, [0.; 2]);
        assert_eq!(output.clock_pts(), Some(100_000));
        channel.try_push(&[0.5; 9800]).unwrap();
        callback.render(&mut waiting).unwrap();
        assert_eq!(waiting, [0.5; 2]);
    }
    #[test]
    fn invalid_play_and_anchor_changes_leave_the_accepted_timeline_intact() {
        let (output, _) = device();
        let channel = output.channel(1).unwrap();
        let control = output.control();
        channel.observe_timed_queue(0, 0).unwrap();
        control.configure_timed_queue(0, 10_000).unwrap();
        for (start, minimum) in [(i64::MIN, 0), (1, 0), (0, 1_000_000)] {
            assert!(control.validate_timed_queue(start, minimum).is_err());
            assert!(control.configure_timed_queue(start, minimum).is_err());
            assert_eq!(control.clock_pts(), Some(0));
        }
        assert!(channel.observe_timed_queue(1, 0).is_err());
        control.configure_timed_queue(0, 10_000).unwrap();
        control.stop();
        assert!(control.configure_timed_queue(0, 0).is_err());
    }
    #[test]
    fn extreme_position_arithmetic_fails_before_publishing_a_new_play() {
        let (output, _) = testing::manual(
            Format {
                sample_rate: 384_000,
                channels: 32,
            },
            1,
            32,
        )
        .unwrap();
        let channel = output.channel(1).unwrap();
        channel.observe_timed_queue(i64::MIN + 1, 0).unwrap();
        assert!(output.control().configure_timed_queue(i64::MAX, 0).is_err());
        assert_eq!(output.clock_pts(), None);
        output
            .control()
            .configure_timed_queue(i64::MIN + 1, 0)
            .unwrap();
        assert_eq!(output.clock_pts(), Some(i64::MIN + 1));
    }
}
