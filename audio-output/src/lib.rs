//! Bounded native audio output and source-scoped device clocks.
//!
//! Open on a dedicated media/device worker after host admission. This module
//! owns neither protocol authentication nor a GUI/event loop. One output belongs
//! to one admitted source; channel handles cannot cross that output instance.

#![forbid(unsafe_code)]

// Rust guideline compliant 2026-10-07
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::io;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, I24, SampleFormat, SizedSample, Stream, StreamConfig, U24};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use vivid_protocol::track::AudioGain;

mod timed_queue;

// Remote audio commonly arrives in bursts after SSH scheduling or network jitter. Keep a bounded
// reserve large enough to bridge those stalls; linked video is discarded when it falls behind.
const RING_BUFFER_SECONDS: usize = 2;
const PREBUFFER_MILLISECONDS: u64 = 100;
/// The least a live track may ask to be buffered before the device starts draining.
///
/// Whatever the ring holds when playback begins is delay the listener keeps for the rest of the
/// stream, so a live producer's declared target latency is honored — but a couple of device
/// periods have to stay in hand or the very first callback underruns and every one after it
/// chases the producer.
const MINIMUM_LIVE_PREBUFFER_US: u64 = 20_000;
/// How much reserve a live track earns back each time its declared target proved too tight.
///
/// A declared target is the producer's estimate of its own link. It cannot know what the link
/// actually does, and a reserve that is too shallow does not fail gracefully: the ring runs dry,
/// the device renders silence the producer never sent, and nothing in a live stream ever refills
/// it. Growing on the evidence — an underrun that really happened — costs the tight target only on
/// links that could not hold it, and is bounded by [`PREBUFFER_MILLISECONDS`], so the worst case is
/// the reserve this presenter would have used anyway.
const LIVE_PREBUFFER_STEP_US: u64 = 20_000;
/// The most a live stream will be delayed to line sound up with picture. Past this the session is
/// no longer interactive, and the honest answer is a lower encoder target, not more buffering.
///
/// Held audio accumulates in the device ring, so this must stay comfortably under
/// [`RING_BUFFER_SECONDS`]: steady-state ring occupancy is the delay itself, and a delay that
/// could reach the ring's capacity would block the decoder thread pushing into it.
const MAXIMUM_LIVE_DELAY_US: u64 = 1_000_000;
/// Delay changes smaller than this are transport jitter, not skew worth re-buffering for.
const LIVE_DELAY_STEP_US: u64 = 20_000;
const LINKED_AUDIO_STALL_FALLBACK: Duration = Duration::from_secs(2);
/// How much of a full ring buffer to wait for the device to drain before retrying a push.
const RING_WAIT_SAMPLES: u64 = 512;
/// A floor on that wait, so a very high sample rate cannot turn it back into a spin.
const MINIMUM_RING_WAIT: Duration = Duration::from_micros(500);
/// How often to re-check an output that is not draining, so `stop` and errors are still noticed.
const STOPPED_OUTPUT_RECHECK: Duration = Duration::from_millis(5);
const UNSET_PTS: i64 = i64::MIN;

// Cap one device ring at 64 MiB of interleaved f32 samples.
const MAX_RING_SAMPLES: usize = 16 * 1024 * 1024;

/// Interleaved device format used by the producer and output callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// Device frames per second, from 8,000 through 384,000.
    pub sample_rate: u32,
    /// Interleaved channels, from one through 32.
    pub channels: u16,
}

impl Format {
    fn validate(self) -> io::Result<()> {
        if !(8_000..=384_000).contains(&self.sample_rate) || !(1..=32).contains(&self.channels) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported output format",
            ));
        }
        Ok(())
    }
}

/// Ring storage limits validated before device buffering is allocated.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Interleaved sample ceiling, at most 16,777,216 (64 MiB).
    pub max_ring_samples: usize,
    /// Ring duration, from one microsecond through two seconds.
    pub buffer_duration_us: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_ring_samples: MAX_RING_SAMPLES,
            buffer_duration_us: RING_BUFFER_SECONDS as u64 * 1_000_000,
        }
    }
}

impl Limits {
    /// Computes an admitted ring size without opening a device.
    ///
    /// # Errors
    /// Returns an error for invalid format/limits or a capacity beyond the sample ceiling.
    pub fn capacity_for(self, format: Format) -> io::Result<usize> {
        format.validate()?;
        if !(1..=MAX_RING_SAMPLES).contains(&self.max_ring_samples)
            || !(1..=2_000_000).contains(&self.buffer_duration_us)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid output ring limits",
            ));
        }
        u64::from(format.sample_rate)
            .checked_mul(self.buffer_duration_us)
            .map(|n| n.div_ceil(1_000_000))
            .and_then(|frames| frames.checked_mul(u64::from(format.channels)))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= self.max_ring_samples)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "output ring exceeds sample budget",
                )
            })
    }
}

struct Shared {
    channel_generation: AtomicU64,
    // Retirement is visible before the transition lock is acquired. The new
    // channel may submit only after that transition's flush has completed.
    ready_generation: AtomicU64,
    generation_transition: Mutex<()>,
    enabled: AtomicBool,
    callback_active: AtomicBool,
    #[cfg(test)]
    before_callback_commit: Mutex<Option<CallbackGate>>,
    prebuffered: AtomicBool,
    stopped: AtomicBool,
    decode_done: AtomicBool,
    eos_observed: AtomicBool,
    queued_samples: AtomicU64,
    played_samples: AtomicU64,
    /// Absolute queued-sample watermark that a timeline reset must discard through.
    ///
    /// A relative outstanding count races the realtime callback: it can snapshot a sample after
    /// the callback popped it but before `played_samples` was published, then discard one sample
    /// from the replacement timeline. An absolute watermark cannot cross that boundary.
    discard_through_samples: AtomicU64,
    discard_samples: AtomicU64,
    rendered_samples: AtomicU64,
    presented_samples: AtomicU64,
    last_presented_position: AtomicU64,
    staged_timed: AtomicBool,
    queue_timeline: Mutex<timed_queue::QueueTimeline>,
    timed: AtomicBool,
    format: Format,
    timeline_base_samples: AtomicU64,
    timeline_origin_us: AtomicI64,
    first_audio_pts_us: AtomicI64,
    leading_silence_samples: AtomicU64,
    /// Silence the device emits *without* advancing the media clock or consuming the ring.
    ///
    /// This is the live audio/video presentation delay. Leading silence is the opposite tool: it
    /// covers timeline that has already elapsed and therefore does advance the clock.
    hold_silence_samples: AtomicU64,
    /// Silence covering timeline the producer dropped. It advances the media clock, like leading
    /// silence, but has its own counter because `observe_timeline_pts` recomputes and *stores*
    /// leading silence on every packet and would otherwise erase a bridge before it played out.
    gap_silence_samples: AtomicU64,
    /// The delay the hold is currently implementing, so it can be measured and adjusted.
    live_delay_us: AtomicU64,
    prebuffer_samples: AtomicU64,
    /// Playback starts at exactly `prebuffer_samples`, discarding whatever piled up beyond it.
    ///
    /// Also marks the reserve as one this presenter tightened, and so may give back on evidence.
    trim_to_prebuffer: AtomicBool,
    /// What one underrun earns a tightened live reserve back, in samples.
    prebuffer_step_samples: u64,
    /// The deepest a tightened live reserve may grow back to: this presenter's own default.
    prebuffer_ceiling_samples: u64,
    /// Queued-sample count observed at a resume PLAY.
    ///
    /// Control and realtime media may use independent transports. Retained audio alone must not
    /// restart the device before the realtime path has caught up with PLAY, or that tail can drain
    /// into an underrun and stop the clock that paces linked video.
    resume_queued_samples: AtomicU64,
    resume_waiting_for_audio: AtomicBool,
    ring_capacity_samples: u64,
    requested_start_pts_us: AtomicI64,
    gain_bits: AtomicU32,
    play_configured_at: Mutex<Option<Instant>>,
    clock_progress: Mutex<ClockProgress>,
    error: Mutex<Option<String>>,
}

struct ClockProgress {
    rendered_samples: u64,
    observed_at: Instant,
}

impl Shared {
    fn set_error(&self, message: impl Into<String>) {
        let mut error = self
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if error.is_none() {
            let mut message = message.into();
            let mut end = message.len().min(1024);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            *error = Some(message);
        }
    }

    fn error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// One source's bounded device output, clock and channel lifetime.
///
/// Open on a device worker. The host binds this opaque instance to its complete
/// source identity and stops it on retirement. Blocking pushes belong on media
/// workers, never GUI or protocol-control threads.
pub struct Output {
    shared: Arc<Shared>,
    producer: Mutex<HeapProd<f32>>,
    _stream: Option<Stream>,
    sample_rate: u32,
    channels: u16,
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("format", &self.format())
            .field("generation", &self.channel_generation())
            .finish_non_exhaustive()
    }
}

/// Non-device-owning playback controls for an output instance.
///
/// Retaining or dropping this handle never retains or destroys a native stream.
/// It is suitable for host state while the device worker owns [`Output`]. The
/// handle grants no submission or protocol authority. Retirement cannot undo
/// samples already handed to a native device, but permanently stops later callbacks.
#[derive(Clone)]
pub struct OutputControl {
    shared: Arc<Shared>,
}

/// A callback-quiescent clock observation from one paused output.
///
/// This does not describe device or acoustic latency. Samples already handed to
/// the native device cannot be recalled. Starting or reconfiguring the output
/// invalidates the observation; the host retains its own source identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PausedClock {
    /// The frozen media position, absent before its timeline origin is known.
    pub pts_us: Option<i64>,
}

impl std::fmt::Debug for OutputControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputControl")
            .field("format", &self.shared.format)
            .finish_non_exhaustive()
    }
}

impl OutputControl {
    /// Reports whether the configured startup buffer or drained EOS is ready.
    ///
    /// This atomic observation never starts playback, waits or calls a device.
    /// The host must check its source and playback identity separately.
    /// A permanently stopped output is never ready to start.
    pub fn ready_to_start(&self) -> bool {
        !self.shared.stopped.load(Ordering::SeqCst) && audio_prebuffer_ready(&self.shared)
    }

    /// Observes a paused clock after every consuming callback has committed.
    ///
    /// Returns `None` while enabled or a callback can still advance the clock.
    /// A returned observation can itself have an unknown PTS. This operation
    /// never waits, allocates, locks or calls a native device. Keep the output
    /// paused and serialize start/reconfiguration while taking this observation.
    pub fn paused_clock(&self) -> Option<PausedClock> {
        let quiescent = || {
            (!self.shared.enabled.load(Ordering::SeqCst)
                || self.shared.stopped.load(Ordering::SeqCst))
                && !self.shared.callback_active.load(Ordering::SeqCst)
        };
        if !quiescent() {
            return None;
        }
        let clock = PausedClock {
            pts_us: self.clock_pts(),
        };
        quiescent().then_some(clock)
    }

    /// Returns the media clock, including its retained paused position.
    pub fn clock_pts(&self) -> Option<i64> {
        let origin = self.shared.timeline_origin_us.load(Ordering::SeqCst);
        (origin != UNSET_PTS).then(|| {
            rendered_pts_us(
                origin,
                self.shared.rendered_samples.load(Ordering::SeqCst),
                self.shared.format.sample_rate,
                self.shared.format.channels,
            )
        })
    }
    /// Enables a live output after activation; permanent stop wins over start.
    pub fn start(&self) {
        if !self.shared.stopped.load(Ordering::SeqCst) {
            self.shared.enabled.store(true, Ordering::SeqCst);
        }
    }

