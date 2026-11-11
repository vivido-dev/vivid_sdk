//! Bounded native audio decoding and resampling for Vivid application hosts.
//!
//! Use one decoder on its owning media worker after authenticating media and
//! admitting its resource contract. This crate opens no device or event loop;
//! playback, flow control, mixing and presentation receipts belong to the host.

// Rust guideline compliant 2026-10-07
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::{io, ptr};
use vivid_protocol::media::ParsedAudioPacket;
use vivid_protocol::track::AudioConfiguration;
use vivid_video::ffi::{self as ffmpeg, AVPacket, AVRational, ParameterValues};

mod timing;
use timing::Clock;
#[doc(inline)]
pub use timing::{DecodedBatch, DecodedSpan};

const AVMEDIA_TYPE_AUDIO: c_int = 1;
const AV_INPUT_BUFFER_PADDING_SIZE: usize = 64;
const AV_SAMPLE_FMT_FLT: c_int = 3;
const AVERROR_EOF: c_int = -541_478_725;
// Bound one interleaved f32 batch to 64 MiB, independently of native internals.
const MAX_OUTPUT_SAMPLES: usize = 16 * 1024 * 1024;
// Also bound progress when decoding or draining produces empty output.
const MAX_OUTPUT_FRAMES: usize = 256;
const PACKET_TIME_BASE: AVRational = AVRational {
    num: 1,
    den: 1_000_000,
};

/// Checks whether the canonical configuration has an available native decoder.
///
/// Allocates and releases a decoder without opening an audio device.
pub fn supports(config: &AudioConfiguration) -> bool {
    Decoder::new(config, config.sample_rate, u16::from(config.channels)).is_ok()
}

/// Resource limits fixed before native decoder allocation.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Largest scratch buffer and combined batch, in interleaved f32 samples.
    ///
    /// Between one and 16,777,216 samples (64 MiB).
    pub max_output_samples: usize,
    /// Decoded frames plus nonempty resampler drain iterations per call, at most 256.
    pub max_output_frames: usize,
    /// Native threads, from one through 64; defaults to one.
    pub threads: u16,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_output_samples: MAX_OUTPUT_SAMPLES,
            max_output_frames: MAX_OUTPUT_FRAMES,
            threads: 1,
        }
    }
}

impl Limits {
    fn validate(self) -> io::Result<()> {
        if !(1..=MAX_OUTPUT_SAMPLES).contains(&self.max_output_samples)
            || !(1..=MAX_OUTPUT_FRAMES).contains(&self.max_output_frames)
            || !(1..=64).contains(&self.threads)
        {
            return Err(invalid("invalid audio decode limits"));
        }
        Ok(())
    }

    fn output_len(self, frames: c_int, channels: c_int, retained: usize) -> io::Result<usize> {
        usize::try_from(frames)
            .ok()
            .and_then(|frames| frames.checked_mul(channels as usize))
            .filter(|count| {
                retained
                    .checked_add(*count)
                    .is_some_and(|n| n <= self.max_output_samples)
            })
            .ok_or_else(|| invalid("audio output exceeds sample budget"))
    }
}

#[repr(C)]
#[derive(Default)]
struct AVChannelLayout {
    order: c_int,
    nb_channels: c_int,
    mask: u64,
    opaque: *mut c_void,
}

