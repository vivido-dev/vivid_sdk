# Pane overlays

Rust, Python, and TypeScript expose `OverlaySession`, `OverlayWindow`, and window options.
Direct Vivido connections have window-control, vector-channel, cached Vello rendering, and
interactive-lane integration. This is a development preview: the complete cross-language overlay
and GPUI-inspired UI plan has not passed live pane acceptance.

The terminating SDK presenter and terminating gateways still reject these profiles explicitly.
Vivido offers them only for terminal targets with native viewport geometry installed. Existing
SDK examples are unchanged.

## Rust window API

```rust,no_run
use vivid_sdk::{OverlaySession, OverlayWindowOptions};
use vivid_sdk::overlay::{Brush, Canvas, Color, Path, Rect, WindowMode};

let overlays = OverlaySession::from_env()?;
let window = overlays.create_window(OverlayWindowOptions::new(
    Rect::new(40., 40., 320., 180.)?,
    WindowMode::Floating,
))?;
let mut canvas = Canvas::new();
canvas.fill(
    Path::rectangle(Rect::new(0., 0., 320., 180.)?)?,
    Brush::Solid(Color(0x203050ff)),
)?;
window.present(canvas)?;
window.center()?;
// Pump overlays.wait_event(timeout) on the application's event loop.
// Match events to a window with overlays.event_targets(&event, &window).
window.close()?;
overlays.close()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`connect(ProducerConfig)` negotiates the required profiles automatically; `from_env()` uses
standard pane discovery and capability authentication. `from_session()` consumes an already
negotiated session. The session owns its windows; dropping it cancels their transport even if
window handles remain alive. An independent bounded renewal worker maintains the input lane
without invoking application callbacks.

Windows support `set_bounds`, `set_visible`, `center`, `request_focus`, `raise`, `lower`, and
explicit `close`. `bounds()` and `viewport()` query host truth, including DPI scale. Use
`create_child(&parent, options)` for parent-scoped popup or modal windows. Foreign-session parent
handles are rejected before sending anything. Coordinates are viewport logical pixels and do
not follow terminal scrollback.

`present(Canvas)` reuses the window's surface, immutable vector track, and authenticated channel.
Initial readiness is bounded to five seconds before activation. Use `submit(Canvas)` to obtain
an `OverlaySubmission` with an opaque identity and a full-width `revision()`. Its bounded
`wait(Duration)` returns `Some(Presented)`, `Some(Superseded)`, or `None` on timeout. A timeout
leaves the receipt usable; lane or track loss returns an error. Waiting does not consume input events.
Presented means successful host composition, not physical scanout. `present` remains a convenience
that submits and discards the receipt. Unresolved submissions are bounded to 256 per session.

`reconcile()` returns authoritative geometry, viewport and window revisions, highest accepted
revision, active revision, last presented revision, and focus. `replace_track(Canvas)` primes
and activates a new immutable track on the same surface, then retires its predecessor, returning
a submission receipt. Window revisions continue across replacement. The old complete scene stays
visible until composition of its replacement; old-track image handles cannot be reused on the
new track. The initial replacement Canvas must contain no old image references.
The high-level helper currently budgets at most 256 KiB per record, reduced by the session's
negotiated limits. The low-level vector channel APIs remain available for explicit resource claims.

The `overlay` module re-exports portable Canvas commands, paths, brushes, transforms, text, and
hit roles. Host text shaping runs off the UI loop. Upload RGBA pixels with `window.upload_rgba`;
`window.draw_image` checks the retained handle's window ownership before adding it to a Canvas.
`release_image(&image)` removes an image from future lookup independently of window lifetime.
Previously submitted scenes retain their references and decoded storage remains charged until
those references are gone. Drawing or submitting a prebuilt Canvas with a released image fails.

`wait_event(Duration)` returns typed pointer, wheel, key, text, IME, geometry, focus, dismissal,
connection-loss, and unsolicited `Viewport` events. Viewport notifications include their own
revision, logical extent, and scale. They arrive initially and on changes, and may coalesce.
`capture_pointer` validates ownership and the current scene revision.
Popups dismiss on Escape or an outside press and consume the corresponding release. Modal focus
is isolated from other producers. Host drag/resize hit regions change window geometry while the
compiled content stays cached. Native pane focus loss cancels gestures and held input state.

## Host integration

- Control dispatch validates the authenticated owner, context authority, generation, revision,
  viewport, and live input lane before window mutation.
- Each bulk channel processes one bounded vector record at a time. Complete lists compile before
  publication; assets use channel-qualified identities and a bounded aggregate owner budget.
  Credit returns after processing releases record storage. Assets participate in ordered EOS
  without advancing scene IDs or satisfying output readiness.
- Cached vector content renders into a separate overlay texture above terminal glyphs and below
  trusted host UI. Moving a window changes its transform. Unchanged scenes skip overlay GPU work;
  updates preserve the texture referenced by the cached terminal scene.
- Replacement tracks must advance the window's scene revision. Stale activation is rejected
  before changing any slot, keeping the drawing and input revision consistent.
- Input uses its own authenticated lane and actor wakeups. Renewals and watchdog deadlines do not
  share bulk compilation. Lane loss, overflow, session loss, context revocation, and surface
  destruction remove the affected owner's windows and resources. Overlays never become posters.

## Python and TypeScript

Python exports blocking handles at `vivid_sdk.OverlaySession` and async handles at
`vivid_sdk.aio.OverlaySession`. Drawing and event types live in `vivid_sdk.overlay`.

```python
from vivid_sdk import OverlaySession, OverlayWindowOptions
from vivid_sdk.overlay import Brush, Canvas, Path, Rect

