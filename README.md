# vivid_sdk

See [optional desktop GUI runtime](docs/GUI.md) for the migrated GPUI framework, embedded views, and independent application processes.

[Native local control](docs/LOCAL-CONTROL.md) provides optional same-owner sockets
and pipes shared by Vivido and compositor management, independently of the GUI.

[Native video decoding](docs/VIDEO.md) exposes Vivido's shared FFmpeg decoder
through optional `video-decode`, independently of GUI and presenter features.

[Native audio decoding](docs/AUDIO.md) similarly exposes the shared bounded audio
decoder and capture resampler through optional `audio-decode`, without an audio device.

Independent `audio-output` adds bounded native device output and source clocks
without FFmpeg, GUI or terminal dependencies; see [native audio services](docs/AUDIO.md).

Experimental `presenter,vector-audio` adds explicitly enabled audio source workers
to `NativeVectorHost`. Device/decoder ownership and retirement stay off host
threads. Standalone timed audio adds bounded pre-roll, PLAY, PAUSE/resume and
device-consumed DRAIN. With `vector-video`, linked timed video follows the audio
master. Matching Live tracks also use the active audio clock after callback
progress, with latest-picture fallback when no usable clock exists. Synchronized
starts remain unfinished. See the
[native audio contract](docs/AUDIO.md#native-live-hosting).

Native mixed raster/vector hosting can enable `vector-images` for bounded,
asynchronous static PNG/JPEG decoding. See [native vector hosting](docs/NATIVE-VECTOR.md)
for image limits, color conversion, display receipts and feature negotiation.

See [pane overlays](docs/OVERLAYS.md) for Rust, Python, TypeScript, and Lua window APIs, Vello drawing, and current acceptance status.

See [shared vector rendering](docs/VECTOR-RENDERER.md) for optional display-list compilation,
host-shaped text painting, hit geometry, and cached GPU composition used by Vivido.

See [native vector presentation](docs/NATIVE-VECTOR.md) for authenticated window
composition, exact display receipts and optional asynchronous GPU readback.

See the [progressive examples](examples/README.md) for eight runnable tutorials in each language, from displaying an image to a producer/presenter round trip.

`vivid_sdk` is the full-duplex Rust SDK for Vivid Protocol 1.5. It serves both roles: the crate
root is the producer, and [`presenter`](src/presenter/) — behind the off-by-default `presenter`
feature — is the terminating presenter that accepts one.

A presenter is namespaced rather than flattened into the crate root, because the two roles name some
things alike: a presenter's `SceneNode` is its own projection of a node, not the producer's.

## Bindings

Three language bindings are built on this crate, each carrying both roles in one artifact:

| Package | README | Target |
|---|---|---|
| `vivid-sdk` (PyPI) | [README-python.md](README-python.md) | Python 3.9+, via a PyO3 extension |
| `@vivido/vivid-sdk` (npm) | [README-typescript.md](README-typescript.md) | Node 20+, via a napi-rs addon |
| `vivid-sdk` (LuaRocks) | [README-lua.md](README-lua.md) | LuaJIT and Lua 5.1-5.5, via an mlua module loaded with `require` |

They marshal; they do not decide. Constants, resource claims, and image container parsing come from
this crate, so a binding cannot disagree with the SDK about a wire value — and
[conformance/](conformance/README.md) checks covered behaviors against both each other and
independent fixture expectations:

```sh
npm run test:conformance
```

Add values to `constants.rs` and explicit names to every host package — for Lua, the
`lua/types/vivid_sdk.lua` definitions. Export-coverage tests check that public declarations match
the table; values are never copied into host modules.
Full API parity and per-platform binding distribution remain in progress.

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

`send_raster_adaptive` and `send_raster_delta_adaptive` use raw encoding when
compression is unavailable or would make the body larger. Malformed frames and
deltas still fail before consuming a media ID; explicit compression requests
continue to report an unavailable codec.

Native discovery uses `VIVID_ENDPOINT_CONTROL`, `VIVID_ENDPOINT_INTERACTIVE`,
`VIVID_ENDPOINT_REALTIME`, `VIVID_ENDPOINT_BULK`, and `VIVID_ROOT_SECRET`. Missing lane endpoints
select the protocol-defined fallback endpoint value while remaining separate connections. The
variables are read as one target's set: a config that names `endpoint_control` takes its other
lanes only from the config, never from an ambient environment that may describe another presenter.

Secret-bearing configuration deliberately implements neither `Debug` nor `Display`. Dropping a
session is an unclean transport loss so a resumable lease may suspend. Call `Session::close()` to
send clean `GOODBYE` and perform final logical-session cleanup.

Desktop input is deliberately not folded into the control event stream. Offer
`desktop-input-v1` and its prerequisite profiles, call `Session::open_input_lane()`, and establish
an `InputBinding` with a fresh producer epoch. Decode ordinary `InputLaneEvent` values with the
current surface dimensions, then apply `InputGate` immediately before the OS injection call.
`Revoked`, `Reset`, `LaneClosed`, or watchdog expiry must atomically disable injection and release
held keys and buttons. Queue overflow closes the affected lane rather than discarding transitions.

The Rust terminating presenter offers opt-in physical input through
`PresenterConfig::desktop_with_input`. Hosts explicitly select a presented,
owner-qualified `DesktopInputTarget`; selection alone never grants input. See
[desktop presenter input](docs/DESKTOP-INPUT.md) for admission, revocation,
generation, reconnect, and producer injection requirements.

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