/// Worker-owned canonical audio decoder returning interleaved float samples.
///
/// Deliberately neither Send nor Sync. Retire it after any decode error. Limits
/// cover conversion buffers and returned batches; codec internals and native
/// call duration remain additional. No device or event loop is created.
pub struct Decoder {
    /// The verified layout of the FFmpeg this decoder reads structures from.
    abi: &'static ffmpeg::Abi,
    context: *mut c_void,
    packet: *mut c_void,
    frame: *mut c_void,
    resampler: *mut c_void,
    // Metadata-only resampler: reads AVFrame properties through FFmpeg's own
    // ABI, without guessing offsets in the version-dependent frame tail.
    validation: *mut c_void,
    input_rate: c_int,
    input_channels: c_int,
    output_rate: c_int,
    output_channels: c_int,
    pending_trim_start: usize,
    pending_trim_end: usize,
    limits: Limits,
    maximum_packet_bytes: usize,
    input_mask: u64,
    input_format: Option<(c_int, c_int, u64)>,
    failed: bool,
    finished: bool,
    clock: Option<Clock>,
    timeline_serial: u64,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("input_rate", &self.input_rate)
            .field("input_channels", &self.input_channels)
            .field("output_rate", &self.output_rate)
            .field("output_channels", &self.output_channels)
            .field("limits", &self.limits)
            .field("failed", &self.failed)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Decoder {
    /// Creates a decoder with default resource limits.
    ///
    /// # Errors
    /// Returns an error for invalid configuration, unsupported native codec/ABI,
    /// invalid output format, or native allocation/setup failure.
    pub fn new(
        config: &AudioConfiguration,
        output_rate: u32,
        output_channels: u16,
    ) -> io::Result<Self> {
        Self::with_limits(config, output_rate, output_channels, Limits::default())
    }

    /// Creates a decoder with explicit conversion and batch budgets.
    ///
    /// Output supports 8–384 kHz and one through 32 interleaved channels.
    /// Input follows the canonical Vivid audio configuration.
    ///
    /// # Errors
    /// Returns an error for invalid configuration/limits, unavailable codec/ABI,
    /// or native allocation/setup failure.
    pub fn with_limits(
        config: &AudioConfiguration,
        output_rate: u32,
        output_channels: u16,
        limits: Limits,
    ) -> io::Result<Self> {
        limits.validate()?;
        vivid_protocol::media::validate_audio_initialization(
            vivid_protocol::media::AudioInitialization {
                codec: &config.codec,
                packetization: &config.packetization,
                extradata: &config.extradata,
                sample_rate: config.sample_rate,
                channels: u16::from(config.channels),
            },
        )?;
        if !(8_000..=192_000).contains(&config.sample_rate)
            || !(1..=8).contains(&config.channels)
            || !(1..=1_048_576).contains(&config.maximum_access_unit_bytes)
            || (config.channel_mask != 0
                && config.channel_mask.count_ones() != u32::from(config.channels))
            || config
                .codec_string
                .as_ref()
                .is_some_and(|value| value.len() > 64)
            || !(8_000..=384_000).contains(&output_rate)
            || !(1..=32).contains(&output_channels)
        {
            return Err(invalid("audio configuration exceeds decode limits"));
        }
        let input_rate = c_int::try_from(config.sample_rate)
            .map_err(|_| invalid("audio sample rate exceeds FFmpeg limits"))?;
        let output_rate = c_int::try_from(output_rate)
            .map_err(|_| invalid("output sample rate exceeds FFmpeg limits"))?;
        if input_rate == 0 || output_rate == 0 || config.channels == 0 || output_channels == 0 {
            return Err(invalid("audio rates and channel counts must be positive"));
        }
        // Verify the linked FFmpeg's structure layout before touching any of it.
        let abi = ffmpeg::abi()?;
        let name =
            CString::new(config.codec.as_str()).map_err(|_| invalid("audio codec has NUL"))?;
        // SAFETY: the CString is NUL-terminated and live through the call; the returned descriptor is library-owned.
        let codec = unsafe { avcodec_find_decoder_by_name(name.as_ptr()) };
        if codec.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("FFmpeg decoder {:?} is unavailable", config.codec),
            ));
        }
        // SAFETY: codec is a checked live library descriptor; this allocation is owned here and null is handled.
        let mut context = unsafe { avcodec_alloc_context3(codec) };
        let mut parameters = match ffmpeg::allocate_parameters() {
            Ok(parameters) => parameters,
            Err(error) => {
                // SAFETY: these are uniquely owned FFmpeg allocations; each matching free function accepts null and clears its pointer.
                unsafe { avcodec_free_context(&mut context) };
                return Err(error);
            }
        };
        if context.is_null() {
            // SAFETY: parameters is uniquely owned here and is no longer used after release.
            unsafe { ffmpeg::free_parameters(&mut parameters) };
            return Err(io::Error::other(
                "FFmpeg could not allocate audio decoder state",
            ));
        }
        let result = (|| {
            let extradata = if config.extradata.is_empty() {
                None
            } else {
                let length = c_int::try_from(config.extradata.len())
                    .map_err(|_| invalid("audio extradata exceeds i32"))?;
                let size = config
                    .extradata
                    .len()
                    .checked_add(AV_INPUT_BUFFER_PADDING_SIZE)
                    .ok_or_else(|| invalid("audio extradata size overflows"))?;
                // SAFETY: the checked size includes FFmpeg padding; allocation failure is handled before writing.
                let extradata: *mut u8 = unsafe { av_mallocz(size) }.cast();
                if extradata.is_null() {
                    return Err(io::Error::other(
                        "FFmpeg could not allocate audio extradata",
                    ));
                }
                // SAFETY: the source slice and freshly allocated destination are disjoint and both contain the checked packet/extradata length.
                unsafe {
                    ptr::copy_nonoverlapping(
                        config.extradata.as_ptr(),
                        extradata,
                        config.extradata.len(),
                    )
                };
                Some((extradata, length))
            };
            // SAFETY: parameters is uniquely owned and the verified ABI matches; padded av_malloc extradata transfers to it.
            unsafe {
                abi.set_parameters(
                    parameters,
                    AVMEDIA_TYPE_AUDIO,
                    ffmpeg::codec_id(codec),
                    ParameterValues {
                        extradata,
                        ..Default::default()
                    },
                );
            }
            // SAFETY: context and parameters are live owned allocations; FFmpeg copies parameters without retaining this borrow.
            check_ffmpeg("could not configure audio decoder", unsafe {
                avcodec_parameters_to_context(context, parameters)
            })?;
            // SAFETY: the decoder context is live, option names are NUL-terminated, and output arguments are initialized writable locals.
            check_ffmpeg("could not set audio packet time base", unsafe {
                av_opt_set_q(context, c"pkt_timebase".as_ptr(), PACKET_TIME_BASE, 0)
            })?;
            let ar = c"ar";
            // SAFETY: context is live; both named scalar options are supported
            // by the verified FFmpeg versions. max_samples counts all channels.
            unsafe {
                check_ffmpeg(
                    "could not bound native audio samples",
                    av_opt_set_int(
                        context,
                        c"max_samples".as_ptr(),
                        limits.max_output_samples as i64,
                        0,
                    ),
                )?;
                check_ffmpeg(
                    "could not bound audio threads",
                    av_opt_set_int(context, c"threads".as_ptr(), i64::from(limits.threads), 0),
                )?;
            }
            // SAFETY: the decoder context is live, option names are NUL-terminated, and output arguments are initialized writable locals.
            check_ffmpeg("could not set audio sample rate", unsafe {
                av_opt_set_int(context, ar.as_ptr(), i64::from(config.sample_rate), 0)
            })?;
            let mut layout = AVChannelLayout::default();
            if config.channel_mask == 0 {
                // SAFETY: layout is initialized writable storage; FFmpeg initializes its layout data before use.
                unsafe { av_channel_layout_default(&mut layout, c_int::from(config.channels)) };
            } else {
                // SAFETY: layout is initialized writable storage, and the native function validates the scalar mask.
                check_ffmpeg("could not set audio channel mask", unsafe {
                    av_channel_layout_from_mask(&mut layout, config.channel_mask)
                })?;
            }
            let layout_result =
                // SAFETY: the decoder context is live, option names are NUL-terminated, and output arguments are initialized writable locals.
                unsafe { av_opt_set_chlayout(context, c"ch_layout".as_ptr(), &layout, 0) };
            // SAFETY: layout was initialized above and is no longer borrowed by the native option setter.
            unsafe { av_channel_layout_uninit(&mut layout) };
            check_ffmpeg("could not set audio channel layout", layout_result)?;
            // SAFETY: context is owned and configured, codec is library-owned, and null requests default options.
            check_ffmpeg("could not open audio decoder", unsafe {
                avcodec_open2(context, codec, ptr::null_mut())
            })
        })();
        // SAFETY: parameters is uniquely owned here and is no longer used after release.
        unsafe { ffmpeg::free_parameters(&mut parameters) };
        if let Err(error) = result {
            // SAFETY: these are uniquely owned FFmpeg allocations; each matching free function accepts null and clears its pointer.
            unsafe { avcodec_free_context(&mut context) };
            return Err(error);
        }
        // SAFETY: the native allocator returns an owned packet pointer; null is checked before use.
        let packet = unsafe { av_packet_alloc() };
        // SAFETY: the native allocator returns an owned frame pointer; null is checked before use.
        let frame = unsafe { av_frame_alloc() };
        if packet.is_null() || frame.is_null() {
            let mut packet = packet;
            let mut frame = frame;
            // SAFETY: these are uniquely owned FFmpeg allocations; each matching free function accepts null and clears its pointer.
            unsafe {
                av_packet_free(&mut packet);
                av_frame_free(&mut frame);
                avcodec_free_context(&mut context);
            }
            return Err(io::Error::other(
                "FFmpeg could not allocate audio decode buffers",
            ));
        }
        Ok(Self {
            abi,
            context,
            packet,
            frame,
            resampler: ptr::null_mut(),
            validation: ptr::null_mut(),
            input_rate,
            input_channels: c_int::from(config.channels),
            output_rate,
            output_channels: c_int::from(output_channels),
            pending_trim_start: 0,
            pending_trim_end: 0,
            limits,
            maximum_packet_bytes: config.maximum_access_unit_bytes as usize,
            input_mask: config.channel_mask,
            input_format: None,
            failed: false,
            finished: false,
            clock: None,
            timeline_serial: 0,
        })
    }

    /// Decodes one packet, retaining trim until delayed output arrives.
    ///
    /// FFmpeg applies OpusHead pre-skip once; callers must not repeat it in trim.
    /// Returned samples own their storage independently of this decoder.
    ///
    /// # Errors
    /// Returns an error for oversized packets, exhausted trim/output budgets,
    /// format changes, native failure, or a retired decoder.
    pub fn push(&mut self, packet: ParsedAudioPacket<'_>) -> io::Result<Vec<f32>> {
        self.push_timed(packet).map(DecodedBatch::into_samples)
    }

    /// Decodes samples with timing derived from actual native frames.
    ///
    /// Resampler delay and protocol trim retain their sample positions. A
    /// decoded timestamp discontinuity drains the old filter before starting a
    /// new conversion timeline; delayed samples never acquire a new anchor.
    ///
    /// # Errors
    /// Returns decode/resource errors or exhausted timestamp/metadata bounds.
    pub fn push_timed(&mut self, packet: ParsedAudioPacket<'_>) -> io::Result<DecodedBatch> {
        self.running()?;
        let result = self.push_inner(packet);
        self.failed = result.is_err();
        result
    }

    fn push_inner(&mut self, packet: ParsedAudioPacket<'_>) -> io::Result<DecodedBatch> {
        if packet.data.len() > self.maximum_packet_bytes {
            return Err(invalid("audio packet exceeds immutable configuration"));
        }
        let duration = i64::try_from(packet.duration_us)
            .map_err(|_| invalid("audio duration exceeds signed timebase"))?;
        let (input_rate, output_rate, output_channels, max_output_samples) = (
            self.input_rate,
            self.output_rate,
            self.output_channels,
            self.limits.max_output_samples,
        );
        let trim = |pending: usize, amount| {
            pending
                .checked_add(converted_trim_samples(
                    amount,
                    input_rate,
                    output_rate,
                    output_channels,
                ))
                .filter(|total| *total <= max_output_samples)
                .ok_or_else(|| invalid("pending audio trim exceeds sample budget"))
        };
        trim(self.pending_trim_start, packet.trim_start_samples)?;
        trim(self.pending_trim_end, packet.trim_end_samples)?;
        if packet.data.is_empty() {
            return Ok(DecodedBatch::new(
                self.output_rate as u32,
                self.output_channels as u16,
            ));
        }
        let mut samples = DecodedBatch::new(self.output_rate as u32, self.output_channels as u16);
        let mut work = 0;
        if packet.trim_start_samples != 0 || packet.trim_end_samples != 0 {
            // Complete the previous packet's conversion before applying this
            // packet's trim. Filter delay must not transfer trim across packets.
            self.drain_resampler(&mut samples, &mut work)?;
            samples.trim(&mut self.pending_trim_start, &mut self.pending_trim_end)?;
            self.reset_conversion();
        }
        let prefix = samples.samples.len();
        let start = trim(self.pending_trim_start, packet.trim_start_samples)?;
        let end = trim(self.pending_trim_end, packet.trim_end_samples)?;
        let size = c_int::try_from(packet.data.len())
            .map_err(|_| invalid("audio packet exceeds native size"))?;
        // SAFETY: self uniquely owns the initialized packet; length fits c_int.
        check_ffmpeg("could not allocate audio packet", unsafe {
            av_new_packet(self.packet, size)
        })?;
        // SAFETY: the verified packet layout is live and exclusively borrowed.
        let native = unsafe { &mut *(self.packet as *mut AVPacket) };
        // SAFETY: disjoint input and freshly allocated output contain this length.
        unsafe { ptr::copy_nonoverlapping(packet.data.as_ptr(), native.data, packet.data.len()) };
        native.pts = packet.pts_us;
        native.dts = packet.dts_us;
        native.duration = duration;
        native.time_base = PACKET_TIME_BASE;
        // SAFETY: the owned context and packet are initialized; decoder retains its references.
        let result = unsafe { avcodec_send_packet(self.context, self.packet) };
        // SAFETY: the packet is no longer borrowed by this call.
        unsafe { av_packet_unref(self.packet) };
        check_ffmpeg("audio decoder rejected packet", result)?;
        self.pending_trim_start = start;
        self.pending_trim_end = end;
        self.receive_into(&mut samples, &mut work)?;
        if self.pending_trim_end != 0 && samples.samples.len() > prefix {
            // End padding is at the end of all converted samples, including
            // the filter tail, rather than the end of its first returned chunk.
            self.drain_resampler(&mut samples, &mut work)?;
            self.reset_conversion();
        }
        samples.trim_from(
            prefix,
            &mut self.pending_trim_start,
            &mut self.pending_trim_end,
        )?;
        Ok(samples)
    }

    /// Drains decoder and resampler once and applies remaining trim.
    ///
    /// # Errors
    /// Returns an error for native failure, output/work exhaustion, or use after
    /// finish or a prior error. Playback and EOS receipts remain the host's job.
    pub fn finish(&mut self) -> io::Result<Vec<f32>> {
        self.finish_timed().map(DecodedBatch::into_samples)
    }

    /// Drains native decoding and resampling with their original timing.
    ///
    /// # Errors
    /// Returns decode/resource errors or exhausted timestamp/metadata bounds.
    pub fn finish_timed(&mut self) -> io::Result<DecodedBatch> {
        self.running()?;
        self.finished = true;
        let result = self.finish_inner();
        self.failed = result.is_err();
        result
    }

    fn running(&self) -> io::Result<()> {
        if self.failed || self.finished {
            return Err(invalid("audio decoder is retired"));
        }
        Ok(())
    }

    fn step(&self, work: &mut usize) -> io::Result<()> {
        *work += 1;
        if *work > self.limits.max_output_frames {
            return Err(invalid("audio decode work exceeds frame budget"));
        }
        Ok(())
    }

    fn finish_inner(&mut self) -> io::Result<DecodedBatch> {
        // SAFETY: the exclusively borrowed initialized context accepts null to drain.
        let result = unsafe { avcodec_send_packet(self.context, ptr::null()) };
        if result < 0 && result != AVERROR_EOF {
            return Err(ffmpeg_error("could not drain audio decoder", result));
        }
        let mut work = 0;
        let mut samples = self.receive(&mut work)?;
        self.drain_resampler(&mut samples, &mut work)?;
        samples.trim(&mut self.pending_trim_start, &mut self.pending_trim_end)?;
        Ok(samples)
    }

    fn drain_resampler(&mut self, samples: &mut DecodedBatch, work: &mut usize) -> io::Result<()> {
        if !self.resampler.is_null() {
            loop {
                // SAFETY: initialized resampler is exclusively borrowed; scalar query retains nothing.
                let delay = unsafe { swr_get_delay(self.resampler, i64::from(self.output_rate)) };
                if delay <= 0 {
                    break;
                }
                // 4096 frames amortize the filter tail; tighter budgets reduce the chunk.
                let capacity = 4096.min(
                    (self
                        .limits
                        .max_output_samples
                        .saturating_sub(samples.samples.len())
                        / self.output_channels as usize) as c_int,
                );
                if capacity == 0 {
                    return Err(invalid("audio drain exceeds sample budget"));
                }
                let length = self.limits.output_len(
                    capacity,
                    self.output_channels,
                    samples.samples.len(),
                )?;
                let mut output = vec![0.0_f32; length];
                let mut planes = [output.as_mut_ptr().cast::<u8>()];
                // SAFETY: output has the declared interleaved capacity; null input drains this owned resampler.
                let converted = unsafe {
                    swr_convert(
                        self.resampler,
                        planes.as_mut_ptr(),
                        capacity,
                        ptr::null(),
                        0,
                    )
                };
                check_ffmpeg("could not drain audio resampler", converted)?;
                if converted == 0 {
                    break;
                }
                self.step(work)?;
                output.truncate(converted as usize * self.output_channels as usize);
                samples.append(
                    &output,
                    self.clock.as_mut().expect("initialized conversion clock"),
                    self.limits.max_output_frames,
                )?;
            }
        }
        Ok(())
    }

    fn receive(&mut self, work: &mut usize) -> io::Result<DecodedBatch> {
        let mut samples = DecodedBatch::new(self.output_rate as u32, self.output_channels as u16);
        self.receive_into(&mut samples, work)?;
        Ok(samples)
    }

    fn reset_conversion(&mut self) {
        // SAFETY: the exclusively owned resampler has been drained; the native
        // function accepts null and clears the pointer before the next frame.
        unsafe { swr_free(&mut self.resampler) };
        self.clock = None;
    }

    fn receive_into(&mut self, samples: &mut DecodedBatch, work: &mut usize) -> io::Result<()> {
        loop {
            // SAFETY: both owned allocations are live; success initializes this frame.
            let result = unsafe { avcodec_receive_frame(self.context, self.frame) };
            if result == -libc::EAGAIN || result == AVERROR_EOF {
                break;
            }
            check_ffmpeg("could not receive decoded audio", result)?;
            self.step(work)?;
            // SAFETY: successfully decoded frame belongs to this verified ABI.
            let frame = unsafe { self.abi.frame(self.frame) };
            if frame.nb_samples > 0 {
                self.validate_format(frame.format)?;
                // SAFETY: receive_frame initialized this live owned frame, and
                // the selected ABI verified the native timestamp field layout.
                let pts = unsafe { self.abi.frame_pts(self.frame) };
                let pts = (pts != i64::MIN).then_some(pts);
                let follows = self
                    .clock
                    .as_ref()
                    .map(|clock| clock.follows(pts, self.input_rate as u32))
                    .transpose()?
                    .unwrap_or(false);
                if !follows {
                    self.drain_resampler(samples, work)?;
                    // SAFETY: this decoder uniquely owns the resampler, which
                    // was drained above; free clears its pointer before reuse.
                    unsafe { swr_free(&mut self.resampler) };
                    self.timeline_serial = self
                        .timeline_serial
                        .checked_add(1)
                        .ok_or_else(|| invalid("audio timeline serial exhausted"))?;
                    self.clock = Some(Clock {
                        timeline: self.timeline_serial,
                        origin: pts,
                        input_frames: 0,
                        output_frames: 0,
                    });
                }
                if self.resampler.is_null() {
                    self.init_resampler(frame.format)?;
                }
                // SAFETY: initialized resampler is exclusively borrowed; includes filter delay.
                let maximum = unsafe { swr_get_out_samples(self.resampler, frame.nb_samples) };
                check_ffmpeg("could not size resampled audio", maximum)?;
                let length =
                    self.limits
                        .output_len(maximum, self.output_channels, samples.samples.len())?;
                let mut output = vec![0.0_f32; length];
                let mut planes = [output.as_mut_ptr().cast::<u8>()];
                let input = if frame.extended_data.is_null() {
                    frame.data.as_ptr() as *const *const u8
                } else {
                    frame.extended_data as *const *const u8
                };
                // SAFETY: output capacity is checked; input planes belong to the live frame with verified format.
                let converted = unsafe {
                    swr_convert(
                        self.resampler,
                        planes.as_mut_ptr(),
                        maximum,
                        input,
                        frame.nb_samples,
                    )
                };
                check_ffmpeg("could not resample audio", converted)?;
                output.truncate(converted as usize * self.output_channels as usize);
                let clock = self.clock.as_mut().expect("initialized conversion clock");
                clock.input_frames = clock
                    .input_frames
                    .checked_add(frame.nb_samples as u64)
                    .ok_or_else(|| invalid("audio input timeline exhausted"))?;
                samples.append(&output, clock, self.limits.max_output_frames)?;
            }
            // SAFETY: no sample or plane reference survives releasing the owned frame.
            unsafe { av_frame_unref(self.frame) };
        }
        Ok(())
    }

    fn validate_format(&mut self, format: c_int) -> io::Result<()> {
        if self.validation.is_null() {
            // SAFETY: the allocator returns an exclusively owned native context.
            self.validation = unsafe { swr_alloc() };
            if self.validation.is_null() {
                return Err(io::Error::other("could not allocate audio frame validator"));
            }
        }
        let mut rate = 0;
        let mut layout = AVChannelLayout::default();
        // SAFETY: the successfully decoded frame is live and the validator is
        // exclusively owned. swr_config_frame copies metadata without converting
        // samples or retaining the frame. No frame-tail offsets are read in Rust.
        unsafe {
            check_ffmpeg(
                "could not inspect decoded audio frame",
                swr_config_frame(self.validation, ptr::null(), self.frame),
            )?;
            check_ffmpeg(
                "could not query decoded audio rate",
                av_opt_get_int(self.validation, c"isr".as_ptr(), 0, &mut rate),
            )?;
            check_ffmpeg(
                "could not query decoded audio layout",
                av_opt_get_chlayout(self.validation, c"ichl".as_ptr(), 0, &mut layout),
            )?;
        }
        let channels = layout.nb_channels;
        let signature = (format, layout.order, layout.mask);
        // SAFETY: getter initialized this owned layout; no references escape.
        unsafe { av_channel_layout_uninit(&mut layout) };
        if rate != i64::from(self.input_rate)
            || channels != self.input_channels
            || (self.input_mask != 0 && (signature.1 != 1 || signature.2 != self.input_mask))
            || self
                .input_format
                .is_some_and(|previous| previous != signature)
        {
            return Err(invalid(
                "decoded audio differs from immutable configuration",
            ));
        }
        self.input_format = Some(signature);
        Ok(())
    }

    fn init_resampler(&mut self, input_format: c_int) -> io::Result<()> {
        let mut input = AVChannelLayout::default();
        let mut output = AVChannelLayout::default();
        // SAFETY: layout is initialized writable storage; FFmpeg initializes its layout data before use.
        unsafe {
            check_ffmpeg(
                "could not read decoder channel layout",
                av_opt_get_chlayout(self.validation, c"ichl".as_ptr(), 0, &mut input),
            )?;
            av_channel_layout_default(&mut output, self.output_channels);
        }
        // SAFETY: input and output layouts are initialized and live; the output pointer receives this decoder's owned resampler.
        let result = unsafe {
            swr_alloc_set_opts2(
                &mut self.resampler,
                &output,
                AV_SAMPLE_FMT_FLT,
                self.output_rate,
                &input,
                input_format,
                self.input_rate,
                0,
                ptr::null_mut(),
            )
        };
        // SAFETY: layout was initialized above and is no longer borrowed by the native option setter.
        unsafe {
            av_channel_layout_uninit(&mut input);
            av_channel_layout_uninit(&mut output);
        }
        check_ffmpeg("could not allocate audio resampler", result)?;
        // SAFETY: the decoder context is live, option names are NUL-terminated, and output arguments are initialized writable locals.
        check_ffmpeg("could not initialize audio resampler", unsafe {
            swr_init(self.resampler)
        })
    }

    #[cfg(test)]
    fn packet_time_base(&self) -> io::Result<AVRational> {
        let mut time_base = AVRational { num: 0, den: 0 };
        // SAFETY: the live decoder writes a rational into this initialized local through a valid pointer.
        check_ffmpeg("could not read audio packet time base", unsafe {
            av_opt_get_q(self.context, c"pkt_timebase".as_ptr(), 0, &mut time_base)
        })?;
        Ok(time_base)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: these are uniquely owned FFmpeg allocations; each matching free function accepts null and clears its pointer.
        unsafe {
            swr_free(&mut self.resampler);
            swr_free(&mut self.validation);
            av_frame_free(&mut self.frame);
            av_packet_free(&mut self.packet);
            avcodec_free_context(&mut self.context);
        }
    }
}

