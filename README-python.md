# vivid-sdk for Python

`vivid-sdk` is the typed Python SDK for Vivid Protocol 1.5, for both roles. Python 3.9+ and Rust
1.88+ are required.

The module namespace produces media; `vivid_sdk.presenter` accepts it. One wheel carries both, so a
Python program can be either end of a session — or, as the tests do, both at once.

The 1.5 package intentionally does not retain the old source/ticket API. A stable `Surface` owns
one or more immutable `Track` objects. Each track submits media through a positively accepted
`TrackChannel` generation.

```python
import vivid_sdk as vivid

session = vivid.connect()
surface = vivid.create_surface(
    session,
    vivid.SurfaceConfig(
        logical_width=2,
        logical_height=2,
        role=vivid.ROLE_FIGURE,
        title="pixel",
    ),
)
vivid.place_terminal_surface(
    session,
    surface,
    width=2 << 32,
    height=1 << 32,
)
track = vivid.create_track(
    session,
    surface,
    vivid.RasterTrackConfig(width=2, height=2),
)
channel = vivid.open_track_channel(session, track)
vivid.send_raster(channel, bytes([255, 0, 0, 255] * 4))
```

The channel blocks only its own sender when cumulative flow maxima are exhausted. Control,
unrelated tracks, and realtime audio remain independently serviced. `channel_eos()` writes EOS
after the last media record on that same connection.

Use `wait_track()` with a bounded timeout before activation or when waiting for presentation,
playback, channel, or loss milestones. The async facade keeps the control reader live while the
wait is pending.

For a PNG or JPEG:

```python
presentation = vivid.display_image("image.png")
try:
    input("press enter to remove the image")
finally:
    presentation.close()
```

`display_image()` returns live handles because a clean 1.5 `GOODBYE` destroys the logical session;
the function cannot close immediately while claiming the retained image remains live.

`vivid_sdk.aio` mirrors the blocking lifecycle and media calls with cancellation-safe worker
threads. Native calls release Python while waiting for replies or channel flow.

By default, discovery reads `VIVID_ENDPOINT_CONTROL`, optional interactive/realtime/bulk endpoint
variables, and `VIVID_ROOT_SECRET`. Explicit values are keyword-only. Secrets, channel tags, and
derived keys are absent from handle representations and exceptions.

See [MIGRATING-1.1-TO-1.5.md](MIGRATING-1.1-TO-1.5.md) for the complete breaking-change guide.

## Presenter

A presenter binds an endpoint, issues a capability per pane, and holds the retained scene a producer
sends. Reading a pane is pull-based: there are no callbacks and no queue to drain, so a slow reader
cannot stall the presenter, and `wait_for_media` is a bounded wait for something to read.

```python
from vivid_sdk import presenter

p = presenter.start("tcp:127.0.0.1:0")          # or "unix:/run/user/1000/vivid.sock"
try:
    presenter.update_metrics(p, pane=1, columns=80, rows=24)
    secret = presenter.issue_pane_capability(p, pane=1)   # hand to exactly one producer

    # ... a producer connects to presenter.endpoint(p) with that secret and sends a frame ...

    if presenter.wait_for_media(p, pane=1, timeout=5.0):
        for layer in presenter.capture_pane(p, pane=1).layers:
            frame = layer.content                          # RasterFrame or EncodedImage
            frame.width, frame.height, frame.rgba
finally:
    presenter.close(p)
```

`tcp:` endpoints are restricted to loopback, and port 0 binds an ephemeral port that
`presenter.endpoint()` then reports. A Unix path is created owner-only and removed on close.

A pane capability is capability material: hand it to one producer over something that is not a
command line, and do not log it. It never appears in a handle's `repr`.

`capture_pane` composes the producer's own retained surfaces. It is not a screenshot — terminal text
belongs to a renderer, and the SDK presenter has none. A capture that produced nothing says why in
`skipped`: `undecoded_video` will never produce pixels here, while `no_retained_pixels` is worth
retrying.

`vivid_sdk.aio.presenter` mirrors all of it as coroutines, over the same cancellation-safe worker
threads as the producer facade. `wait_for_media` is the one that matters: it blocks for its whole
timeout, and running it on the event loop thread would stall every other task.
