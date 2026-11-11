# Native audio support

`vivid_audio` contains the FFmpeg audio decoder and capture resampler shared with
Vivido. Applications use `vivid_sdk::audio` through optional `audio-decode`.
It opens no audio device, GUI, terminal, presenter or event loop. Device queues,
mixing, playback clocks, authenticated ingress and display/drain receipts remain
host responsibilities.

Create `Decoder` on the media worker that uses it. It is neither Send nor Sync.
Returned interleaved f32 samples own their storage and survive decoder retirement.
`push_timed` and `finish_timed` return `DecodedBatch` with bounded `DecodedSpan`
ranges covering every sample. Positions use actual decoded frame PTS in the
configured microsecond packet timebase, plus exact output-frame offsets. Packet
duration does not determine PCM placement. Unknown native PTS remains unknown
until a timeline obtains an anchor; it is never retroactively assigned to an
earlier unknown run. `push` and `finish` discard the same path's metadata without
copying its samples.
`finish` drains decoder and resampler once. Any decode failure retires the decoder;
create a new one for recovery. Canonical initialization is validated before native
allocation. Packet bytes cannot exceed the immutable access-unit contract. Native
rate, channel layout and sample-format changes are checked before conversion.
Frame metadata is inspected through FFmpeg's own frame API with a reusable
metadata-only context. A codec context that still reports the original format
cannot hide a different returned frame. Rust does not read frame-tail offsets.

`Limits` bounds conversion scratch storage and each combined returned batch to at
most 16,777,216 interleaved samples (64 MiB). It also limits decoded frames and
resampler drain iterations to 256, and native threads to 64 (one by default).
Trim accumulation and cumulative output sizes use checked accounting; nonfinite
samples are refused. FFmpeg's `max_samples` option is set before opening the
decoder. Codec-internal storage and native call duration are additional; this
interface does not implement a hard memory limit or interrupt native calls.
Hosts must reserve their aggregate budgets before accepting tracks.
Conversion can temporarily retain both the batch and its scratch buffer; allow
up to twice the sample budget for this Rust storage, in addition to native
codec/resampler allocations and samples retained by the host.
Also reserve `max_output_frames * size_of::<DecodedSpan>()` for timing metadata.
Timestamp arithmetic and timeline serials are checked; exhaustion retires the
decoder just like a native error.

Packet trim carries across delayed decoder output. OpusHead pre-skip belongs to
FFmpeg and must not also appear as packet leading trim. Output conversion supports
8–384 kHz and one through 32 channels; input follows the Vivid canonical audio
configuration. `CaptureResampler` converts mono float input to 48 kHz signed-16
output in caller-owned storage. It refuses insufficient buffers before consuming
input, keeping native delayed-input storage bounded.

A native PTS gap or overlap drains the previous resampler tail with its original
origin and offsets, then starts a new conversion timeline. Packet trim boundaries
also drain the previous filter before applying new trim. End padding is removed
after the current filter tail, so it cannot cut a hole before delayed samples.
Conversion can restart at these boundaries; timeline serials identify conversion
runs and carry no protocol authority. Frame offsets include removed leading
samples. Device staging, silence insertion and overlap policy belong to the host.

Native builds require FFmpeg development libraries. The shared verified ABI comes
from `vivid_video`, which currently links libavcodec, libavutil and libswscale;
audio additionally links libswresample. Windows follows the existing pkg-config
or vcpkg import-library convention. Executables own runtime-library packaging.
Audio support remains independent of GUI and CPAL dependencies.

## Provenance and verification

Migrated on 2026-11-08 from the CI worktree's `vivido/src/vivid/audio.rs`, based
on Vivido revision `8d0f3c8881e36cad035207d12aab17defda2f54a`. Vivido keeps its
device output, bounded ring, channel-scoped controls and linked playback clock.
The old local decoder and private FFmpeg forwarding module were removed.

Tests run without audio devices. Synthetic PCM tests and four compressed-codec
fixtures cover Opus pre-skip, AAC priming, Xiph-laced Vorbis initialization, raw
FLAC STREAMINFO, exact output counts, EOS/resampling, format changes, resource
bounds and independently retained samples. See [fixture generation](fixtures/README.md).