fn converted_trim_samples(
    input_samples: u32,
    input_rate: c_int,
    output_rate: c_int,
    output_channels: c_int,
) -> usize {
    if input_rate <= 0 || output_rate <= 0 || output_channels <= 0 {
        return 0;
    }
    u64::from(input_samples)
        .saturating_mul(output_rate as u64)
        .div_ceil(input_rate as u64)
        .saturating_mul(output_channels as u64)
        .min(usize::MAX as u64) as usize
}

#[cfg(test)]
fn trim_pending_samples(samples: &mut Vec<f32>, start: &mut usize, end: &mut usize) {
    let remove_start = (*start).min(samples.len());
    if remove_start > 0 {
        samples.drain(..remove_start);
        *start -= remove_start;
    }
    let remove_end = (*end).min(samples.len());
    samples.truncate(samples.len().saturating_sub(remove_end));
    *end -= remove_end;
}

fn check_ffmpeg(context: &str, result: c_int) -> io::Result<()> {
    if result < 0 {
        Err(ffmpeg_error(context, result))
    } else {
        Ok(())
    }
}

/// Worker-owned mono float to 48 kHz s16 microphone resampler.
pub struct CaptureResampler(*mut c_void);

impl std::fmt::Debug for CaptureResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureResampler").finish_non_exhaustive()
    }
}

