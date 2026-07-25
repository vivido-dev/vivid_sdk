# vivid-sdk for Python

`vivid-sdk` is the typed Python interface to the Rust `vivid_sdk` producer library. It connects to
a Vivid 1.1 presenter, manages scene sources, and submits raster, encoded image, video access-unit,
and audio access-unit records. It does not decode media files; callers supply already-decoded RGBA
pixels or codec access units.

The public API is function-oriented. `Session`, `Source`, and `MediaSender` are opaque native
handles with read-only `id`, `kind`, and `closed` state where applicable. Blocking calls release
Python while Rust waits for control replies or source-scoped media credit, so unrelated Python
threads and independent senders can continue running.

## Install for development

Python 3.9 or newer and Rust 1.85 or newer are required.

```sh
uv venv -p 3.9
uv pip install -e '.[test]'
```

Maturin builds the private `vivid_sdk._native` extension. Application code should import only
from `vivid_sdk` or `vivid_sdk.aio`.

## Connect and close

By default `connect()` reads `VIVID_ENDPOINT`, `VIVID_ENDPOINT_BULK`, and `VIVID_TOKEN`. Explicit
values can be supplied as keyword arguments. Keep the token out of logs and command arguments.

```python
import vivid_sdk as vivid

session = vivid.connect()
try:
    print(vivid.display_state(session))
finally:
    vivid.close(session)
```

Use `dry_run=True` to validate control and media records without a presenter. Supplying
`trace_dir=...` also creates an offline session and writes Vivid trace files.

## Display an image in one call

For the common retained-image case, the SDK inspects PNG/JPEG dimensions, fits the image to the
terminal, creates the anchor and source, reserves the occupied rows, sends the image, and closes
the session:

```python
import vivid_sdk

vivid_sdk.display_image("image.png", scale=1.0)
```

The compact runnable version is in `examples/vivid_image.py`. Use the lower-level functions below
when an application needs to reuse a session or manage its own scene nodes.

## Raster and encoded images

Sources receive an ID automatically. Opening a sender consumes the `Source` handle; subsequent
operations may use the sender itself anywhere a source ID is accepted.

```python
source = vivid.create_raster_source(session, width=2, height=2)
vivid.place_source(session, source, columns=2, rows=1)
sender = vivid.open_sender(session, source)
vivid.send_raster(
    sender,
    bytes([255, 0, 0, 255] * 4),
    width=2,
    height=2,
)
vivid.destroy_source(session, sender)
```

For PNG or JPEG data, declare its encoded size and optional SHA-256 digest, then send the complete
image as one record:

```python
import hashlib

encoded = open("image.png", "rb").read()
source = vivid.create_image_source(
    session,
    vivid.ImageSourceConfig(
        encoding="png",
        width=640,
        height=480,
        encoded_length=len(encoded),
        sha256=hashlib.sha256(encoded).digest(),
    ),
)
sender = vivid.open_sender(session, source)
vivid.send_image(sender, encoded)
```

## Encoded video and audio

`VideoSourceConfig` and `AudioSourceConfig` describe canonical, container-independent Vivid access
units. Demuxers must normalize codec initialization and packetization before calling the SDK.

```python
video = vivid.create_video_source(
    session,
    vivid.VideoSourceConfig(
        codec="h264",
        packetization="annex-b",
        width=1920,
        height=1080,
        max_access_unit_bytes=4 * 1024 * 1024,
    ),
)
vivid.place_source(session, video, columns=80, rows=24)
sender = vivid.open_sender(session, video)

vivid.send_video(
    sender,
    access_unit,
    packet_id=1,
    pts_us=0,
    dts_us=0,
    duration_us=33_333,
    key=True,
)
vivid.play(session, sender, start_pts_us=0, minimum_buffer_us=100_000)
vivid.wait_until_playing(session, sender, timeout=5.0)
vivid.eos(session, sender, epoch=1)
vivid.drain(session, sender, timeout=10.0)
```

Audio uses `send_audio()` with packet timing and optional trim samples. Use
`create_linked_av_sources()` when audio must be clocked to video. If the presenter accepts video
but rejects linked audio, `LinkedAudioError.video_source` retains the usable video handle.

## Asyncio

`vivid_sdk.aio` mirrors the blocking functions through worker threads:

```python
from vivid_sdk import aio

session = await aio.connect(dry_run=True)
try:
    source = await aio.create_raster_source(session, 2, 2)
    sender = await aio.open_sender(session, source)
    await aio.send_raster(sender, bytes([0, 0, 0, 255] * 4), width=2, height=2)
finally:
    await aio.close(session)
```

Cancelling an async media send wakes its Rust credit wait and closes that sender. Other cancelled
operations finish native cleanup before `CancelledError` is delivered, so handles are never being
mutated by an abandoned worker thread.

## Errors and events

Invalid values raise `ValueError` or `OverflowError`; expired deadlines raise `TimeoutError`;
transport and protocol failures raise `VividError`; and reuse of a consumed or closed handle raises
`ClosedHandleError`. `take_event()` returns `VisibilityEvent`, `NeedKeyframeEvent`, or
`SourceLostEvent` without exposing internal tickets or authentication material.

## Observability and cancellation-safe waits

When `FEATURE_OBSERVABILITY_CORE_V1` is accepted, queries return typed snapshots and revisions:

```python
vivid.set_observation(session, vivid.OBSERVATION_CLASS_MASK)
status = vivid.query_source(session, sender)
scene = vivid.query_scene(session, maximum_nodes_per_page=256, maximum_pages=16)

wait = vivid.begin_wait_source(
    session,
    sender,
    vivid.WAIT_FIRST_VISIBLE_PRESENTATION,
    timeout=10.0,
)
try:
    milestone = vivid.wait(wait)
finally:
    vivid.cancel_wait(wait)  # harmless after completion
```

Scene pagination is caller-bounded and revision-bound. Dropping a Rust wait handle, explicitly
cancelling a Python `Wait`, or cancelling `await aio.wait(wait)` sends `CANCEL_WAIT`; it does not
leave presenter wait state behind. `play()` reports admission only. Use `wait_until_playing()` or
`play_and_wait_until_playing()` only when actual playback start is required.
