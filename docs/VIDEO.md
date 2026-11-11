# Native video decoding

Enable SDK `video-decode` to use `vivid_sdk::video`. This optional support crate
is independent of the GUI runtime, presenter and GPU renderer. The producer's
default feature set retains its existing dependency graph and native requirements.

Vivido uses this decoder in its existing per-channel media workers. Optional SDK
`vector-video` also integrates live video, including bounded reordered output, into `NativeVectorHost`'s
raster/vector composition. The compositor enables that integration through its
default-on `video` feature. Video-only timed playback adds bounded pre-roll,
pause/resume and due-frame selection. Linked audio and synchronized playback
remain work; see the [native presenter contract](NATIVE-VECTOR.md#timed-video).

```rust
use vivid_protocol::{media::ParsedVideoPacket, track::{TrackMode, VideoConfiguration}};
use vivid_sdk::video::{Decoder, DecodedFrame, Limits};

fn create_on_media_worker(config: &VideoConfiguration) -> std::io::Result<Decoder> {
    Decoder::with_limits(config, TrackMode::Live, Limits {
        max_dimension: 8192,
        max_frame_bytes: 32 * 1024 * 1024,
        max_output_bytes: 64 * 1024 * 1024,
        max_output_frames: 2,
        threads: 2,
        recycle_buffers: false,
    })
}

// Retain the same decoder between packets to preserve codec references.
fn decode_next(
    decoder: &mut Decoder,
    packet: ParsedVideoPacket<'_>,
) -> std::io::Result<Vec<DecodedFrame>> {
    let (frames, _discarded) = decoder.push_discarding_before(packet, None)?;
    Ok(frames)
}
```

`recycle_buffers: false` lets a host account every output allocation through its
own retained leases. The default retains Vivido's two-buffer recycling behavior.
The native decoder also applies FFmpeg's `max_pixels` option before opening the
codec, so an oversized initialization header can fail before packet decoding.
`max_frame_bytes` limits one image independently of `max_output_bytes`, which
limits the complete returned batch. The native pixel bound uses the smaller of
those ceilings; allowing a larger batch never enlarges one image.
This complements the checks before RGBA allocation; it does not account all
codec reference buffers or enforce a call deadline.

Admission must cover the whole source lifetime, native codec buffers, returned
pictures, retained scenes and GPU references. `Limits` bounds conversion/output
and native threading, rather than all decoder memory or wall time. Build and
runtime requirements, lifecycle semantics, ABI support, and provenance are in the
[native support README](../video/README.md). Frame publication, clocking, EOS,
pause/flush and exact physical display receipts remain the host's responsibility.