    /// Pauses consumption without discarding samples or the device clock.
    pub fn pause(&self) {
        self.shared.enabled.store(false, Ordering::SeqCst);
    }

    /// Permanently silences future callbacks and releases blocked writers.
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        self.pause();
    }

    /// Updates linear gain without changing queued samples or the clock.
    pub fn set_gain(&self, gain: AudioGain) {
        self.shared
            .gain_bits
            .store(gain.as_f32().to_bits(), Ordering::SeqCst);
    }

    /// Returns the number of complete frames elapsed on this media timeline.
    pub fn rendered_frames(&self) -> u64 {
        self.shared.rendered_samples.load(Ordering::SeqCst) / u64::from(self.shared.format.channels)
    }
}

impl Output {
    /// Returns controls that cannot keep this instance's native stream alive.
    pub fn control(&self) -> OutputControl {
        OutputControl {
            shared: self.shared.clone(),
        }
    }
    /// Opens the default native device with a bounded two-second ring.
    ///
    /// # Errors
    /// Returns an error for zero generation, unavailable/unsupported device,
    /// excessive ring size, or stream creation/start failure.
    pub fn open(channel_generation: u64) -> io::Result<Arc<Self>> {
        Self::open_with_limits(channel_generation, Limits::default())
    }

    /// Opens the default device within explicit pre-admitted ring limits.
    ///
    /// # Errors
    /// Returns an error for invalid generation/limits, unsupported device format,
    /// excessive ring storage, or native device/stream failure.
    pub fn open_with_limits(channel_generation: u64, limits: Limits) -> io::Result<Arc<Self>> {
        if channel_generation == 0 {
            return Err(retired_channel());
        }
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no default audio output device")
        })?;
        let supported = device.default_output_config().map_err(|error| {
            io::Error::other(format!("could not query the default audio output: {error}"))
        })?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let ring_capacity_samples = limits.capacity_for(Format {
            sample_rate: config.sample_rate,
            channels: config.channels,
        })?;
        let shared = Arc::new(Shared {
            channel_generation: AtomicU64::new(channel_generation),
            ready_generation: AtomicU64::new(channel_generation),
            generation_transition: Mutex::new(()),
            enabled: AtomicBool::new(false),
            callback_active: AtomicBool::new(false),
            #[cfg(test)]
            before_callback_commit: Mutex::new(None),
            prebuffered: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            decode_done: AtomicBool::new(false),
            eos_observed: AtomicBool::new(false),
            queued_samples: AtomicU64::new(0),
            played_samples: AtomicU64::new(0),
            discard_through_samples: AtomicU64::new(0),
            discard_samples: AtomicU64::new(0),
            rendered_samples: AtomicU64::new(0),
            presented_samples: AtomicU64::new(0),
            last_presented_position: AtomicU64::new(0),
            staged_timed: AtomicBool::new(false),
            queue_timeline: Mutex::new(Default::default()),
            timed: AtomicBool::new(false),
            format: Format {
                sample_rate: config.sample_rate,
                channels: config.channels,
            },
            timeline_base_samples: AtomicU64::new(0),
            timeline_origin_us: AtomicI64::new(UNSET_PTS),
            first_audio_pts_us: AtomicI64::new(UNSET_PTS),
            leading_silence_samples: AtomicU64::new(0),
            hold_silence_samples: AtomicU64::new(0),
            gap_silence_samples: AtomicU64::new(0),
            live_delay_us: AtomicU64::new(0),
            prebuffer_samples: AtomicU64::new(
                samples_for_duration(
                    config.sample_rate,
                    config.channels,
                    PREBUFFER_MILLISECONDS * 1_000,
                )
                .min(ring_capacity_samples as u64),
            ),
            trim_to_prebuffer: AtomicBool::new(false),
            prebuffer_step_samples: samples_for_duration(
                config.sample_rate,
                config.channels,
                LIVE_PREBUFFER_STEP_US,
            ),
            prebuffer_ceiling_samples: samples_for_duration(
                config.sample_rate,
                config.channels,
                PREBUFFER_MILLISECONDS * 1_000,
            )
            .min(ring_capacity_samples as u64),
            resume_queued_samples: AtomicU64::new(0),
            resume_waiting_for_audio: AtomicBool::new(false),
            ring_capacity_samples: u64::try_from(ring_capacity_samples).unwrap_or(u64::MAX),
            requested_start_pts_us: AtomicI64::new(UNSET_PTS),
            gain_bits: AtomicU32::new(1.0_f32.to_bits()),
            play_configured_at: Mutex::new(None),
            clock_progress: Mutex::new(ClockProgress {
                rendered_samples: 0,
                observed_at: Instant::now(),
            }),
            error: Mutex::new(None),
        });
        let ring = HeapRb::<f32>::new(ring_capacity_samples);
        let (producer, consumer) = ring.split();
        let stream = match format {
            SampleFormat::I8 => build_stream::<i8, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::I16 => build_stream::<i16, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::I24 => build_stream::<I24, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::I32 => build_stream::<i32, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::I64 => build_stream::<i64, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::U8 => build_stream::<u8, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::U16 => build_stream::<u16, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::U24 => build_stream::<U24, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::U32 => build_stream::<u32, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::U64 => build_stream::<u64, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::F32 => build_stream::<f32, _>(&device, &config, consumer, shared.clone()),
            SampleFormat::F64 => build_stream::<f64, _>(&device, &config, consumer, shared.clone()),
            unsupported => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported audio sample format {unsupported}"),
            )),
        }?;
        stream
            .play()
            .map_err(|error| io::Error::other(format!("could not start audio output: {error}")))?;
        Ok(Arc::new(Self {
            shared,
            producer: Mutex::new(producer),
            _stream: Some(stream),
            sample_rate: config.sample_rate,
            channels: config.channels,
        }))
    }

    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn test_output() -> Arc<Self> {
        let shared = Arc::new(Shared {
            channel_generation: AtomicU64::new(1),
            ready_generation: AtomicU64::new(1),
            generation_transition: Mutex::new(()),
            enabled: AtomicBool::new(false),
            callback_active: AtomicBool::new(false),
            #[cfg(test)]
            before_callback_commit: Mutex::new(None),
            prebuffered: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            decode_done: AtomicBool::new(false),
            eos_observed: AtomicBool::new(false),
            queued_samples: AtomicU64::new(0),
            played_samples: AtomicU64::new(0),
            discard_through_samples: AtomicU64::new(0),
            discard_samples: AtomicU64::new(0),
            rendered_samples: AtomicU64::new(0),
            presented_samples: AtomicU64::new(0),
            last_presented_position: AtomicU64::new(0),
            staged_timed: AtomicBool::new(false),
            queue_timeline: Mutex::new(Default::default()),
            timed: AtomicBool::new(false),
            format: Format {
                sample_rate: 48_000,
                channels: 2,
            },
            timeline_base_samples: AtomicU64::new(0),
            timeline_origin_us: AtomicI64::new(UNSET_PTS),
            first_audio_pts_us: AtomicI64::new(UNSET_PTS),
            leading_silence_samples: AtomicU64::new(0),
            hold_silence_samples: AtomicU64::new(0),
            gap_silence_samples: AtomicU64::new(0),
            live_delay_us: AtomicU64::new(0),
            prebuffer_samples: AtomicU64::new(0),
            trim_to_prebuffer: AtomicBool::new(false),
            prebuffer_step_samples: samples_for_duration(48_000, 2, LIVE_PREBUFFER_STEP_US),
            prebuffer_ceiling_samples: samples_for_duration(
                48_000,
                2,
                PREBUFFER_MILLISECONDS * 1_000,
            ),
            resume_queued_samples: AtomicU64::new(0),
            resume_waiting_for_audio: AtomicBool::new(false),
            ring_capacity_samples: 192_000,
            requested_start_pts_us: AtomicI64::new(UNSET_PTS),
            gain_bits: AtomicU32::new(1.0_f32.to_bits()),
            play_configured_at: Mutex::new(None),
            clock_progress: Mutex::new(ClockProgress {
                rendered_samples: 0,
                observed_at: Instant::now(),
            }),
            error: Mutex::new(None),
        });
        let (producer, _consumer) = HeapRb::<f32>::new(192_000).split();
        Arc::new(Self {
            shared,
            producer: Mutex::new(producer),
            _stream: None,
            sample_rate: 48_000,
            channels: 2,
        })
    }

    /// A device-free sink that runs the production callback against a bounded ring.
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn test_clocked_output(generation: u64) -> Arc<Self> {
        let mut output = Arc::try_unwrap(Self::test_output()).ok().unwrap();
        let capacity = 48_000 * 2 * RING_BUFFER_SECONDS;
        let shared = Arc::get_mut(&mut output.shared).unwrap();
        shared.ring_capacity_samples = capacity as u64;
        shared
            .channel_generation
            .store(generation, Ordering::SeqCst);
        shared.ready_generation.store(generation, Ordering::SeqCst);
        let (producer, mut consumer) = HeapRb::<f32>::new(capacity).split();
        output.producer = Mutex::new(producer);
        let weak = Arc::downgrade(&output.shared);
        thread::spawn(move || {
            let mut samples = [0.0_f32; 960];
            loop {
                thread::sleep(Duration::from_millis(10));
                let Some(shared) = weak.upgrade() else { break };
                if shared.stopped.load(Ordering::SeqCst) {
                    break;
                }
                render_audio(&mut samples, &mut consumer, &shared);
            }
        });
        Arc::new(output)
    }

    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn force_video_gate_stall_for_test(&self) {
        *self
            .shared
            .play_configured_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(Instant::now() - LINKED_AUDIO_STALL_FALLBACK - Duration::from_millis(1));
        self.shared
            .clock_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observed_at = Instant::now() - LINKED_AUDIO_STALL_FALLBACK - Duration::from_millis(1);
    }

    /// The fixed device format to request from a decoder or mixer.
    pub fn format(&self) -> Format {
        Format {
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }

    /// Allocated interleaved ring samples, for aggregate host accounting.
    pub fn ring_capacity_samples(&self) -> u64 {
        self.shared.ring_capacity_samples
    }

    /// Number of queued interleaved samples not yet consumed or discarded.
    pub fn buffered_samples(&self) -> u64 {
        self.shared
            .queued_samples
            .load(Ordering::SeqCst)
            .saturating_sub(self.shared.played_samples.load(Ordering::SeqCst))
    }

    /// Returns the current generation, including one still completing retirement.
    pub fn channel_generation(&self) -> u64 {
        self.shared.channel_generation.load(Ordering::SeqCst)
    }

    /// Retain the device stream while making every operation from the retired channel stale.
    ///
    /// The generation is published before waiting for an in-flight push. A producer blocked on a
    /// full, paused ring therefore observes retirement and releases the transition lock; once the
    /// lock is acquired, flushing is a barrier after every mutation from the old channel.
    /// Replacement channels remain unavailable until that flush finishes;
    /// another advance during the transition is refused.
    pub fn advance_channel(&self, current: u64, next: u64) -> bool {
        if current == 0
            || current.checked_add(1) != Some(next)
            || self.shared.ready_generation.load(Ordering::SeqCst) != current
            || self.shared.stopped.load(Ordering::SeqCst)
        {
            return false;
        }
        if self
            .shared
            .channel_generation
            .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let _transition = self
            .shared
            .generation_transition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.flush();
        self.shared.ready_generation.store(next, Ordering::SeqCst);
        true
    }

    /// Acquires a capability scoped to this instance and its current generation.
    ///
    /// # Errors
    /// Returns an error for a zero, retired, transitioning or stopped generation.
    pub fn channel(self: &Arc<Self>, generation: u64) -> io::Result<Channel> {
        if generation == 0
            || self.channel_generation() != generation
            || self.shared.ready_generation.load(Ordering::SeqCst) != generation
            || self.shared.stopped.load(Ordering::SeqCst)
        {
            return Err(retired_channel());
        }
        Ok(Channel {
            output: self.clone(),
            generation,
        })
    }

    /// Enables playback after prebuffer readiness; a stopped output stays stopped.
    pub fn start(&self) {
        self.control().start();
    }

    /// Size the startup reserve for a live track from the target latency it declared.
    ///
    /// A live track never issues PLAY, so without this it keeps the conservative
    /// 100 ms default meant for a stream whose pacing is unknown. That reserve
    /// is not just a startup cost: the device drains at exactly the rate the producer fills it, so
    /// whatever is queued when the first callback runs stays queued, and the listener hears every
    /// millisecond of it as delay for the rest of the stream. A producer that declares a tighter
    /// target — an interactive one sending its own mixer output — gets the delay it asked for.
    ///
    /// Zero is "no target stated", not "no delay wanted", and keeps the default. The declared
    /// value can only lower the reserve from that default, never raise it, so a producer cannot
    /// talk this presenter into holding more sound than it would have held anyway.
    pub fn configure_live(&self, target_latency_us: u64) {
        self.shared.timed.store(false, Ordering::SeqCst);
        if target_latency_us == 0 {
            return;
        }
        let target_us =
            target_latency_us.clamp(MINIMUM_LIVE_PREBUFFER_US, PREBUFFER_MILLISECONDS * 1_000);
        self.shared.prebuffer_samples.store(
            self.samples_for(target_us)
                .min(self.shared.ring_capacity_samples),
            Ordering::SeqCst,
        );
        self.shared.trim_to_prebuffer.store(true, Ordering::SeqCst);
    }

    /// Configures live startup while preserving the entire staged PCM timeline.
    ///
    /// Call before submitting samples. The host inserts gaps and trims overlaps
    /// in queue order. Startup and underrun recovery never discard queued frames;
    /// an empty device ring holds this live clock until more frames arrive.
    /// The target reserve follows the same bounds as [`Self::configure_live`].
    pub fn configure_timestamped_live(&self, target_latency_us: u64) {
        self.configure_live(target_latency_us);
        self.shared.trim_to_prebuffer.store(false, Ordering::SeqCst);
    }

    /// Updates linear gain without changing clock or queued media.
    pub fn set_gain(&self, gain: AudioGain) {
        self.control().set_gain(gain);
    }

    /// Validates PLAY without changing the clock or device state.
    ///
    /// # Errors
    /// Returns an error for an unknown start PTS or prebuffer beyond ring capacity.
    pub fn validate_play(&self, start_pts_us: i64, minimum_buffer_us: u64) -> io::Result<()> {
        let requested = (u128::from(self.sample_rate) * u128::from(minimum_buffer_us))
            .div_ceil(1_000_000)
            * u128::from(self.channels);
        if start_pts_us == UNSET_PTS || requested > u128::from(self.shared.ring_capacity_samples) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio PLAY exceeds output capacity",
            ));
        }
        Ok(())
    }

    /// Configures exact timed startup or resume without starting the device.
    ///
    /// # Errors
    /// Returns an error for an unknown start PTS or unreachable prebuffer, before mutation.
    pub fn configure_play(&self, start_pts_us: i64, minimum_buffer_us: u64) -> io::Result<()> {
        self.validate_play(start_pts_us, minimum_buffer_us)?;
        self.shared.timed.store(true, Ordering::SeqCst);
        self.shared.hold_silence_samples.store(0, Ordering::SeqCst);
        self.shared.gap_silence_samples.store(0, Ordering::SeqCst);
        self.shared.live_delay_us.store(0, Ordering::SeqCst);
        self.shared
            .requested_start_pts_us
            .store(start_pts_us, Ordering::SeqCst);
        // `rendered_samples` and the samples retained in the ring both belong to this channel's
        // existing timeline. A PLAY after PAUSE must not re-label either of them with a new
        // origin: doing so makes the retained audio tail disagree with linked video and creates a
        // second jump when newly submitted audio reaches the device. Before the first rendered
        // sample, PLAY and the first packet may still arrive in either order, so realign below.
        let rendered_samples = self.shared.rendered_samples.load(Ordering::SeqCst);
        if rendered_samples == 0 {
            self.shared
                .timeline_origin_us
                .store(start_pts_us, Ordering::SeqCst);
        }
        // PLAY and the first post-pause audio packet are deliberately independent protocol paths.
        // Snapshot the accepted-audio watermark before enabling the device. A resume does not
        // prebuffer from the retained tail alone: at least one new decoded sample proves the
        // realtime path has crossed the same boundary. A full ring is already safe and must be
        // allowed to drain, because its producer may be blocked waiting for capacity.
        self.shared.resume_queued_samples.store(
            self.shared.queued_samples.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
        self.shared
            .resume_waiting_for_audio
            .store(rendered_samples != 0, Ordering::SeqCst);
        let minimum_samples = (u128::from(self.sample_rate) * u128::from(minimum_buffer_us))
            .div_ceil(1_000_000)
            * u128::from(self.channels);
        self.shared
            .prebuffer_samples
            .store(minimum_samples as u64, Ordering::SeqCst);
        self.shared.prebuffered.store(false, Ordering::SeqCst);
        self.shared.trim_to_prebuffer.store(false, Ordering::SeqCst);
        *self
            .shared
            .play_configured_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
        *self
            .shared
            .clock_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ClockProgress {
            rendered_samples: self.shared.rendered_samples.load(Ordering::SeqCst),
            observed_at: Instant::now(),
        };
        self.realign_timeline();
        Ok(())
    }

    /// Reports whether buffered media or completed decoding permits startup.
    pub fn ready_to_start(&self) -> bool {
        audio_prebuffer_ready(&self.shared)
    }

    /// Freezes consumption and retains queued samples and clock position.
    pub fn pause(&self) {
        self.control().pause();
    }

    /// Pauses and retires queued samples before a new source timeline.
    pub fn flush(&self) {
        self.pause();
        let queued = self.shared.queued_samples.load(Ordering::SeqCst);
        self.shared
            .timeline_base_samples
            .store(queued, Ordering::SeqCst);
        self.shared
            .discard_through_samples
            .fetch_max(queued, Ordering::SeqCst);
        self.shared.discard_samples.store(0, Ordering::SeqCst);
        self.shared.hold_silence_samples.store(0, Ordering::SeqCst);
        self.shared.gap_silence_samples.store(0, Ordering::SeqCst);
        self.shared.live_delay_us.store(0, Ordering::SeqCst);
        self.shared.prebuffered.store(false, Ordering::SeqCst);
        self.shared.resume_queued_samples.store(0, Ordering::SeqCst);
        self.shared
            .resume_waiting_for_audio
            .store(false, Ordering::SeqCst);
        self.shared.decode_done.store(false, Ordering::SeqCst);
        self.shared.eos_observed.store(false, Ordering::SeqCst);
        self.shared.rendered_samples.store(0, Ordering::SeqCst);
        self.shared.presented_samples.store(0, Ordering::SeqCst);
        self.shared
            .last_presented_position
            .store(0, Ordering::SeqCst);
        self.shared.staged_timed.store(false, Ordering::SeqCst);
        *self
            .shared
            .queue_timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Default::default();
        self.shared
            .timeline_origin_us
            .store(UNSET_PTS, Ordering::SeqCst);
        self.shared
            .requested_start_pts_us
            .store(UNSET_PTS, Ordering::SeqCst);
        self.shared
            .first_audio_pts_us
            .store(UNSET_PTS, Ordering::SeqCst);
        self.shared
            .leading_silence_samples
            .store(0, Ordering::SeqCst);
        *self
            .shared
            .play_configured_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        *self
            .shared
            .clock_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ClockProgress {
            rendered_samples: 0,
            observed_at: Instant::now(),
        };
    }

    /// Anchor the media clock on the audio this timeline will actually render.
    ///
    /// The clock is `origin + rendered/rate`, so the anchor only means anything while nothing has
    /// been rendered against it: once samples have left the device, moving the origin rewrites
    /// time that has already been presented. Within that window PLAY and the first packet may
    /// arrive in either order and both have to agree, so the origin is the earlier of the
    /// requested start and the audio actually queued, and the distance between them is leading
    /// silence rather than clock the device would otherwise run early.
    fn realign_timeline(&self) {
        if self.shared.rendered_samples.load(Ordering::SeqCst) != 0 {
            return;
        }
        let requested = self.shared.requested_start_pts_us.load(Ordering::SeqCst);
        let audio_pts = self.shared.first_audio_pts_us.load(Ordering::SeqCst);
        let origin = match (requested, audio_pts) {
            (UNSET_PTS, UNSET_PTS) => return,
            (UNSET_PTS, audio) => audio,
            (start, UNSET_PTS) => start,
            (start, audio) => start.min(audio),
        };
        self.shared
            .timeline_origin_us
            .store(origin, Ordering::SeqCst);
        let leading_silence = if audio_pts == UNSET_PTS {
            0
        } else {
            leading_silence_sample_count(origin, audio_pts, self.sample_rate, self.channels)
        };
        self.shared
            .leading_silence_samples
            .store(leading_silence, Ordering::SeqCst);
    }

    /// Observes the earliest admitted audio PTS without moving an established clock.
    pub fn observe_audio_pts(&self, pts_us: i64) {
        if pts_us != UNSET_PTS {
            // A packet straddling an already requested start is trimmed to that start before it is
            // pushed, so the first sample the device will render is the later of the two. Anchor
            // on that rather than on the packet header, which would run the clock a packet slow.
            let requested = self.shared.requested_start_pts_us.load(Ordering::SeqCst);
            let queued_pts = if requested == UNSET_PTS {
                pts_us
            } else {
                pts_us.max(requested)
            };
            let mut current = self.shared.first_audio_pts_us.load(Ordering::SeqCst);
            while current == UNSET_PTS || queued_pts < current {
                match self.shared.first_audio_pts_us.compare_exchange(
                    current,
                    queued_pts,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        }
        self.realign_timeline();
    }

    /// Removes decoded samples preceding the exact requested start PTS.
    pub fn trim_before_start(&self, pts_us: i64, duration_us: u64, samples: &mut Vec<f32>) {
        let start = self.shared.requested_start_pts_us.load(Ordering::SeqCst);
        if start == UNSET_PTS || pts_us >= start || samples.is_empty() {
            return;
        }
        let packet_end = pts_us.saturating_add(i64::try_from(duration_us).unwrap_or(i64::MAX));
        if packet_end <= start || duration_us == 0 {
            samples.clear();
            return;
        }
        let discard_us = start.saturating_sub(pts_us) as u64;
        let discard = self.samples_for(discard_us);
        let discard = usize::try_from(discard)
            .unwrap_or(usize::MAX)
            .min(samples.len());
        samples.drain(..discard);
    }

    /// Reports whether the running audio master has reached a media PTS.
    pub fn pts_reached(&self, pts_us: i64) -> bool {
        if !self.shared.enabled.load(Ordering::SeqCst)
            || !self.shared.prebuffered.load(Ordering::SeqCst)
        {
            return false;
        }
        let origin = self.shared.timeline_origin_us.load(Ordering::SeqCst);
        origin != UNSET_PTS
            && pts_us
                <= rendered_pts_us(
                    origin,
                    self.shared.rendered_samples.load(Ordering::SeqCst),
                    self.sample_rate,
                    self.channels,
                )
    }

    /// Audio-master position remains observable while paused or waiting for prebuffer.
    pub fn clock_pts(&self) -> Option<i64> {
        self.control().clock_pts()
    }

    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn enabled_for_test(&self) -> bool {
        self.shared.enabled.load(Ordering::SeqCst)
    }

    /// The media PTS currently leaving the device, or `None` before playback is running.
    pub fn rendered_pts(&self) -> Option<i64> {
        if !self.shared.enabled.load(Ordering::SeqCst)
            || !self.shared.prebuffered.load(Ordering::SeqCst)
        {
            return None;
        }
        let origin = self.shared.timeline_origin_us.load(Ordering::SeqCst);
        (origin != UNSET_PTS).then(|| {
            rendered_pts_us(
                origin,
                self.shared.rendered_samples.load(Ordering::SeqCst),
                self.sample_rate,
                self.channels,
            )
        })
    }

    /// Oldest video PTS worth converting for a linked live stream. Frames before this threshold
    /// are still decoded as references, but their pixels need not be allocated or uploaded.
    ///
    /// The threshold is measured against the clock *after* the live presentation delay, so it means
    /// "older than the picture that belongs with the sound coming out of the speaker" rather than
    /// "slower to arrive than audio". Those are the same thing only on a link with no video
    /// queueing delay, and treating them as the same discarded every frame of a congested session.
    pub fn discard_video_before(&self, maximum_latency_us: u64) -> Option<i64> {
        self.rendered_pts()
            .map(|pts| pts.saturating_sub(i64::try_from(maximum_latency_us).unwrap_or(i64::MAX)))
    }

    /// How far behind capture this output is deliberately playing, in microseconds.
    pub fn live_delay_us(&self) -> u64 {
        self.shared.live_delay_us.load(Ordering::SeqCst)
    }

    /// Delays live audio to match the associated video presentation.
    ///
    /// Audio and video are captured on one clock but do not arrive on one: video is two orders of
    /// magnitude larger and queues behind itself in the transport. Playing audio the moment it
    /// arrives therefore runs sound ahead of picture by exactly that difference. The device holds
    /// the difference as silence that does not advance the media clock, which is the only way the
    /// two can be presented together. Returns the delay now in force.
    pub fn request_live_delay(&self, target_us: u64) -> u64 {
        if self.shared.timed.load(Ordering::SeqCst) {
            return 0;
        }
        let capacity_us = self.shared.ring_capacity_samples.saturating_mul(1_000_000)
            / (u64::from(self.sample_rate) * u64::from(self.channels));
        let target_us = target_us.min(MAXIMUM_LIVE_DELAY_US).min(capacity_us);
        let current = self.shared.live_delay_us.load(Ordering::SeqCst);
        if target_us.abs_diff(current) < LIVE_DELAY_STEP_US {
            return current;
        }
        if target_us > current {
            let added = target_us - current;
            self.shared
                .hold_silence_samples
                .fetch_add(self.samples_for(added), Ordering::SeqCst);
            self.shared.live_delay_us.store(target_us, Ordering::SeqCst);
            return target_us;
        }
        // Shrinking the delay means throwing away audio that has already been buffered, so it is
        // only worth doing when the saving is audible-scale and the excess is durable.
        let removed = current - target_us;
        self.shared
            .discard_samples
            .fetch_add(self.samples_for(removed), Ordering::SeqCst);
        self.shared.live_delay_us.store(target_us, Ordering::SeqCst);
        target_us
    }

    /// How long until the media clock reaches `pts_us`, or `None` when it already has.
    ///
    /// The device renders at a fixed rate, so the remaining time is arithmetic rather than
    /// something to poll for. `None` is also returned when the clock is not running: the caller
    /// has its own fallback for that and must not wait on a clock that is not advancing.
    pub fn time_until_pts(&self, pts_us: i64) -> Option<Duration> {
        let rendered = self.rendered_pts()?;
        let remaining = pts_us
            .checked_sub(rendered)
            .filter(|remaining| *remaining > 0)?;
        Some(Duration::from_micros(
            u64::try_from(remaining).unwrap_or(u64::MAX),
        ))
    }

    /// How long until the device has consumed `samples` from the ring.
    ///
    /// Used to wait out a full ring buffer without polling it. The device only drains while it is
    /// enabled, so a stopped or paused output reports nothing and the caller re-checks its own
    /// exit conditions instead.
    fn time_to_drain(&self, samples: u64) -> Option<Duration> {
        if !self.shared.enabled.load(Ordering::SeqCst) {
            return None;
        }
        let rate = u64::from(self.sample_rate).saturating_mul(u64::from(self.channels));
        (rate > 0)
            .then(|| Duration::from_micros(samples.saturating_mul(1_000_000).div_ceil(rate.max(1))))
    }

    fn samples_for(&self, duration_us: u64) -> u64 {
        samples_for_duration(self.sample_rate, self.channels, duration_us)
    }

    /// Covers a bounded live audio gap without restarting playback.
    ///
    /// # Errors
    /// Returns an error for timed output or gaps exceeding the ring's duration budget.
    ///
    /// A producer that dropped packets leaves a hole. Silence of exactly the hole's length keeps
    /// the media clock honest — the alternative, restarting the timeline, discards every buffered
    /// sample and is audible every time transport jitter is mistaken for a discontinuity.
    pub fn bridge_live_gap(&self, gap_us: u64) -> io::Result<()> {
        let count = (u128::from(self.sample_rate) * u128::from(gap_us)).div_ceil(1_000_000)
            * u128::from(self.channels);
        if self.shared.timed.load(Ordering::SeqCst)
            || count > u128::from(self.shared.ring_capacity_samples)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "live audio gap exceeds output budget",
            ));
        }
        self.shared
            .gap_silence_samples
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current
                    .checked_add(count as u64)
                    .filter(|n| *n <= self.shared.ring_capacity_samples)
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "live audio gap exceeds output budget",
                )
            })?;
        Ok(())
    }

    /// Drop queued device samples and restart a live timeline. Reserved for a producer that
    /// genuinely restarted its clock, which is the only case a continuous timeline cannot express.
    pub fn rebase_live(&self, pts_us: i64) {
        self.flush();
        self.observe_audio_pts(pts_us);
        self.start();
    }

    pub fn video_gate_stalled(&self) -> bool {
        if !self.shared.enabled.load(Ordering::SeqCst)
            || !self
                .shared
                .play_configured_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some_and(|configured| configured.elapsed() > LINKED_AUDIO_STALL_FALLBACK)
        {
            return false;
        }
        let rendered_samples = self.shared.rendered_samples.load(Ordering::SeqCst);
        let mut progress = self
            .shared
            .clock_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if rendered_samples != progress.rendered_samples {
            progress.rendered_samples = rendered_samples;
            progress.observed_at = Instant::now();
            return false;
        }
        if progress.observed_at.elapsed() <= LINKED_AUDIO_STALL_FALLBACK {
            return false;
        }
        drop(progress);
        self.shared.set_error("linked audio output clock stalled");
        true
    }

    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn push(&self, samples: &[f32]) -> io::Result<()> {
        self.push_while(samples, || true)
    }

    fn push_while(&self, samples: &[f32], current: impl Fn() -> bool) -> io::Result<()> {
        if !samples.len().is_multiple_of(usize::from(self.channels))
            || samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio samples must be finite whole frames",
            ));
        }
        let mut producer = self
            .producer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.shared
            .queued_samples
            .load(Ordering::SeqCst)
            .checked_add(samples.len() as u64)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "audio sample counter exhausted",
                )
            })?;
        let mut offset = 0;
        while offset < samples.len() {
            if !current() {
                return Err(retired_channel());
            }
            if self.shared.stopped.load(Ordering::SeqCst) {
                return Err(retired_channel());
            }
            if let Some(error) = self.shared.error() {
                return Err(io::Error::new(io::ErrorKind::NotConnected, error));
            }
            // A callback publishes each pop independently. Even a whole-frame
            // producer may therefore see only one vacant channel slot. Do not
            // publish a prefix of a frame while its remaining channels wait.
            let channels = usize::from(self.channels);
            let vacant = ringbuf::traits::Observer::vacant_len(&*producer);
            let available = vacant / channels * channels;
            let count = available.min(samples.len() - offset);
            let accepted = producer.push_slice(&samples[offset..offset + count]);
            if accepted != 0 {
                self.shared
                    .queued_samples
                    .fetch_add(accepted as u64, Ordering::SeqCst);
                offset += accepted;
                continue;
            }
            // The ring is full. It drains at the device's fixed rate, so wait for the device to
            // consume a chunk rather than polling it. Signalling from the audio callback instead
            // would put a lock on the realtime thread.
            let wait = self
                .time_to_drain(RING_WAIT_SAMPLES)
                .unwrap_or(STOPPED_OUTPUT_RECHECK)
                .clamp(MINIMUM_RING_WAIT, STOPPED_OUTPUT_RECHECK);
            thread::sleep(wait);
        }
        Ok(())
    }

    /// Records decoder completion; queued device samples still have to drain.
    pub fn finish_decode(&self) {
        self.shared.decode_done.store(true, Ordering::SeqCst);
    }

    /// Records EOS acceptance independently of decoder and device drain.
    pub fn signal_eos(&self) {
        self.shared.eos_observed.store(true, Ordering::SeqCst);
    }

    /// One non-blocking drain check.
    ///
    /// `None` means still draining. Media §15 makes `DRAIN` bounded pending state that must not
    /// block control, so the session actor polls this instead of parking a thread on
    /// [`Self::poll_drained`].
    pub fn poll_drained(&self) -> Option<io::Result<()>> {
        if self.shared.stopped.load(Ordering::SeqCst) {
            return Some(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "audio output stopped",
            )));
        }
        if let Some(error) = self.shared.error() {
            return Some(Err(io::Error::new(io::ErrorKind::NotConnected, error)));
        }
        let drained = self.shared.eos_observed.load(Ordering::SeqCst)
            && self.shared.decode_done.load(Ordering::SeqCst)
            && self.shared.played_samples.load(Ordering::SeqCst)
                >= self.shared.queued_samples.load(Ordering::SeqCst);
        drained.then_some(Ok(()))
    }

    /// Permanently silences this output and releases blocked producers.
    pub fn stop(&self) {
        self.control().stop();
    }
}

