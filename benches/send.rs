//! Producer send-path benchmark against an offline session.
//!
//! Run with `cargo bench --bench send`. Each scenario reports the mean wall time, heap allocations,
//! and allocated bytes of one send. An offline session runs every validation, sequence check, and
//! flow charge a live one does, but has no presenter and no rate limit, so the numbers isolate the
//! SDK's own per-record cost from transport and presenter behavior.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use vivid_protocol::media::{AudioPacket, VideoPacket};
use vivid_protocol::messages::LaneClass;
use vivid_protocol::resource::{Resource, ResourceContract};
use vivid_protocol::track::{KindConfiguration, TrackConfiguration};
use vivid_sdk::{
    ProducerConfig, RequestMetadata, SLOT_AUDIO, SLOT_PRIMARY_VIDEO, SLOT_RASTER, Session, Surface,
    SurfaceBuilder, SurfaceRole, TrackBuilder, TrackChannel, TrackMode,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

static TRACKING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Counts heap allocations made while a scenario is measured.
struct CountingAllocator;

// SAFETY: every operation delegates to the System allocator with the caller's pointer and layout.
// The atomics only observe successful requests.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegated with the caller-provided layout.
        let pointer = unsafe { System.alloc(layout) };
        record(pointer, layout.size());
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegated with the caller-provided layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record(pointer, layout.size());
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: delegated with the original pointer and layout.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: delegated with the original pointer, layout, and requested size.
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        record(new_pointer, new_size);
        new_pointer
    }
}

