# vivid_sdk

`vivid_sdk` is the full-duplex Rust SDK for Vivid Protocol 1.5. It serves both roles: the crate
root is the producer, and [`presenter`](src/presenter/) — behind the off-by-default `presenter`
feature — is the terminating presenter that accepts one.

A presenter is namespaced rather than flattened into the crate root, because the two roles name some
things alike: a presenter's `SceneNode` is its own projection of a node, not the producer's.

`SocketListener` binds the endpoint spellings a producer already understands — `unix:/absolute/path`
or `tcp:127.0.0.1:PORT`, loopback only, with port 0 binding an ephemeral port. A product that owns
its own transport implements `PresenterListener` instead.

Version 1.5 is a direct, breaking cutover. The SDK no longer exposes Vivid 1.1 sources, media
tickets, feature IDs, rolling credits, attachment generations, or source-scoped scenes. Its public
objects match the 1.5 protocol:

- `Session` performs profile negotiation and transcript-bound root, lease-activation, or resume
  authentication.
- `Surface` is stable semantic, scene, policy, and input identity.
- `Track` is one immutable video, audio, raster, or encoded-image configuration owned by a
  surface.
- `TrackChannel` is one authenticated channel generation with positive `CHANNEL_ACCEPTED`,
  cumulative byte/record maxima, recovery-unit enforcement, and ordered `CHANNEL_EOS`.
- `InputLane` is a separately authenticated interactive-lane generation. It exposes
  generation-qualified input, watchdog renewals, revocations, resets, and fail-closed lane loss.

Changing a codec, encoded resolution, bitrate contract, or transport does not recreate a surface.
Create and prime a replacement track, atomically activate its slot, then destroy the old track.
Only a real coordinate/injection-target change advances surface generation.

Use `query_surface()`, `query_track()`, and bounded `wait_track()` calls for authoritative
readiness and recovery. A leased session can prepare its next secret-redacted
`ProducerAuthentication` with `resume_authentication()`. After authenticated resume, retain the
old handles, reconcile them, then call `adopt_surface()` followed by `adopt_track()` before
advancing and reopening channels.

## Establishment retries and connection lifetime

Use `EstablishmentAttempt::new(config, retry_timeout)` when a leased activation or resume must
survive a lost WELCOME. Retry `attempt.connect()` (or `connect_with_factory` on the same carrier)
on that same object after a transport failure. It retains the exact authenticated HELLO and
allows a timeout up to 300 seconds; the presenter may enforce a shorter activation/grace window.
A successful attempt cannot be reused and releases its handshake secrets. Expired attempts reject
further connects and release their prepared buffers; dropping an attempt also releases them.
Root authentication requires a fresh attempt after failure because accepted root nonces cannot
be replayed. Ordinary `Session::connect` still performs one attempt.

Dropping a session or input lane shuts down its transport, including native readers retained by
background workers. `Session::close` performs graceful GOODBYE; dropping is unclean loss and allows
a leased presenter to suspend. `abort` continues to close the local lifecycle so callers can stop
media workers before a graceful close.

The presenter's `Writer` admits records to a bounded per-connection queue; success means queued,
not delivered. Its worker performs the socket writes independently of presenter state locks.
There are at most 64 queued items plus the current write. Outstanding bytes, including the current
write, are bounded by four negotiated maximum records (at least 1 MiB, at most the 64 MiB hard
record limit plus its header). Saturation or a write failure closes that connection. Use `flush`
outside shared state locks when delivery must complete; its wait is bounded to three seconds.
Custom accepted transports must implement `ConnectionCancel` so cancellation interrupts blocked
I/O. Root replay tracking retains at most 4096 accepted principal/nonces for five minutes and
fails closed at capacity without evicting live entries.

## Offline tracing

Offline sessions produce metadata-only `control.ndjson`, `track-*.ndjson`, and lane/transfer
NDJSON files. These replace the former binary `.vivid` traces and cannot be replayed as wire
traffic. The offline session uses synthetic authentication. Trace files contain no record bodies;
new paths are required and Unix permissions are `0600`. Close all sessions and channel handles
before reading the final files so their bounded asynchronous trace writers can drain.

## Rust example