/// An opaque submission capability for one output instance and generation.
#[derive(Debug)]
pub struct Channel {
    output: Arc<Output>,
    generation: u64,
}

impl Channel {
    /// Publishes a whole-frame prefix without waiting for ring space.
    ///
    /// Zero means the device ring has no complete vacant frame. Run on a media
    /// worker; input validation examines the supplied sample slice.
    ///
    /// # Errors
    /// Returns WouldBlock for another writer, Interrupted for retirement, and
    /// InvalidInput for nonfinite/incomplete samples or exhausted accounting.
    /// Device failures return NotConnected.
    pub fn try_push(&self, samples: &[f32]) -> io::Result<usize> {
        let _transition = self
            .output
            .shared
            .generation_transition
            .try_lock()
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        if self.output.channel_generation() != self.generation
            || self.output.shared.ready_generation.load(Ordering::SeqCst) != self.generation
            || self.output.shared.stopped.load(Ordering::SeqCst)
        {
            return Err(retired_channel());
        }
        let channels = usize::from(self.output.channels);
        if !samples.len().is_multiple_of(channels)
            || samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio samples must be finite whole frames",
            ));
        }
        if let Some(error) = self.output.shared.error() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, error));
        }
        let mut producer = self
            .output
            .producer
            .try_lock()
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        let vacant = ringbuf::traits::Observer::vacant_len(&*producer);
        let count = (vacant / channels * channels).min(samples.len());
        self.output
            .shared
            .queued_samples
            .load(Ordering::SeqCst)
            .checked_add(count as u64)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "audio sample counter exhausted",
                )
            })?;
        let accepted = producer.push_slice(&samples[..count]);
        self.output
            .shared
            .queued_samples
            .fetch_add(accepted as u64, Ordering::SeqCst);
        Ok(accepted)
    }

    fn with_current<T>(&self, operation: impl FnOnce(&Output) -> T) -> io::Result<T> {
        let _transition = self
            .output
            .shared
            .generation_transition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.output.channel_generation() != self.generation
            || self.output.shared.ready_generation.load(Ordering::SeqCst) != self.generation
            || self.output.shared.stopped.load(Ordering::SeqCst)
        {
            return Err(retired_channel());
        }
        Ok(operation(&self.output))
    }

    /// Observes the earliest admitted audio PTS without moving an established clock.
    /// Observes PTS for this exact output and channel generation.
    ///
    /// # Errors
    /// Returns an error for a retired/stopped generation; push also refuses invalid
    /// samples, exhausted accounting, or a failed device.
    pub fn observe_audio_pts(&self, pts_us: i64) -> io::Result<()> {
        self.with_current(|output| output.observe_audio_pts(pts_us))
    }

    /// Covers a live producer gap with clock-advancing silence.
    ///
    /// # Errors
    /// Returns an error for a retired/stopped generation; push also refuses invalid
    /// samples, exhausted accounting, or a failed device.
    pub fn bridge_live_gap(&self, gap_us: u64) -> io::Result<()> {
        self.with_current(|output| output.bridge_live_gap(gap_us))?
    }

    /// Restarts a live timeline within this exact channel.
    ///
    /// # Errors
    /// Returns an error for a retired/stopped generation; push also refuses invalid
    /// samples, exhausted accounting, or a failed device.
    pub fn rebase_live(&self, pts_us: i64) -> io::Result<()> {
        self.with_current(|output| output.rebase_live(pts_us))
    }

    /// Removes decoded samples preceding the exact requested start PTS.
    /// Trims pre-start samples for this exact channel.
    ///
    /// # Errors
    /// Returns an error for a retired/stopped generation; push also refuses invalid
    /// samples, exhausted accounting, or a failed device.
    pub fn trim_before_start(
        &self,
        pts_us: i64,
        duration_us: u64,
        samples: &mut Vec<f32>,
    ) -> io::Result<()> {
        self.with_current(|output| output.trim_before_start(pts_us, duration_us, samples))
    }

    /// Submits finite whole frames, waiting only on this output's ring.
    ///
    /// # Errors
    /// Returns an error for a retired/stopped generation; push also refuses invalid
    /// samples, exhausted accounting, or a failed device.
    pub fn push(&self, samples: &[f32]) -> io::Result<()> {
        self.with_current(|output| {
            output.push_while(samples, || output.channel_generation() == self.generation)
        })?
    }

    /// Records decoder completion; queued device samples still have to drain.
    /// Records decoder completion for this channel.
    ///
    /// # Errors
    /// Returns an error for a retired/stopped generation; push also refuses invalid
    /// samples, exhausted accounting, or a failed device.
    pub fn finish_decode(&self) -> io::Result<()> {
        self.with_current(Output::finish_decode)
    }

    /// Records EOS acceptance independently of decoder and device drain.
    /// Records EOS acceptance for this channel.
    ///
    /// # Errors
    /// Returns an error for a retired/stopped generation; push also refuses invalid
    /// samples, exhausted accounting, or a failed device.
    pub fn signal_eos(&self) -> io::Result<()> {
        self.with_current(Output::signal_eos)
    }
}

