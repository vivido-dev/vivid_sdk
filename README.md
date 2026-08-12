# vivid_sdk

`vivid_sdk` is the full-duplex Rust producer SDK for Vivid Protocol 1.5.

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