with OverlaySession.from_env() as session:
    with session.create_window(OverlayWindowOptions(Rect(40, 40, 320, 180))) as window:
        canvas = Canvas().fill(
            Path.rounded_rectangle(Rect(0, 0, 320, 180), 12),
            Brush.solid(0x203050FF),
        )
        window.present(canvas)
        window.center()
        input("Press Enter to close")
```

For asyncio use `async with await aio.OverlaySession.from_env()` and
`async with await session.create_window(options)`. Await window operations and iterate
`async for event in session.events()`. Canvas building remains synchronous. Native calls release
the GIL; no native worker invokes application callbacks. Cancellation of resource creation waits
for completion and closes the newly created resource. `event.targets(window)` accepts either
blocking or async window handles without acquiring a transport lock.

```typescript
import { OverlaySession, overlay } from "@vivido/vivid-sdk";
const { Canvas, Path, Brush } = overlay;

await using session = await OverlaySession.fromEnv();
await using window = await session.createWindow({
  bounds: { x: 40, y: 40, width: 320, height: 180 }, mode: "floating",
});
const path = Path.roundedRectangle({ x: 0, y: 0, width: 320, height: 180 }, 12);
await window.present(new Canvas().fill(path, Brush.solid(0x203050ff)).hit(1n, path));
await window.center();
for await (const event of session.events()) {
  if (event.kind === "dismissed" || event.kind === "connection-lost") break;
  if (event.kind === "pointer" && event.targets(window) && event.down) break;
}
```

Both connection helpers negotiate the profile bundle automatically and accept the language's
normal connection options. Pass `parent=window` in Python or the parent as `createWindow`'s
second argument in TypeScript to create child windows. Python uses `raise_window()` because
`raise` is a keyword; TypeScript uses `raise()`.

Canvas supports rectangles, rounded rectangles, ellipses, Bézier paths, solid/linear/radial
brushes, strokes, opacity, affine transforms, nested clips, host-shaped text, and hit regions.
`snapshot()` copies the current display list; `present` also snapshots before sending. Paths are
copied when added to a Canvas. Use `validate()` to check a complete list without sending it.
An image returned by `upload_rgba` / `uploadRgba` belongs to its window; `draw_image` / `drawImage`
validates this before appending the retained reference. Await TypeScript `drawImage` before
modifying or presenting the Canvas. `release_image(image)` / `releaseImage(image)` releases its
namespace entry while protecting references in pending or displayed scenes.

Python `window.submit(canvas)` and TypeScript `await window.submit(canvas)` return a receipt;
`receipt.wait(seconds)` returns `"presented"`, `"superseded"`, or a timeout (`None` / `undefined`).
Await both submission and receipt waits in Python's async facade. TypeScript receipt revisions
are `bigint`. Use `reconcile()` for a typed status snapshot and `replace_track(canvas)` /
`replaceTrack(canvas)` for an immutable track replacement. In TypeScript all status revisions
are also `bigint`. Receipt waits remain independent of event iteration.

Colors are straight-alpha sRGB `0xRRGGBBAA`; opacity and gradient offsets are in `[0, 1]`.
Geometry uses logical pixels. Hit roles are `input`, `drag`, `resize`, and `transparent`;
resize edge bits are left=1, right=2, top=4, bottom=8. Save/restore delimit transforms and clips.
Text supports host font family, weight, italic, color, and optional maximum width; measurement
and richer styled text remain separate host-service work.

`wait_event(timeout)` / `waitEvent(timeout)` use **seconds**, bounded to 0–60. Event iteration
uses bounded waits and ends after connection loss or explicit session close. Typed events cover
pointer, wheel, physical key, committed text, IME, focus, geometry, dismissal, cancellation,
and viewport changes (`ViewportEvent` / `kind: "viewport"`, with `revision` and `viewport`).
Python provides event dataclasses; TypeScript provides a discriminated union on `kind`.
IME selections use Python character indexes or JavaScript UTF-16 indexes into the preedit string.
TypeScript scene revisions and application hit IDs are `bigint` throughout. Window, session,
and asset wire identities stay opaque. Closing sessions invalidates their remaining windows.

### Binding validation

Build from the SDK directory with `python -m maturin develop` in the configured virtual
environment and `npm run build:debug`. Run `python -m pytest python-tests/test_overlay.py`,
`python -m mypy --platform linux`, `npm test`, `npm run typecheck`, and
`npm run typecheck:overlay-consumer`. The mypy platform override includes the package's Unix
automation annotations; it does not run Unix-only automation tests on Windows.

For the native Vivido socket/GPU integration test, build both bindings first, set
`VIVID_OVERLAY_TEST_PYTHON` to that environment's Python executable and
`VIVID_OVERLAY_TEST_NODE` to the Node executable, then run from `vivido/`:

```sh
cargo test --lib native_overlay_python_and_typescript_bindings -- --ignored --nocapture
```

The harness starts Python blocking, Python asyncio, and TypeScript producers against isolated
authenticated host sessions. It verifies exact four-color Vello readback, window controls,
retained assets, typed pointer input with an ID above 2^53, IME offset conversion, capture,
child popups, per-submission outcomes, track replacement/reconciliation, independent asset
release, unsolicited viewport changes, and cleanup. Each subprocess has a bounded deadline.
Capabilities are passed through child environments, never command arguments. A Vello adapter
is required; this test fails rather than silently skipping its rendering assertions.

## Remaining work and acceptance

- Host text measurement and focused-editor geometry for platform IME positioning.
- Full live visual/input acceptance, including clipboard policy, IME placement, accessibility,
  platform-specific input behavior, and performance measurements.
- The separate declarative UI engine, layout/editing services, accessibility bridge, and gallery.

The new socket integration regression exercises two producers reusing local IDs, window movement,
modal focus protection, popup dismissal, IME event delivery, capture, and independent cleanup.
It also performs actual Vello GPU readback, verifies DPI placement and clipping, and checks that
idle frames and window movement preserve cached texture identity. This is not a substitute for
live native pane interaction acceptance. No new live pane acceptance or benchmark has completed.

Windows binding validation passes for Python blocking, Python asyncio, and native TypeScript,
including the Vivido socket/GPU test above. The Python SDK, presenter, and overlay suites have
32 passing tests; TypeScript has 22. Python's Unix-socket automation suite and live native pane
interaction checks were not run on this Windows host. Socket-delivered IME events do not prove
platform IME positioning or native input acceptance.

See the [normative overlay specification](../vivid_protocol/vivid-protocol-1.5-overlays.md).