fn samples_for_duration(sample_rate: u32, channels: u16, duration_us: u64) -> u64 {
    let channels = u128::from(channels);
    if channels == 0 {
        return 0;
    }
    let frames = (u128::from(sample_rate) * u128::from(duration_us)).div_ceil(1_000_000);
    (frames * channels).min(u128::from(u64::MAX) / channels * channels) as u64
}

fn retired_channel() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "retired audio channel generation",
    )
}

/// Drop everything a timeline reset or a shrinking live delay asked for, in one callback.
///
/// Both are index arithmetic on the ring rather than work proportional to a device period, so
/// there is nothing to pace. Pacing them anyway is what made a seek cost the whole buffered
/// reserve: the output stayed silent, `rendered_samples` stood still, and every linked video frame
/// paced by this clock stalled with it for as long as the ring was deep. Returns how many samples
/// were dropped, which is media the caller must not also render.
fn discard_stale_samples<C: Consumer<Item = f32>>(shared: &Shared, consumer: &mut C) -> u64 {
    let mut discarded = 0_u64;
    let discard_through = shared.discard_through_samples.load(Ordering::SeqCst);
    let played = shared.played_samples.load(Ordering::SeqCst);
    if played < discard_through {
        let outstanding = usize::try_from(discard_through - played).unwrap_or(usize::MAX);
        discarded += consumer.skip(outstanding) as u64;
    }
    let requested = shared.discard_samples.load(Ordering::SeqCst);
    if requested != 0 {
        let removed = consumer.skip(usize::try_from(requested).unwrap_or(usize::MAX)) as u64;
        if removed != 0 {
            let _ = shared.discard_samples.try_update(
                Ordering::SeqCst,
                Ordering::SeqCst,
                |remaining| Some(remaining.saturating_sub(removed)),
            );
            discarded += removed;
        }
    }
    if discarded > 0 {
        shared.played_samples.fetch_add(discarded, Ordering::SeqCst);
    }
    discarded
}