impl CaptureResampler {
    /// Creates a worker-owned mono float to 48 kHz signed-16 resampler.
    ///
    /// # Errors
    /// Returns an error for rates outside 8–384 kHz, unsupported native ABI,
    /// or native allocation/setup failure.
    pub fn new(rate: u32) -> io::Result<Self> {
        ffmpeg::abi()?;
        if !(8_000..=384_000).contains(&rate) {
            return Err(invalid("invalid capture rate"));
        }
        let rate = c_int::try_from(rate).map_err(|_| invalid("capture rate too large"))?;
        let mut result = Self(ptr::null_mut());
        let mut mono = AVChannelLayout::default();
        // SAFETY: layout is initialized writable storage; FFmpeg initializes its layout data before use.
        unsafe { av_channel_layout_default(&mut mono, 1) };
        // SAFETY: layouts live through the call; result owns the returned context exclusively.
        let status = unsafe {
            swr_alloc_set_opts2(
                &mut result.0,
                &mono,
                1,
                48_000,
                &mono,
                AV_SAMPLE_FMT_FLT,
                rate,
                0,
                ptr::null_mut(),
            )
        };
        // SAFETY: layout was initialized above and is no longer borrowed by the native option setter.
        unsafe { av_channel_layout_uninit(&mut mono) };
        check_ffmpeg("could not allocate microphone resampler", status)?;
        // SAFETY: the allocation succeeded and result exclusively owns the context.
        check_ffmpeg("could not initialize microphone resampler", unsafe {
            swr_init(result.0)
        })?;
        Ok(result)
    }

