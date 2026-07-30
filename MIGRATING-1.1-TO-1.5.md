# Migrating vivid_sdk from Vivid 1.1 to 1.5

This is a breaking migration. Do not mix 1.1 handles or retry state with a 1.5 session.

| Vivid 1.1 SDK | Vivid 1.5 SDK | Required implementation change |
|---|---|---|
| `ProducerSession` / `Session` | `Session` | Negotiate profile names and validate the authenticated `WELCOME` transcript |
| feature ID arrays | required/optional profile names | Offer a sorted, prerequisite-closed profile set |
| `VIVID_ENDPOINT` | `VIVID_ENDPOINT_CONTROL` | Update discovery and launcher environments |
| `VIVID_TOKEN` | `VIVID_ROOT_SECRET` | Supply 32 secret bytes as 64 hex characters; never transmit the secret itself |
| `Source` | `Surface` plus one or more `Track` values | Move descriptor, policy, scene, and input identity to the surface |
| source codec mutation | replacement immutable track | Prime and activate the new track without recreating the surface |
| media ticket and `ATTACH_CHANNEL` | authenticated `TrackChannel` generation | Wait for positive `CHANNEL_ACCEPTED` before any media |
| media-kind connection | `TRACK` connection plus track kind/lane in `CHANNEL_OPEN` | Select realtime or bulk by QoS, not media type |
| incremental `CREDIT` | cumulative `MAX_CHANNEL_DATA` | Track sent totals and absolute maxima per channel generation |
| control-stream `EOS` plus sequence barrier | ordered `CHANNEL_EOS` | Send EOS on the track channel after the final media record |
| source revision | surface revision/generation and track revision/channel generation | Never copy one counter domain into another |
| source scene node | node referencing a complete surface identity | Keep nodes stable during codec or channel replacement |
| linked A/V sources | video/audio slots on one surface | Atomically activate the slot set |
| control-stream `DesktopInputEvent` | authenticated `InputLane` plus `InputGate` | Bind with a fresh producer epoch and re-check the complete grant tuple at the final injection boundary |
| delegated context bearer capability | controller-created session lease verifier/secret | Keep activation material locally and use the retry-safe lease state machine |
| anchor marker v2 | session-derived marker v3 | Include the owning context ID in the authenticated marker |

## Rust changes

1. Upgrade `vivid_sdk` and `vivid_protocol` together to `1.5`.
2. Raise the Rust toolchain to 1.87 or newer.
3. Replace source constructors with `create_surface()` followed by `create_track()`.
4. Put finite rate, bitrate, body, in-flight, decoder, and retained-pixel claims in every
   `TrackConfiguration`.
5. Replace `open_media_sender()` with `open_track_channel()`. Treat an open failure after bytes
   may have been sent as generation state, not as permission to replay on another endpoint.
6. Send the first video key unit, full raster, complete image, or valid independent audio unit
   required by the new/recovered generation.
7. Use `activate_tracks()` after the replacement reaches its required current-generation
   milestone.
8. Use `advance_channel()` for reattachment. Never reuse old flow maxima or readiness bits.
9. Move desktop input to `Session::open_input_lane()`. Treat `INPUT_REVOKED`, `INPUT_RESET`,
   `InputLaneEvent::LaneClosed`, watchdog expiry, and queue failure as one fail-closed release
   path. Pass every decoded event through `InputGate` immediately before OS injection.
10. After resume, reconcile retained handles with `query_surface()` and `query_track()`, then call
    `adopt_surface()` before `adopt_track()`. Advance every selected channel generation before
    reopening it; media IDs remain monotonic across those generations.
11. Call `Session::close()` for clean final cleanup. Dropping is deliberately unclean so a lease
   may suspend.

Every `Surface` and `Track` handle carries its full context ancestry. Maps and cleanup predicates
must use `(context_id, surface_id)` and `(context_id, surface_id, track_id)`, not the local numeric
ID alone.

## Python changes

Old:

```python
source = vivid.create_raster_source(session, 640, 480)
sender = vivid.open_sender(session, source)
vivid.send_raster(sender, rgba, width=640, height=480)
```

New:

```python
surface = vivid.create_surface(
    session, vivid.SurfaceConfig(logical_width=640, logical_height=480)
)
track = vivid.create_track(
    session, surface, vivid.RasterTrackConfig(width=640, height=480)
)
channel = vivid.open_track_channel(session, track)
vivid.send_raster(channel, rgba)
```

`Source`, `MediaSender`, feature constants, source waits, attachment resolution, rolling credits,
and delegated capability bytes are removed. Migrate behavior, not names.