/// Start a live stream at the reserve it asked for, not at whatever the device's start-up cost it.
///
/// The gate only says *at least* the target is buffered. A device takes tens of milliseconds to
/// deliver its first callback and the producer fills the ring throughout, so playback routinely
/// begins several times deeper than the track asked for — and it stays there, because from then on
/// the device drains at exactly the rate the producer fills. The overshoot has never been heard and
/// would only ever be heard late, so it is dropped once, here, where the delay is still nothing but
/// queue depth. The clock advances with it: the next audible sample really is that much further
/// into the timeline.
fn trim_to_live_prebuffer<C: Consumer<Item = f32>>(shared: &Shared, consumer: &mut C) {
    if !shared.trim_to_prebuffer.load(Ordering::SeqCst) {
        return;
    }
    let buffered = shared
        .queued_samples
        .load(Ordering::SeqCst)
        .saturating_sub(shared.played_samples.load(Ordering::SeqCst));
    let excess = buffered.saturating_sub(shared.prebuffer_samples.load(Ordering::SeqCst));
    if excess == 0 {
        return;
    }
    let channels = usize::from(shared.format.channels);
    let available = ringbuf::traits::Observer::occupied_len(consumer);
    let count = usize::try_from(excess).unwrap_or(usize::MAX).min(available);
    let removed = consumer.skip(count / channels * channels) as u64;
    shared.played_samples.fetch_add(removed, Ordering::SeqCst);
    shared.rendered_samples.fetch_add(removed, Ordering::SeqCst);
}

/// Give a tightened live reserve back one step after the link proved it too shallow.
///
/// Returns whether the reserve grew. At the ceiling it does not, and the output is left to behave
/// exactly as it did before any of this: a reserve that deep is the one this presenter would have
/// chosen on its own, so there is nothing left to learn and re-gating would only trade a short
/// gap for a longer one.
fn grow_live_prebuffer(shared: &Shared) -> bool {
    let current = shared.prebuffer_samples.load(Ordering::SeqCst);
    if current >= shared.prebuffer_ceiling_samples {
        return false;
    }
    let grown = current
        .saturating_add(shared.prebuffer_step_samples)
        .min(shared.prebuffer_ceiling_samples);
    shared.prebuffer_samples.store(grown, Ordering::SeqCst);
    // Rebuild to the new depth rather than play on from an empty ring, which would underrun again
    // on the next callback and every one after it.
    shared.prebuffered.store(false, Ordering::SeqCst);
    true
}

fn audio_prebuffer_ready(shared: &Shared) -> bool {
    let queued = shared.queued_samples.load(Ordering::SeqCst);
    let played = shared.played_samples.load(Ordering::SeqCst);
    let buffered = if shared.staged_timed.load(Ordering::SeqCst) {
        let valid =
            queued.saturating_sub(played.max(shared.timeline_base_samples.load(Ordering::SeqCst)));
        let leading = shared
            .leading_silence_samples
            .load(Ordering::SeqCst)
            .saturating_sub(shared.rendered_samples.load(Ordering::SeqCst));
        valid.saturating_add(leading)
    } else {
        queued.saturating_sub(played)
    };
    let decode_done = shared.decode_done.load(Ordering::SeqCst);
    let minimum_ready = buffered >= shared.prebuffer_samples.load(Ordering::SeqCst) || decode_done;
    let resume_ready = !shared.resume_waiting_for_audio.load(Ordering::SeqCst)
        || queued > shared.resume_queued_samples.load(Ordering::SeqCst)
        || buffered >= shared.ring_capacity_samples
        || decode_done;
    minimum_ready && resume_ready
}

fn build_stream<T, C>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: C,
    shared: Arc<Shared>,
) -> io::Result<Stream>
where
    T: SizedSample + FromSample<f32>,
    C: Consumer<Item = f32> + Send + 'static,
{
    let error_shared = shared.clone();
    device
        .build_output_stream(
            *config,
            move |output: &mut [T], _| render_audio(output, &mut consumer, &shared),
            move |error| error_shared.set_error(format!("audio output stream error: {error}")),
            None,
        )
        .map_err(|error| io::Error::other(format!("could not build audio output: {error}")))
}

fn take_silence(counter: &AtomicU64, channels: u64) -> bool {
    counter
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            (remaining != 0).then(|| remaining.saturating_sub(channels))
        })
        .is_ok()
}