    /// Resamples mono input into caller-owned output storage.
    ///
    /// At most 16,777,216 input or output samples are accepted per call.
    /// Output must fit the native upper bound, including filter delay.
    ///
    /// # Errors
    /// Returns an error for excessive/insufficient buffers or native failure.
    pub fn convert(&mut self, input: &[f32], output: &mut [i16]) -> io::Result<usize> {
        if input.len() > MAX_OUTPUT_SAMPLES || output.len() > MAX_OUTPUT_SAMPLES {
            return Err(invalid("capture buffer exceeds sample budget"));
        }
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err(invalid("capture contains nonfinite samples"));
        }
        // SAFETY: the exclusively borrowed native resampler is initialized.
        let required = unsafe {
            swr_get_out_samples(
                self.0,
                c_int::try_from(input.len()).map_err(|_| invalid("capture input too large"))?,
            )
        };
        check_ffmpeg("could not size capture output", required)?;
        if required as usize > output.len() {
            return Err(invalid("capture output is too small"));
        }
        let count = c_int::try_from(input.len()).map_err(|_| invalid("capture input too large"))?;
        let capacity =
            c_int::try_from(output.len()).map_err(|_| invalid("capture output too large"))?;
        let source = [input.as_ptr().cast::<u8>()];
        let mut destination = [output.as_mut_ptr().cast::<u8>()];
        // SAFETY: both mono interleaved buffers have the declared sample capacity; the context
        // is initialized, exclusively borrowed, and configured for float input / i16 output.
        let count = unsafe {
            swr_convert(
                self.0,
                destination.as_mut_ptr(),
                capacity,
                source.as_ptr(),
                count,
            )
        };
        check_ffmpeg("could not resample microphone", count)?;
        Ok(count as usize)
    }
}

