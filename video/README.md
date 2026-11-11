# Native video support

`vivid_video` contains the FFmpeg elementary-packet decoder shared by Vivido and
native SDK live-video hosts. Applications should enable SDK `video-decode` and
use `vivid_sdk::video`; GUI, presenter, renderer and audio-device dependencies are
not implied. The default producer SDK does not link FFmpeg.

Create each decoder on the media worker that will use it. It deliberately is
neither `Send` nor `Sync`. Decoded RGBA pictures can outlive it and cross worker
boundaries. Their storage returns to that decoder's pool only when its last
owner releases the buffer; each decoder retains at most two free buffers.

`Decoder::new` keeps Vivido's automatic native thread selection and 8,192-axis
limit. `Decoder::with_limits` allows a smaller output budget, batch count and
explicit native thread count. `max_frame_bytes` bounds one image separately from
the returned batch's `max_output_bytes`; the smaller ceiling also sets the native
pixel limit. Coded and decoded dimensions, encoded access-unit
sizes, each converted frame and the combined returned batch are checked. Defaults
allow at most 256 MiB of RGBA output and 65 pictures per call. Native reference
buffers and decoder-internal allocations are additional memory. This interface
does not enforce a decode deadline or interrupt an in-progress codec call.
`recycle_buffers: false` disables RGBA pooling for externally charged host
buffers. FFmpeg's `max_pixels` option is also set before opening the codec,
complementing the checks before RGBA conversion.

The caller authenticates media, validates complete track/channel configuration,
and owns resource admission and playback. The decoder checks canonical video
codec/packetization pairs and relevant allocation lengths independently. It
preserves microsecond timestamps, mode-aware threading, declared matrix/range
conversion, software AV1 selection, late-frame conversion avoidance and clean
EOS draining. It does not schedule playback, mix audio, grant input, perform
display acknowledgement, or offer a complete color-managed output pipeline.
Retire a decoder after an error.

Build prerequisites are native FFmpeg 6, 7, 8 or 9 development libraries:
`libavcodec`, `libavutil` and `libswscale` through `pkg-config`, or the matching
dynamic FFmpeg libraries in a Windows vcpkg triplet. AV1 requires `libdav1d`
decoder support. Executables remain responsible for packaging runtime libraries;
Vivido's existing Windows staging and delay-load configuration remain in place.
Native Windows execution must be verified separately from cross compilation.

The hidden `ffi` module exposes verified ABI helpers for the SDK's shared native
audio support. Applications use `vivid_sdk::audio` for safe decoding; the ABI
module carries unsafe contracts and is not the ordinary application API.
ABI probes run before native structure access and refuse unrecognized layouts.

## Provenance

Migrated on 2026-11-08 from the CI repository's `vivido/src/vivid/decoder.rs`,
`vivido/src/vivid/ffmpeg.rs`, and `RgbaBuffer` in `vivido/src/vivid/scene.rs`.
Vivido's base revision was `8d0f3c8881e36cad035207d12aab17defda2f54a`;
the active worktree also contained ongoing compositor integration. The migrated
sources and subsequent changes retain Vivido's Apache-2.0 license, included here.
The synthetic regression streams and generation commands are in `fixtures/`.

```sh
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```
