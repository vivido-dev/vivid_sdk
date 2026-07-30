# vivid-sdk for Python

`vivid-sdk` is the typed Python producer SDK for Vivid Protocol 1.5. Python 3.9+ and Rust 1.87+
are required.

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