// CPAL invokes one FnMut consumer per output. Mark entry before checking pause,
// and publish completion only after all clock/sample accounting is committed.
// A callback that starts after a quiescent observation sees pause and consumes
// nothing. This guard adds only atomics to the production realtime path.
struct Callback<'a>(&'a AtomicBool);
impl<'a> Callback<'a> {
    fn enter(shared: &'a Shared) -> Self {
        shared.callback_active.store(true, Ordering::SeqCst);
        Self(&shared.callback_active)
    }
}
impl Drop for Callback<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
struct CallbackGate {
    entered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

fn render_audio<T, C>(output: &mut [T], consumer: &mut C, shared: &Shared)
where
    T: SizedSample + FromSample<f32>,
    C: Consumer<Item = f32>,
{
    let _callback = Callback::enter(shared);
    let staged = shared.staged_timed.load(Ordering::SeqCst);
    if (discard_stale_samples(shared, consumer) > 0 && !staged)
        || shared.stopped.load(Ordering::SeqCst)
        || !shared.enabled.load(Ordering::SeqCst)
    {
        drop(_callback);
        output.fill_with(|| T::from_sample(0.));
        return;
    }
    if !shared.prebuffered.load(Ordering::SeqCst) {
        if !audio_prebuffer_ready(shared) {
            drop(_callback);
            output.fill_with(|| T::from_sample(0.));
            return;
        }
        trim_to_live_prebuffer(shared, consumer);
        shared
            .resume_waiting_for_audio
            .store(false, Ordering::SeqCst);
        shared.prebuffered.store(true, Ordering::SeqCst);
    }
    let channels = usize::from(shared.format.channels);
    let frame_samples = channels as u64;
    let timed = shared.timed.load(Ordering::SeqCst);
    let first_pts = shared.first_audio_pts_us.load(Ordering::SeqCst);
    let origin = shared.timeline_origin_us.load(Ordering::SeqCst);
    let position = shared.rendered_samples.load(Ordering::SeqCst);
    let leading = if staged {
        shared.leading_silence_samples.load(Ordering::SeqCst)
    } else if first_pts == UNSET_PTS || origin == UNSET_PTS {
        0
    } else {
        leading_silence_sample_count(
            origin,
            first_pts,
            shared.format.sample_rate,
            shared.format.channels,
        )
    };
    let consumed = shared
        .played_samples
        .load(Ordering::SeqCst)
        .saturating_sub(shared.timeline_base_samples.load(Ordering::SeqCst));
    let played_before = shared.played_samples.load(Ordering::SeqCst);
    let mut late = if timed && first_pts != UNSET_PTS {
        position.saturating_sub(leading).saturating_sub(consumed)
    } else {
        0
    };
    let (mut played, mut rendered, mut presented) = (0_u64, 0_u64, 0_u64);
    let mut presented_at = None;
    let mut underran = false;
    let gain = f32::from_bits(shared.gain_bits.load(Ordering::SeqCst));
    // A producer can publish only part of a frame while the ring is nearly full.
    // Consume complete interleaved frames so an underrun cannot swap channels.
    for frame in output.chunks_mut(channels) {
        if frame.len() != channels {
            frame.fill_with(|| T::from_sample(0.));
            continue;
        }
        if shared.decode_done.load(Ordering::SeqCst)
            && played_before.saturating_add(played) >= shared.queued_samples.load(Ordering::SeqCst)
        {
            frame.fill_with(|| T::from_sample(0.));
            continue;
        }
        if timed {
            if first_pts == UNSET_PTS || position.saturating_add(rendered) < leading {
                frame.fill_with(|| T::from_sample(0.));
                if first_pts != UNSET_PTS || !shared.decode_done.load(Ordering::SeqCst) {
                    rendered += frame_samples;
                }
                continue;
            }
            if late != 0 {
                let available = ringbuf::traits::Observer::occupied_len(consumer) / channels;
                let skipped = (late / frame_samples).min(available as u64) * frame_samples;
                played += consumer.skip(skipped as usize) as u64;
                late = late.saturating_sub(skipped);
                if late != 0 {
                    frame.fill_with(|| T::from_sample(0.));
                    if !shared.decode_done.load(Ordering::SeqCst) {
                        rendered += frame_samples;
                        late += frame_samples;
                    }
                    underran = true;
                    continue;
                }
            }
        }
        if take_silence(&shared.hold_silence_samples, frame_samples) {
            frame.fill_with(|| T::from_sample(0.));
            continue;
        }
        if !timed
            && (take_silence(&shared.gap_silence_samples, frame_samples)
                || take_silence(&shared.leading_silence_samples, frame_samples))
        {
            frame.fill_with(|| T::from_sample(0.));
            rendered += frame_samples;
            continue;
        }
        if ringbuf::traits::Observer::occupied_len(consumer) >= channels {
            for sample in frame {
                // This is the sole consumer; the producer cannot remove samples
                // between the whole-frame availability check and these pops.
                let value = consumer.try_pop().expect("checked complete audio frame");
                *sample = T::from_sample((value * gain).clamp(-1., 1.));
            }
            played += frame_samples;
            rendered += frame_samples;
            presented += frame_samples;
            presented_at = Some(position.saturating_add(rendered));
        } else {
            frame.fill_with(|| T::from_sample(0.));
            underran = true;
            if timed && !shared.decode_done.load(Ordering::SeqCst) {
                rendered += frame_samples;
                late += frame_samples;
            }
        }
    }
    // Unit tests can park an actual callback after consumption, before its
    // final clock commit. No hook or mutex exists in a production callback.
    #[cfg(test)]
    if let Some(gate) = shared.before_callback_commit.lock().unwrap().take() {
        gate.entered.send(()).unwrap();
        gate.release.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    shared.played_samples.fetch_add(played, Ordering::SeqCst);
    if let Some(position) = presented_at {
        shared
            .last_presented_position
            .store(position, Ordering::SeqCst);
    }
    shared
        .presented_samples
        .fetch_add(presented, Ordering::SeqCst);
    shared
        .rendered_samples
        .fetch_add(rendered, Ordering::SeqCst);
    if underran
        && shared.trim_to_prebuffer.load(Ordering::SeqCst)
        && !shared.decode_done.load(Ordering::SeqCst)
    {
        grow_live_prebuffer(shared);
    }
}

fn leading_silence_sample_count(
    timeline_origin_us: i64,
    audio_origin_us: i64,
    sample_rate: u32,
    channels: u16,
) -> u64 {
    samples_for_duration(
        sample_rate,
        channels,
        audio_origin_us.saturating_sub(timeline_origin_us).max(0) as u64,
    )
}

fn rendered_pts_us(origin_us: i64, samples: u64, sample_rate: u32, channels: u16) -> i64 {
    if sample_rate == 0 || channels == 0 {
        return origin_us;
    }
    let frames = samples / u64::from(channels);
    let elapsed_us = frames.saturating_mul(1_000_000) / u64::from(sample_rate);
    origin_us.saturating_add(i64::try_from(elapsed_us).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Observer;

    #[test]
    fn paused_clock_waits_for_the_consuming_callback_commit_and_is_output_scoped() {
        let format = Format {
            sample_rate: 48_000,
            channels: 2,
        };
        let (a, mut ar) = testing::manual(format, 1, 3840).unwrap();
        let (b, mut br) = testing::manual(format, 1, 3840).unwrap();
        for output in [&a, &b] {
            let channel = output.channel(1).unwrap();
            channel.observe_timed_queue(0, 0).unwrap();
            assert_eq!(channel.try_push(&[0.25; 3840]).unwrap(), 3840);
            output.control().configure_timed_queue(0, 0).unwrap();
            output.start();
        }
        let (entered, observed) = std::sync::mpsc::sync_channel(1);
        let (release, resume) = std::sync::mpsc::sync_channel(1);
        *a.shared.before_callback_commit.lock().unwrap() = Some(CallbackGate {
            entered,
            release: resume,
        });
        thread::scope(|scope| {
            let callback = scope.spawn(|| {
                let mut samples = [0.; 1920];
                ar.render(&mut samples).unwrap();
                assert_eq!(samples, [0.25; 1920]);
            });
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            a.pause();
            assert!(a.control().paused_clock().is_none());
            // The callback consumed PCM but has not published its final clock.
            assert_eq!(a.clock_pts(), Some(0));
            let mut samples = [0.; 1920];
            br.render(&mut samples).unwrap();
            assert_eq!(samples, [0.25; 1920]);
            assert_eq!(b.clock_pts(), Some(20_000));
            assert!(b.control().paused_clock().is_none());
            release.send(()).unwrap();
            callback.join().unwrap();
        });
        assert_eq!(a.control().paused_clock().unwrap().pts_us, Some(20_000));
        let mut samples = [1.; 1920];
        ar.render(&mut samples).unwrap();
        assert_eq!(samples, [0.; 1920]);
        assert_eq!(a.control().paused_clock().unwrap().pts_us, Some(20_000));
        a.start();
        assert!(a.control().paused_clock().is_none());
        ar.render(&mut samples).unwrap();
        assert_eq!(samples, [0.25; 1920]);
        a.stop();
        assert_eq!(a.control().paused_clock().unwrap().pts_us, Some(40_000));
        assert_eq!(b.clock_pts(), Some(20_000));
    }

    #[test]
    fn a_quiescent_unknown_clock_is_distinct_from_running_or_buffering() {
        let (output, mut renderer) = testing::manual(
            Format {
                sample_rate: 48_000,
                channels: 2,
            },
            1,
            3840,
        )
        .unwrap();
        assert_eq!(output.control().paused_clock().unwrap().pts_us, None);
        output.control().configure_timed_queue(0, 40_000).unwrap();
        output.start();
        let mut samples = [1.; 1920];
        renderer.render(&mut samples).unwrap();
        assert_eq!(samples, [0.; 1920]);
        assert!(output.control().paused_clock().is_none());
        output.pause();
        assert_eq!(output.control().paused_clock().unwrap().pts_us, Some(0));
    }

    #[test]
    fn timestamped_live_playback_preserves_audio_gap_audio_order_and_clock() {
        let output = Output::test_output();
        output.configure_timestamped_live(20_000);
        output.observe_audio_pts(1_000_000);
        output.start();
        let (mut producer, mut consumer) = HeapRb::<f32>::new(5760).split();
        for value in [0.25, 0.0, -0.5] {
            producer.push_slice(&[value; 1920]);
        }
        output.shared.queued_samples.store(5760, Ordering::SeqCst);
        for (index, value) in [0.25, 0.0, -0.5].into_iter().enumerate() {
            let mut samples = [1.0; 1920];
            render_audio(&mut samples, &mut consumer, &output.shared);
            assert_eq!(samples, [value; 1920]);
            assert_eq!(
                output.clock_pts(),
                Some(1_000_000 + (index as i64 + 1) * 20_000)
            );
        }
        assert_eq!(consumer.occupied_len(), 0);
        let mut underrun = [1.0; 960];
        render_audio(&mut underrun, &mut consumer, &output.shared);
        assert_eq!(underrun, [0.0; 960]);
        assert_eq!(output.clock_pts(), Some(1_060_000));
    }

    #[test]
    fn exhausted_accounting_cannot_wrap_or_mutate_the_device_ring() {
        let output = Output::test_output();
        output
            .shared
            .queued_samples
            .store(u64::MAX - 1, Ordering::SeqCst);
        assert!(output.push(&[0.; 2]).is_err());
        assert_eq!(
            output.shared.queued_samples.load(Ordering::SeqCst),
            u64::MAX - 1
        );
        assert_eq!(output.producer.lock().unwrap().occupied_len(), 0);
    }

    #[test]
    fn partially_published_input_never_moves_a_sample_to_another_channel() {
        let output = Output::test_output();
        output.observe_audio_pts(0);
        output.start();
        let (mut producer, mut consumer) = HeapRb::<f32>::new(4).split();
        producer.push_slice(&[0.25]);
        output.shared.queued_samples.store(1, Ordering::SeqCst);
        let mut frame = [1.; 2];
        render_audio(&mut frame, &mut consumer, &output.shared);
        assert_eq!(frame, [0.; 2]);
        assert_eq!(consumer.occupied_len(), 1);
        producer.push_slice(&[-0.75]);
        output.shared.queued_samples.store(2, Ordering::SeqCst);
        render_audio(&mut frame, &mut consumer, &output.shared);
        assert_eq!(frame, [0.25, -0.75]);
        assert_eq!(output.clock_pts(), Some(20));
    }

    #[test]
    fn live_prebuffer_trimming_keeps_a_partially_published_frame_aligned() {
        let output = Output::test_output();
        output.observe_audio_pts(0);
        output.shared.prebuffer_samples.store(2, Ordering::SeqCst);
        output
            .shared
            .trim_to_prebuffer
            .store(true, Ordering::SeqCst);
        output.start();
        let (mut producer, mut consumer) = HeapRb::<f32>::new(8).split();
        producer.push_slice(&[0.25, -0.75, 0.5]);
        output.shared.queued_samples.store(3, Ordering::SeqCst);
        let mut frame = [1.; 2];
        render_audio(&mut frame, &mut consumer, &output.shared);
        assert_eq!(
            frame,
            [0.25, -0.75],
            "live trimming swapped device channels"
        );
        assert_eq!(consumer.occupied_len(), 1);
        producer.push_slice(&[-0.5]);
        output.shared.queued_samples.store(4, Ordering::SeqCst);
        render_audio(&mut frame, &mut consumer, &output.shared);
        assert_eq!(frame, [0.5, -0.5]);
        assert_eq!(output.clock_pts(), Some(41));
    }

    #[test]
    fn replacement_channel_is_not_admitted_before_the_retirement_flush_finishes() {
        let output = Output::test_output();
        let peer = Output::test_output();
        let transition = output.shared.generation_transition.lock().unwrap();
        let worker_output = output.clone();
        let worker = thread::spawn(move || worker_output.advance_channel(1, 2));
        let deadline = Instant::now() + Duration::from_secs(2);
        while output.channel_generation() != 2 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(output.channel_generation(), 2);
        assert!(output.channel(1).is_err());
        assert!(
            output.channel(2).is_err(),
            "a new writer can race ahead of the flush that would discard its samples"
        );
        assert!(!output.advance_channel(2, 3));
        peer.channel(1).unwrap().push(&[0.25, -0.75]).unwrap();
        assert_eq!(peer.buffered_samples(), 2);
        drop(transition);
        assert!(worker.join().unwrap());
        output.channel(2).unwrap().push(&[0.5, -0.5]).unwrap();
        assert_eq!(output.buffered_samples(), 2);
        assert_eq!(peer.channel_generation(), 1);
    }

    #[test]
    fn a_single_vacant_channel_slot_cannot_publish_half_a_frame() {
        use std::sync::{atomic::AtomicUsize, mpsc};

        let mut output = Arc::try_unwrap(Output::test_output()).ok().unwrap();
        let (producer, mut consumer) = HeapRb::<f32>::new(4).split();
        output.producer = Mutex::new(producer);
        let output = Arc::new(output);
        output.push(&[0.25, -0.25, 0.5, -0.5]).unwrap();
        assert_eq!(consumer.try_pop(), Some(0.25));
        let (paused, ready) = mpsc::channel();
        let (proceed, resumed) = mpsc::channel();
        let writer = output.clone();
        let worker = thread::spawn(move || {
            let checks = AtomicUsize::new(0);
            writer.push_while(&[0.75, -0.75], || {
                if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                    paused.send(()).unwrap();
                    resumed.recv().unwrap();
                }
                true
            })
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let published = consumer.occupied_len();
        let old = [consumer.try_pop(), consumer.try_pop(), consumer.try_pop()];
        proceed.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(published, 3, "the producer published only a left channel");
        assert_eq!(old, [Some(-0.25), Some(0.5), Some(-0.5)]);
        assert_eq!(consumer.try_pop(), Some(0.75));
        assert_eq!(consumer.try_pop(), Some(-0.75));
        assert_eq!(output.shared.queued_samples.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn video_gate_stall_requires_enabled_and_absent_clock_progress() {
        let output = Output::test_output();
        assert!(!output.video_gate_stalled());

        output.configure_play(0, 100_000).unwrap();
        output.start();
        assert!(!output.video_gate_stalled());
        output.force_video_gate_stall_for_test();
        assert!(output.video_gate_stalled());
        assert_eq!(
            output.shared.error().as_deref(),
            Some("linked audio output clock stalled")
        );

        output.pause();
        assert!(!output.video_gate_stalled());
        output.flush();
        assert!(!output.video_gate_stalled());
        assert!(
            output
                .shared
                .play_configured_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none()
        );
    }

    #[test]
    fn a_live_track_is_buffered_to_the_target_latency_it_declared() {
        let output = Output::test_output();
        // 48 kHz stereo: 40 ms is 3,840 samples, 100 ms is 9,600.
        output.configure_live(40_000);
        assert_eq!(
            output.shared.prebuffer_samples.load(Ordering::SeqCst),
            3_840
        );

        // A target below the floor, and one above the presenter's own default, are both bounded.
        output.configure_live(1);
        assert_eq!(
            output.shared.prebuffer_samples.load(Ordering::SeqCst),
            1_920
        );
        output.configure_live(5_000_000);
        assert_eq!(
            output.shared.prebuffer_samples.load(Ordering::SeqCst),
            9_600
        );
    }

    #[test]
    fn a_live_track_stating_no_target_keeps_the_default_reserve_untrimmed() {
        let output = Output::test_output();
        output
            .shared
            .prebuffer_samples
            .store(9_600, Ordering::SeqCst);
        output.configure_live(0);
        assert_eq!(
            output.shared.prebuffer_samples.load(Ordering::SeqCst),
            9_600
        );

        // Nothing is discarded either: a producer that stated no target gets the conservative
        // reserve it would have got before, however deep the device start-up let it grow.
        let (mut producer, mut consumer) = HeapRb::<f32>::new(32_768).split();
        let buffered = producer.push_slice(&[0.5; 32_768]) as u64;
        output
            .shared
            .queued_samples
            .store(buffered, Ordering::SeqCst);
        trim_to_live_prebuffer(&output.shared, &mut consumer);
        assert_eq!(output.shared.played_samples.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn live_playback_starts_at_the_declared_reserve_however_deep_the_device_filled() {
        let output = Output::test_output();
        output.configure_live(40_000);
        let (mut producer, mut consumer) = HeapRb::<f32>::new(32_768).split();
        // A device that takes 250 ms to deliver its first callback leaves the ring six times
        // deeper than the track asked for, and every extra sample is permanent delay.
        let buffered = producer.push_slice(&[0.5; 24_000]) as u64;
        output
            .shared
            .queued_samples
            .store(buffered, Ordering::SeqCst);

        trim_to_live_prebuffer(&output.shared, &mut consumer);

        let excess = buffered - 3_840;
        assert_eq!(output.shared.played_samples.load(Ordering::SeqCst), excess);
        // The clock advances with the drop: the next audible sample is that much further in.
        assert_eq!(
            output.shared.rendered_samples.load(Ordering::SeqCst),
            excess
        );
        assert_eq!(consumer.occupied_len() as u64, 3_840);

        // Nothing more is taken once playback is at its reserve.
        trim_to_live_prebuffer(&output.shared, &mut consumer);
        assert_eq!(output.shared.played_samples.load(Ordering::SeqCst), excess);
    }

    #[test]
    fn timed_playback_keeps_every_sample_play_asked_it_to_buffer() {
        let output = Output::test_output();
        // A live generation may precede a timed one on the same device output.
        output.configure_live(40_000);
        output.configure_play(0, 100_000).unwrap();
        let (mut producer, mut consumer) = HeapRb::<f32>::new(32_768).split();
        let buffered = producer.push_slice(&[0.5; 24_000]) as u64;
        output
            .shared
            .queued_samples
            .store(buffered, Ordering::SeqCst);

        trim_to_live_prebuffer(&output.shared, &mut consumer);

        assert_eq!(output.shared.played_samples.load(Ordering::SeqCst), 0);
        assert_eq!(consumer.occupied_len() as u64, buffered);
    }

    #[test]
    fn a_live_reserve_the_link_could_not_hold_grows_back_toward_the_default() {
        let output = Output::test_output();
        output.configure_live(40_000);
        output.shared.prebuffered.store(true, Ordering::SeqCst);

        // 48 kHz stereo: one 20 ms step is 1,920 samples, the 100 ms ceiling is 9,600.
        assert!(grow_live_prebuffer(&output.shared));
        assert_eq!(
            output.shared.prebuffer_samples.load(Ordering::SeqCst),
            5_760
        );
        // Growing re-gates, so the reserve is actually rebuilt instead of underrunning again.
        assert!(!output.shared.prebuffered.load(Ordering::SeqCst));

        for expected in [7_680, 9_600] {
            assert!(grow_live_prebuffer(&output.shared));
            assert_eq!(
                output.shared.prebuffer_samples.load(Ordering::SeqCst),
                expected
            );
        }

        // At the ceiling the output is left exactly as it behaved before live targets existed:
        // the reserve stops growing and an underrun no longer re-gates.
        output.shared.prebuffered.store(true, Ordering::SeqCst);
        assert!(!grow_live_prebuffer(&output.shared));
        assert_eq!(
            output.shared.prebuffer_samples.load(Ordering::SeqCst),
            9_600
        );
        assert!(output.shared.prebuffered.load(Ordering::SeqCst));
    }

    #[test]
    fn a_reserve_this_presenter_did_not_tighten_is_never_grown_or_trimmed() {
        // Timed playback owns its own reserve through PLAY, and a live producer that stated no
        // target keeps the default. Neither is a reserve this presenter chose, so neither is one
        // it may revisit: the underrun path is gated on the same flag the trim is.
        let timed = Output::test_output();
        timed.configure_live(40_000);
        timed.configure_play(0, 100_000).unwrap();
        assert!(!timed.shared.trim_to_prebuffer.load(Ordering::SeqCst));

        let untargeted = Output::test_output();
        untargeted.configure_live(0);
        assert!(!untargeted.shared.trim_to_prebuffer.load(Ordering::SeqCst));
    }

    #[test]
    fn producer_push_accounts_an_accepted_slice_once() {
        let output = Output::test_output();
        let samples = [0.0, 0.25, -0.5, 1.0, -1.0, 0.75];
        output.push(&samples).unwrap();
        assert_eq!(
            output.shared.queued_samples.load(Ordering::SeqCst),
            samples.len() as u64
        );
    }

    #[test]
    fn a_timeline_reset_frees_the_whole_reserve_in_one_callback() {
        let output = Output::test_output();
        let (mut producer, mut consumer) = HeapRb::<f32>::new(4_096).split();
        // A seek arrives with the device reserve full, which is the steady state of timed audio.
        let buffered = producer.push_slice(&[0.5; 4_096]) as u64;
        assert_eq!(buffered, 4_096);
        output
            .shared
            .queued_samples
            .store(buffered, Ordering::SeqCst);
        output.flush();

        // One device period is a few hundred samples. Draining the reserve at that rate silenced
        // the output, and every video frame paced by its clock, for the whole two-second reserve.
        assert_eq!(
            discard_stale_samples(&output.shared, &mut consumer),
            buffered
        );
        assert_eq!(
            output.shared.played_samples.load(Ordering::SeqCst),
            buffered
        );
        assert_eq!(consumer.try_pop(), None);

        // The replacement timeline pushed behind the watermark survives untouched.
        assert_eq!(producer.push_slice(&[0.25; 8]), 8);
        output.shared.queued_samples.fetch_add(8, Ordering::SeqCst);
        assert_eq!(discard_stale_samples(&output.shared, &mut consumer), 0);
        assert_eq!(consumer.try_pop(), Some(0.25));
    }

    #[test]
    fn play_anchors_the_clock_on_the_audio_already_queued_for_the_new_timeline() {
        let output = Output::test_output();
        // The replacement channel can push before PLAY is dispatched: control and media travel on
        // independent connections. Its first packet straddles the seek target and is not trimmed,
        // so the clock has to start where that sample does or every frame is shown a packet early.
        output.observe_audio_pts(6_980_000);
        output.push(&[0.5; 8]).unwrap();
        output.configure_play(7_000_000, 0).unwrap();

        assert_eq!(
            output.shared.timeline_origin_us.load(Ordering::SeqCst),
            6_980_000
        );
        assert_eq!(
            output.shared.leading_silence_samples.load(Ordering::SeqCst),
            0
        );

        // Audio that starts after the requested target is leading silence, not a head start.
        let later = Output::test_output();
        later.configure_play(7_000_000, 0).unwrap();
        later.observe_audio_pts(7_010_000);
        assert_eq!(
            later.shared.timeline_origin_us.load(Ordering::SeqCst),
            7_000_000
        );
        assert_eq!(
            later.shared.leading_silence_samples.load(Ordering::SeqCst),
            leading_silence_sample_count(7_000_000, 7_010_000, 48_000, 2)
        );

        // A packet straddling an already published start is trimmed to it before it is pushed.
        let trimmed = Output::test_output();
        trimmed.configure_play(7_000_000, 0).unwrap();
        trimmed.observe_audio_pts(6_980_000);
        assert_eq!(
            trimmed.shared.timeline_origin_us.load(Ordering::SeqCst),
            7_000_000
        );
        assert_eq!(
            trimmed
                .shared
                .leading_silence_samples
                .load(Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn resume_preserves_the_clock_of_rendered_and_buffered_audio() {
        // The physical audio clock can be slightly ahead of a nested PLAY by the time that control
        // edge crosses the bridge. Its retained samples still continue the physical timeline, so
        // the nested request must not rewrite their PTS or the linked video will jump twice.
        let output = Output::test_output();
        output.observe_audio_pts(1_000_000);
        output
            .shared
            .queued_samples
            .store(192_000, Ordering::SeqCst);
        output.shared.played_samples.store(96_000, Ordering::SeqCst);
        output
            .shared
            .rendered_samples
            .store(96_000, Ordering::SeqCst);
        output.configure_play(1_950_000, 1).unwrap();
        output.start();
        output.shared.prebuffered.store(true, Ordering::SeqCst);

        assert_eq!(
            output.shared.timeline_origin_us.load(Ordering::SeqCst),
            1_000_000
        );
        assert_eq!(output.rendered_pts(), Some(2_000_000));
        assert_eq!(
            output.shared.first_audio_pts_us.load(Ordering::SeqCst),
            1_000_000
        );
        assert_eq!(output.shared.queued_samples.load(Ordering::SeqCst), 192_000);
        assert_eq!(output.shared.played_samples.load(Ordering::SeqCst), 96_000);
        assert_eq!(
            output.shared.discard_through_samples.load(Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn resume_waits_for_audio_accepted_after_play() {
        // Over vvssh, PLAY uses the control connection while audio uses the realtime connection.
        // The retained tail predates PLAY and cannot prove that the realtime path has caught up.
        let output = Output::test_output();
        output.observe_audio_pts(1_000_000);
        output.shared.queued_samples.store(20, Ordering::SeqCst);
        output.shared.played_samples.store(10, Ordering::SeqCst);
        output.shared.rendered_samples.store(10, Ordering::SeqCst);

        output.configure_play(1_000_104, 1).unwrap();

        assert!(!audio_prebuffer_ready(&output.shared));
        output.shared.queued_samples.fetch_add(1, Ordering::SeqCst);
        assert!(audio_prebuffer_ready(&output.shared));
    }

    #[test]
    fn resume_can_drain_a_full_ring_without_waiting_on_its_blocked_producer() {
        let mut output = Output::test_output();
        Arc::get_mut(&mut Arc::get_mut(&mut output).unwrap().shared)
            .unwrap()
            .ring_capacity_samples = 32;
        output.observe_audio_pts(1_000_000);
        output.shared.queued_samples.store(42, Ordering::SeqCst);
        output.shared.played_samples.store(10, Ordering::SeqCst);
        output.shared.rendered_samples.store(10, Ordering::SeqCst);

        output.configure_play(1_000_104, 1).unwrap();

        assert_eq!(output.shared.ring_capacity_samples, 32);
        assert!(audio_prebuffer_ready(&output.shared));
    }

    #[test]
    fn initial_play_does_not_require_a_post_play_audio_write() {
        let output = Output::test_output();
        output.configure_play(1_000_000, 0).unwrap();

        assert!(
            !output
                .shared
                .resume_waiting_for_audio
                .load(Ordering::SeqCst)
        );
        assert!(audio_prebuffer_ready(&output.shared));
    }

    #[test]
    fn channel_advance_reuses_the_output_but_retires_every_old_generation_write() {
        let output = Output::test_output();
        let retired = output.channel(1).unwrap();
        retired.observe_audio_pts(1_000_000).unwrap();
        retired.push(&[0.25; 8]).unwrap();
        output.configure_play(1_000_000, 1).unwrap();
        output.start();

        assert!(output.advance_channel(1, 2));
        assert_eq!(output.channel_generation(), 2);
        assert!(!output.shared.enabled.load(Ordering::SeqCst));
        assert_eq!(
            output.shared.timeline_origin_us.load(Ordering::SeqCst),
            UNSET_PTS
        );
        assert_eq!(
            output.shared.discard_through_samples.load(Ordering::SeqCst),
            8
        );
        assert_eq!(output.shared.discard_samples.load(Ordering::SeqCst), 0);
        assert_eq!(
            retired.push(&[0.5]).unwrap_err().kind(),
            io::ErrorKind::Interrupted,
            "the retired decoder must not contaminate the replacement clock"
        );

        let replacement = output.channel(2).unwrap();
        replacement.observe_audio_pts(7_000_000).unwrap();
        replacement.push(&[0.75; 4]).unwrap();
        assert_eq!(
            output.shared.timeline_origin_us.load(Ordering::SeqCst),
            7_000_000
        );
        assert_eq!(output.shared.queued_samples.load(Ordering::SeqCst), 12);
        assert!(!output.advance_channel(1, 3));
        assert_eq!(output.channel_generation(), 2);
    }

    #[test]
    fn video_gate_observes_audio_clock_progress_before_declaring_a_stall() {
        let output = Output::test_output();
        output.configure_play(0, 100_000).unwrap();
        output.start();
        output.force_video_gate_stall_for_test();
        output.shared.rendered_samples.store(256, Ordering::SeqCst);
        assert!(!output.video_gate_stalled());
        assert!(output.shared.error().is_none());
    }

    #[test]
    fn rendered_audio_frames_are_the_media_clock() {
        assert_eq!(rendered_pts_us(20_000, 48_000 * 2, 48_000, 2), 1_020_000);
        assert_eq!(leading_silence_sample_count(0, 100_000, 48_000, 2), 9_600);
        assert_eq!(leading_silence_sample_count(100_000, 0, 48_000, 2), 0);
    }

    #[test]
    fn live_video_discard_threshold_tracks_rendered_audio() {
        let output = Output::test_output();
        output.observe_audio_pts(1_000_000);
        output.start();
        output.shared.prebuffered.store(true, Ordering::SeqCst);
        output
            .shared
            .rendered_samples
            .store(48_000 * 2, Ordering::SeqCst);

        assert_eq!(output.discard_video_before(100_000), Some(1_900_000));
    }

    #[test]
    fn a_live_delay_holds_audio_back_without_moving_the_media_clock() {
        // Regression: audio played on arrival runs ahead of the picture it belongs with by the
        // transport's video delay. The hold is what puts them back together, and it must not be
        // mistaken for elapsed media time — a delay that advanced the clock would leave the video
        // exactly as late as it was before, and be discarded for it.
        let output = Output::test_output();
        output.observe_audio_pts(1_000_000);
        output.start();
        output.shared.prebuffered.store(true, Ordering::SeqCst);
        let before = output.rendered_pts();

        assert_eq!(output.request_live_delay(300_000), 300_000);
        assert_eq!(output.live_delay_us(), 300_000);
        assert_eq!(
            output.shared.hold_silence_samples.load(Ordering::SeqCst),
            48_000 * 2 * 3 / 10
        );
        assert_eq!(
            output.rendered_pts(),
            before,
            "a hold is not elapsed media time"
        );

        // Jitter-sized changes are ignored, and the hold is bounded.
        assert_eq!(output.request_live_delay(310_000), 300_000);
        assert_eq!(output.request_live_delay(u64::MAX), MAXIMUM_LIVE_DELAY_US);
    }

    #[test]
    fn shrinking_a_live_delay_discards_the_samples_it_no_longer_holds() {
        let output = Output::test_output();
        output.request_live_delay(500_000);

        assert_eq!(output.request_live_delay(200_000), 200_000);
        assert_eq!(output.live_delay_us(), 200_000);
        assert_eq!(
            output.shared.discard_samples.load(Ordering::SeqCst),
            48_000 * 2 * 3 / 10
        );
    }

    #[test]
    fn bridging_a_gap_advances_the_clock_and_keeps_every_buffered_sample() {
        // The producer dropped 120 ms. The timeline is intact, so the clock must move over the
        // hole while the audio already buffered behind it survives; restarting the timeline for
        // this is audible and used to happen for any step over 2 ms.
        let output = Output::test_output();
        output.observe_audio_pts(1_000_000);
        output.start();
        output.shared.prebuffered.store(true, Ordering::SeqCst);
        output.shared.queued_samples.store(9_600, Ordering::SeqCst);

        output.bridge_live_gap(120_000).unwrap();
        // The packet that follows the gap is observed like any other. Leading silence is
        // recomputed and *stored* on every observation, so a bridge kept there would be erased
        // before a single sample of it reached the device.
        output.observe_audio_pts(1_200_000);

        assert_eq!(
            output.shared.gap_silence_samples.load(Ordering::SeqCst),
            48_000 * 2 * 12 / 100
        );
        assert_eq!(
            output.shared.leading_silence_samples.load(Ordering::SeqCst),
            0,
            "the observation really does clear leading silence, which is why the bridge is separate"
        );
        assert_eq!(output.shared.discard_samples.load(Ordering::SeqCst), 0);
        assert_eq!(output.shared.queued_samples.load(Ordering::SeqCst), 9_600);
        assert_eq!(
            output.shared.timeline_origin_us.load(Ordering::SeqCst),
            1_000_000,
            "a gap does not restart the timeline"
        );
    }

    #[test]
    fn live_rebase_discards_old_samples_and_restarts_the_clock() {
        let output = Output::test_output();
        output.observe_audio_pts(1_000_000);
        output.start();
        output.shared.queued_samples.store(960, Ordering::SeqCst);
        output.shared.played_samples.store(480, Ordering::SeqCst);

        output.rebase_live(2_000_000);

        assert!(output.shared.enabled.load(Ordering::SeqCst));
        assert_eq!(
            output.shared.timeline_origin_us.load(Ordering::SeqCst),
            2_000_000
        );
        assert_eq!(
            output.shared.discard_through_samples.load(Ordering::SeqCst),
            960
        );
        assert_eq!(output.shared.discard_samples.load(Ordering::SeqCst), 0);
        assert!(!output.shared.prebuffered.load(Ordering::SeqCst));
    }

    #[test]
    #[ignore = "requires the machine's default audio output device"]
    fn default_audio_device_renders_vvdoom_float_pcm() {
        let output = Output::open(1).unwrap();
        let sample_rate = output.sample_rate;
        let channels = usize::from(output.channels);
        let frame_count = sample_rate as usize / 2;
        let samples = (0..frame_count)
            .flat_map(|frame| {
                let phase = frame as f32 * 440.0 * std::f32::consts::TAU / sample_rate as f32;
                std::iter::repeat_n(phase.sin() * 0.2, channels)
            })
            .collect::<Vec<_>>();
        output.observe_audio_pts(0);
        output.start();
        output.push(&samples).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while output.shared.played_samples.load(Ordering::SeqCst) == 0 && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        output.stop();
        assert!(output.shared.played_samples.load(Ordering::SeqCst) > 0);
        assert!(output.shared.error().is_none());
    }

    #[test]
    fn gain_updates_without_changing_audio_clock_state() {
        let output = Output::test_output();
        output.shared.rendered_samples.store(42, Ordering::SeqCst);
        output.set_gain(AudioGain::from_percent(175).unwrap());
        assert_eq!(
            f32::from_bits(output.shared.gain_bits.load(Ordering::SeqCst)),
            1.75
        );
        assert_eq!(output.shared.rendered_samples.load(Ordering::SeqCst), 42);
    }
}