impl Drop for CaptureResampler {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the allocation, and swr_free accepts null on failed setup.
        unsafe { swr_free(&mut self.0) };
    }
}

fn ffmpeg_error(context: &str, code: c_int) -> io::Error {
    let mut buffer: [c_char; 256] = [0; 256];
    // SAFETY: buffer is writable for its declared length; successful av_strerror writes a NUL-terminated diagnostic.
    let description = if unsafe { av_strerror(code, buffer.as_mut_ptr(), buffer.len()) } == 0 {
        // SAFETY: successful av_strerror wrote a terminated string into the still-live buffer.
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    } else {
        format!("FFmpeg error {code}")
    };
    io::Error::other(format!("{context}: {description}"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[link(name = "avcodec")]
unsafe extern "C" {}
#[link(name = "avutil")]
unsafe extern "C" {}
#[link(name = "swresample")]
unsafe extern "C" {}

unsafe extern "C" {
    fn avcodec_find_decoder_by_name(name: *const c_char) -> *const c_void;
    fn avcodec_alloc_context3(codec: *const c_void) -> *mut c_void;
    fn avcodec_free_context(context: *mut *mut c_void);
    fn avcodec_parameters_to_context(context: *mut c_void, parameters: *const c_void) -> c_int;
    fn avcodec_open2(
        context: *mut c_void,
        codec: *const c_void,
        options: *mut *mut c_void,
    ) -> c_int;
    fn avcodec_send_packet(context: *mut c_void, packet: *const c_void) -> c_int;
    fn avcodec_receive_frame(context: *mut c_void, frame: *mut c_void) -> c_int;
    fn av_packet_alloc() -> *mut c_void;
    fn av_packet_free(packet: *mut *mut c_void);
    fn av_packet_unref(packet: *mut c_void);
    fn av_new_packet(packet: *mut c_void, size: c_int) -> c_int;
    fn av_frame_alloc() -> *mut c_void;
    fn av_frame_free(frame: *mut *mut c_void);
    fn av_frame_unref(frame: *mut c_void);
    fn av_mallocz(size: usize) -> *mut c_void;
    fn av_opt_set_q(
        object: *mut c_void,
        name: *const c_char,
        value: AVRational,
        flags: c_int,
    ) -> c_int;
    #[cfg(test)]
    fn av_opt_get_q(
        object: *mut c_void,
        name: *const c_char,
        flags: c_int,
        output: *mut AVRational,
    ) -> c_int;
    fn av_opt_set_int(object: *mut c_void, name: *const c_char, value: i64, flags: c_int) -> c_int;
    fn av_opt_set_chlayout(
        object: *mut c_void,
        name: *const c_char,
        layout: *const AVChannelLayout,
        flags: c_int,
    ) -> c_int;
    fn av_channel_layout_default(layout: *mut AVChannelLayout, channels: c_int);
    fn av_channel_layout_from_mask(layout: *mut AVChannelLayout, mask: u64) -> c_int;
    fn av_channel_layout_uninit(layout: *mut AVChannelLayout);
    fn swr_alloc_set_opts2(
        context: *mut *mut c_void,
        output_layout: *const AVChannelLayout,
        output_format: c_int,
        output_rate: c_int,
        input_layout: *const AVChannelLayout,
        input_format: c_int,
        input_rate: c_int,
        log_offset: c_int,
        log_context: *mut c_void,
    ) -> c_int;
    fn av_opt_get_int(
        object: *mut c_void,
        name: *const c_char,
        flags: c_int,
        output: *mut i64,
    ) -> c_int;
    fn av_opt_get_chlayout(
        object: *mut c_void,
        name: *const c_char,
        flags: c_int,
        output: *mut AVChannelLayout,
    ) -> c_int;
    fn swr_get_out_samples(context: *mut c_void, input_samples: c_int) -> c_int;
    fn swr_get_delay(context: *mut c_void, base: i64) -> i64;
    fn swr_alloc() -> *mut c_void;
    fn swr_config_frame(context: *mut c_void, output: *const c_void, input: *const c_void)
    -> c_int;
    fn swr_init(context: *mut c_void) -> c_int;
    fn swr_convert(
        context: *mut c_void,
        output: *mut *mut u8,
        output_count: c_int,
        input: *const *const u8,
        input_count: c_int,
    ) -> c_int;
    fn swr_free(context: *mut *mut c_void);
    fn av_strerror(error: c_int, buffer: *mut c_char, buffer_size: usize) -> c_int;
}

#[cfg(test)]
fn trim_samples(
    samples: &mut Vec<f32>,
    trim_start_samples: u32,
    trim_end_samples: u32,
    input_rate: c_int,
    output_rate: c_int,
    output_channels: c_int,
) {
    if input_rate <= 0 || output_rate <= 0 || output_channels <= 0 || samples.is_empty() {
        return;
    }
    let mut start =
        converted_trim_samples(trim_start_samples, input_rate, output_rate, output_channels);
    let mut end =
        converted_trim_samples(trim_end_samples, input_rate, output_rate, output_channels);
    trim_pending_samples(samples, &mut start, &mut end);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mismatched_native_frame(mono: bool) {
        let config = AudioConfiguration {
            codec: "pcm_s16le".into(),
            packetization: "pcm-packet-v1".into(),
            extradata: vec![],
            sample_rate: 48_000,
            channels: 2,
            channel_mask: 3,
            maximum_access_unit_bytes: 4096,
            codec_string: None,
        };
        let mut decoder = Decoder::new(&config, 48_000, 2).unwrap();
        let set_context = |rate, channels| {
            let mut layout = AVChannelLayout::default();
            // SAFETY: initialized layout and a live, exclusively owned codec;
            // each native setter copies its arguments before returning.
            unsafe {
                check_ffmpeg(
                    "test context rate",
                    av_opt_set_int(decoder.context, c"ar".as_ptr(), rate, 0),
                )
                .unwrap();
                av_channel_layout_default(&mut layout, channels);
                let result =
                    av_opt_set_chlayout(decoder.context, c"ch_layout".as_ptr(), &layout, 0);
                av_channel_layout_uninit(&mut layout);
                check_ffmpeg("test context layout", result).unwrap();
            }
        };
        set_context(if mono { 48_000 } else { 44_100 }, if mono { 1 } else { 2 });
        // SAFETY: owned packet/context/frame are live. Allocate four initialized
        // PCM bytes, decode them, and retain the native frame through validation.
        unsafe {
            check_ffmpeg("allocate test packet", av_new_packet(decoder.packet, 4)).unwrap();
            let packet = &mut *(decoder.packet as *mut AVPacket);
            ptr::write_bytes(packet.data, 0, 4);
            check_ffmpeg(
                "send test packet",
                avcodec_send_packet(decoder.context, decoder.packet),
            )
            .unwrap();
            av_packet_unref(decoder.packet);
            check_ffmpeg(
                "receive test frame",
                avcodec_receive_frame(decoder.context, decoder.frame),
            )
            .unwrap();
        }
        // The codec context now agrees with the contract; the retained frame
        // still carries the different rate or layout with which it was decoded.
        set_context(48_000, 2);
        // SAFETY: a successful receive initialized this still-retained frame.
        let format = unsafe { decoder.abi.frame(decoder.frame) }.format;
        assert!(
            decoder.validate_format(format).is_err(),
            "context metadata hid a different native frame"
        );
    }

    #[test]
    fn decoder_rejects_frame_rate_changes_hidden_by_the_codec_context() {
        mismatched_native_frame(false);
    }

    #[test]
    fn decoder_rejects_frame_layout_changes_hidden_by_the_codec_context() {
        mismatched_native_frame(true);
    }
    #[test]
    fn trim_is_rescaled_and_channel_aligned() {
        let mut samples: Vec<f32> = (0..20).map(|value| value as f32).collect();
        trim_samples(&mut samples, 2, 1, 24_000, 48_000, 2);
        assert_eq!(
            samples,
            (8..16).map(|value| value as f32).collect::<Vec<_>>()
        );
    }
    #[test]
    fn delayed_decoder_output_preserves_pending_trim() {
        let mut start = 4;
        let mut end = 2;
        let mut empty = Vec::new();
        trim_pending_samples(&mut empty, &mut start, &mut end);
        assert_eq!((start, end), (4, 2));

        let mut samples: Vec<f32> = (0..12).map(|value| value as f32).collect();
        trim_pending_samples(&mut samples, &mut start, &mut end);
        assert_eq!(
            samples,
            (4..10).map(|value| value as f32).collect::<Vec<_>>()
        );
        assert_eq!((start, end), (0, 0));
    }
    #[test]
    fn ffmpeg_decodes_pcm_access_units_without_an_output_device() {
        let config = AudioConfiguration {
            codec: "pcm_s16le".into(),
            packetization: "pcm-packet-v1".into(),
            extradata: Vec::new(),
            sample_rate: 48_000,
            channels: 2,
            channel_mask: 3,
            maximum_access_unit_bytes: 4_096,
            codec_string: None,
        };
        let encoded = vec![0_u8; 480 * 2 * 2];
        assert!(supports(&config));
        let mut decoder = Decoder::new(&config, 48_000, 2).unwrap();
        assert_eq!(decoder.packet_time_base().unwrap(), PACKET_TIME_BASE);
        let mut samples = decoder
            .push(ParsedAudioPacket {
                epoch: 1,
                packet_id: 1,
                pts_us: 0,
                dts_us: 0,
                duration_us: 10_000,
                trim_start_samples: 0,
                trim_end_samples: 0,
                data: &encoded,
            })
            .unwrap();
        samples.extend(decoder.finish().unwrap());
        assert_eq!(samples.len(), 480 * 2);
        assert!(samples.iter().all(|sample| *sample == 0.0));
    }
    #[test]
    fn ffmpeg_decodes_vvdoom_float_pcm_access_units() {
        let config = AudioConfiguration {
            codec: "pcm_f32le".into(),
            packetization: "pcm-packet-v1".into(),
            extradata: Vec::new(),
            sample_rate: 48_000,
            channels: 2,
            channel_mask: 3,
            maximum_access_unit_bytes: (960 * 2 * size_of::<f32>()) as u32,
            codec_string: Some("pcm-f32".into()),
        };
        let source = (0..960 * 2)
            .map(|sample| if sample % 2 == 0 { 0.5_f32 } else { -0.25_f32 })
            .collect::<Vec<_>>();
        let encoded = source
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>();
        assert!(supports(&config));
        let mut decoder = Decoder::new(&config, 48_000, 2).unwrap();
        let mut samples = decoder
            .push(ParsedAudioPacket {
                epoch: 1,
                packet_id: 1,
                pts_us: 0,
                dts_us: 0,
                duration_us: 20_000,
                trim_start_samples: 0,
                trim_end_samples: 0,
                data: &encoded,
            })
            .unwrap();
        samples.extend(decoder.finish().unwrap());
        assert_eq!(samples, source);
    }
}