```rust,no_run
use vivid_protocol::{
    messages::LaneClass,
    track::{KindConfiguration, RasterConfiguration, TrackConfiguration, TrackMode},
};
use vivid_sdk::{
    CoordinateModel, GENERIC_CONTENT, ProducerConfig, RequestMetadata, Session,
    SurfaceDefinition, SurfaceDescriptor, SurfaceRole,
};

let mut session = Session::connect(ProducerConfig::default())?;
let context_id = session.info().root_context_id;
let surface_id = session.allocate_id()?;
let surface = session.create_surface(
    SurfaceDefinition {
        context_id,
        surface_id,
        semantic_profile: GENERIC_CONTENT.into(),
        coordinate_model: CoordinateModel::DesktopLogicalPixels,
        logical_width: 640,
        logical_height: 480,
        scale_numerator: 1,
        scale_denominator: 1,
        rotation: 0,
        descriptor: SurfaceDescriptor {
            role: SurfaceRole::Figure,
            title: "frame".into(),
            semantic_content_revision: 1,
            semantic_availability: 0,
            locator_hint: String::new(),
        },
        policy: 0,
        profile_parameters: vec![],
    },
    &RequestMetadata::default(),
)?;

let track_id = session.allocate_id()?;
let track = session.create_track(
    TrackConfiguration {
        context_id,
        surface_id,
        track_id,
        slot: 3,
        mode: TrackMode::Live,
        lane: LaneClass::Bulk,
        maximum_record_body: 72 + 640 * 480 * 4,
        maximum_rate_millihertz: 60_000,
        maximum_encoded_bits_per_second: 600_000_000,
        maximum_records_per_second: 60,
        maximum_inflight_body_bytes: 8_000_000,
        kind: KindConfiguration::Raster(RasterConfiguration {
            width: 640,
            height: 480,
            alpha_mode: 1,
            delta_enabled: false,
            maximum_delta_operations: 1,
            zstd_enabled: false,
        }),
        target_latency_us: 16_000,
        maximum_latency_us: 100_000,
        retained_pixel_charge: 640 * 480,
    },
    &RequestMetadata::default(),
)?;

let channel = session.open_track_channel(&track)?;
channel.send_raster(0, 1, &vec![0; 640 * 480 * 4], false)?;
channel.eos()?;
# Ok::<(), std::io::Error>(())
```

Native discovery uses `VIVID_ENDPOINT_CONTROL`, `VIVID_ENDPOINT_INTERACTIVE`,
`VIVID_ENDPOINT_REALTIME`, `VIVID_ENDPOINT_BULK`, and `VIVID_ROOT_SECRET`. Missing lane endpoints
select the protocol-defined fallback endpoint value while remaining separate connections.

Secret-bearing configuration deliberately implements neither `Debug` nor `Display`. Dropping a
session is an unclean transport loss so a resumable lease may suspend. Call `Session::close()` to
send clean `GOODBYE` and perform final logical-session cleanup.

Desktop input is deliberately not folded into the control event stream. Offer
`desktop-input-v1` and its prerequisite profiles, call `Session::open_input_lane()`, and establish
an `InputBinding` with a fresh producer epoch. Decode ordinary `InputLaneEvent` values with the
current surface dimensions, then apply `InputGate` immediately before the OS injection call.
`Revoked`, `Reset`, `LaneClosed`, or watchdog expiry must atomically disable injection and release
held keys and buttons. Queue overflow closes the affected lane rather than discarding transitions.

See [MIGRATING-1.1-TO-1.5.md](MIGRATING-1.1-TO-1.5.md) for the old-to-new API mapping.

## Physical playback observations

`Session::track_query_handle()` provides a read-only handle for a bounded background observer.
It shares request correlation and session cancellation but never reconciles mutable local track
state from a late reply. Keep one query in flight and qualify observations against the expected
owner, track and channel generation.

The presenter role accepts owner-scoped `BridgePositionSnapshot` feedback from a terminating
outer bridge. Decoder-reset and playback-request mismatches are ignored. Timed-video status and
PTS presentation waits use physical presentation IDs/timestamps, never the last admitted packet.
The existing optional TRACK_STATUS playback map carries the physical clock when available;
absence of feedback remains unknown rather than a synthetic advancing clock.

Virtual-presenter channel/epoch replacement clears EOS. `apply_outer_playback` requires the
source decoder reset serial from the outer snapshot and ignores retired-generation completion.