fn record(pointer: *mut u8, bytes: usize) {
    if !pointer.is_null() && TRACKING.load(Ordering::Relaxed) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

/// Raster geometry for the frame scenarios: a 720p framebuffer, 3.5 MiB of RGBA per frame.
const RASTER_WIDTH: u32 = 1280;
const RASTER_HEIGHT: u32 = 720;
/// A large inter-frame video access unit.
const VIDEO_PACKET_BYTES: usize = 64 * 1024;
/// A small PCM packet; the builder's audio claim allows up to 1 KiB.
const AUDIO_PACKET_BYTES: usize = 512;

fn main() -> io::Result<()> {
    let mut session = Session::connect(ProducerConfig::offline())?;
    let metadata = RequestMetadata::default();
    let definition = SurfaceBuilder::new(&session, 2, 2)?
        .titled(SurfaceRole::Figure, "send benchmark")
        .build()?;
    let surface = session.create_surface(definition, &metadata)?;
    // The offline contract caps each claim at 1,000,000, below one 720p frame in flight or a video
    // track's bit rate. An offline session has no presenter to enforce claims against, so build
    // these tracks against wide ceilings that keep only the session's own record limit.
    let mut contract = ResourceContract::new([1 << 40; 33]);
    contract.set(
        Resource::MediaRecordBody,
        session
            .info()
            .resource_contract
            .get(Resource::MediaRecordBody),
    );

    println!(
        "{:<28} {:>12} {:>14} {:>16}",
        "scenario", "time/send", "allocs/send", "alloc bytes/send"
    );

    let pixel_count = usize::try_from(RASTER_WIDTH * RASTER_HEIGHT).expect("fits in usize");
    let solid = [32_u8, 64, 96, 255].repeat(pixel_count);
    let noise = noise(pixel_count * 4);

    let raw = raster_channel(&mut session, &contract, &surface, false)?;
    let mut frame_id = 0;
    run("raster 720p raw", 100, || {
        frame_id += 1;
        raw.send_raster(0, frame_id, &solid, false)
    })?;

    let adaptive = raster_channel(&mut session, &contract, &surface, true)?;
    let mut frame_id = 0;
    run("raster 720p adaptive, flat", 50, || {
        frame_id += 1;
        adaptive.send_raster_adaptive(0, frame_id, &solid)
    })?;
    run("raster 720p adaptive, noise", 20, || {
        frame_id += 1;
        adaptive.send_raster_adaptive(0, frame_id, &noise)
    })?;

    let configuration = TrackBuilder::new(
        &surface,
        SLOT_PRIMARY_VIDEO,
        TrackMode::Live,
        LaneClass::Bulk,
    )
    .video(1920, 1080, "h264")
    .build(&contract, session.allocate_id()?)?;
    let video = open(&mut session, configuration)?;
    let payload = &noise[..VIDEO_PACKET_BYTES];
    let mut packet_id = 0;
    run("video 64 KiB packet", 2_000, || {
        packet_id += 1;
        video.send_video(VideoPacket {
            epoch: 0,
            packet_id,
            pts_us: 0,
            dts_us: 0,
            duration_us: 16_667,
            key: true,
            data: payload,
        })
    })?;

    let mut configuration =
        TrackBuilder::new(&surface, SLOT_AUDIO, TrackMode::Live, LaneClass::Bulk)
            .audio(48_000, 2)
            .build(&contract, session.allocate_id()?)?;
    // The builder's Opus default needs an OpusHead; PCM needs no initialization data.
    if let KindConfiguration::Audio(audio) = &mut configuration.kind {
        audio.codec = "pcm_s16le".into();
        audio.packetization = "pcm-packet-v1".into();
        audio.channel_mask = 3;
    }
    let audio = open(&mut session, configuration)?;
    let payload = &noise[..AUDIO_PACKET_BYTES];
    let mut packet_id = 0;
    run("audio 512 B packet", 20_000, || {
        packet_id += 1;
        audio.send_audio(AudioPacket {
            epoch: 0,
            packet_id,
            pts_us: 0,
            dts_us: 0,
            duration_us: 20_000,
            trim_start_samples: 0,
            trim_end_samples: 0,
            data: payload,
        })
    })?;

    session.close()
}

fn raster_channel(
    session: &mut Session,
    contract: &ResourceContract,
    surface: &Surface,
    zstd_enabled: bool,
) -> io::Result<TrackChannel> {
    let mut builder = TrackBuilder::new(surface, SLOT_RASTER, TrackMode::Live, LaneClass::Bulk)
        .raster(RASTER_WIDTH, RASTER_HEIGHT)?;
    if zstd_enabled {
        builder = builder.raster_deltas(1, true);
    }
    let configuration = builder.build(contract, session.allocate_id()?)?;
    open(session, configuration)
}

fn open(session: &mut Session, configuration: TrackConfiguration) -> io::Result<TrackChannel> {
    let track = session.create_track(configuration, &RequestMetadata::default())?;
    session.open_track_channel(&track)
}

/// Time `iterations` sends after one untimed warm-up send, and print the per-send means.
fn run(name: &str, iterations: u32, mut send: impl FnMut() -> io::Result<u64>) -> io::Result<()> {
    black_box(send()?);
    ALLOCATIONS.store(0, Ordering::SeqCst);
    ALLOCATED_BYTES.store(0, Ordering::SeqCst);
    TRACKING.store(true, Ordering::SeqCst);
    let started = Instant::now();
    for _ in 0..iterations {
        black_box(send()?);
    }
    let elapsed = started.elapsed();
    TRACKING.store(false, Ordering::SeqCst);
    let per_send = |total: u64| total as f64 / f64::from(iterations);
    println!(
        "{name:<28} {:>12} {:>14.1} {:>16.0}",
        format!("{:.1?}", elapsed / iterations),
        per_send(ALLOCATIONS.load(Ordering::SeqCst)),
        per_send(ALLOCATED_BYTES.load(Ordering::SeqCst)),
    );
    Ok(())
}

/// Deterministic incompressible bytes, so zstd falls back to the raw frame.
fn noise(length: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..length)
        .map(|_| {
            // xorshift64: fast, reproducible, and statistically flat enough to defeat zstd.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}
