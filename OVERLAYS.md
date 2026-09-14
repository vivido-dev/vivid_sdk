# Pane overlays

The Rust SDK now exports `OverlaySession`, `OverlayWindow`, and `OverlayWindowOptions`.
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
Initial readiness is bounded to five seconds before activation. Later calls acknowledge submission,
not physical presentation; the producer does not claim a superseded scene reached the GPU.
The high-level helper currently budgets at most 256 KiB per record, reduced by the session's
negotiated limits. The low-level vector channel APIs remain available for explicit resource claims.

The `overlay` module re-exports portable Canvas commands, paths, brushes, transforms, text, and
hit roles. Host text shaping runs off the UI loop. Upload RGBA pixels with `window.upload_rgba`;
`window.draw_image` checks the retained handle's window ownership before adding it to a Canvas.
Assets remain retained until window/channel cleanup; independent asset release is still pending.

`wait_event(Duration)` returns typed pointer, wheel, key, text, IME, geometry, focus, dismissal,
and connection-loss events. `capture_pointer` validates ownership and the current scene revision.
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

## Remaining work and acceptance

- Python blocking/async and TypeScript native async high-level window/Canvas wrappers.
- Explicit per-submission presented/superseded outcomes, richer reconciliation across replacement
  tracks, independent retained-asset release, and unsolicited typed viewport updates.
- Host text measurement and focused-editor geometry for platform IME positioning.
- Full live visual/input acceptance, including clipboard policy, IME placement, accessibility,
  platform-specific input behavior, and performance measurements.
- The separate declarative UI engine, layout/editing services, accessibility bridge, and gallery.

The new socket integration regression exercises two producers reusing local IDs, window movement,
modal focus protection, popup dismissal, IME event delivery, capture, and independent cleanup.
It also performs actual Vello GPU readback, verifies DPI placement and clipping, and checks that
idle frames and window movement preserve cached texture identity. This is not a substitute for
live native pane interaction acceptance. No new live pane acceptance or benchmark has completed.

Validation on Windows includes Rust default and presenter-enabled SDK tests, the protocol and
gateway suites, and Vivido's workspace suite (708 library tests passed; 4 ignored, plus passing
workspace targets). Vivido's suite passed with one test thread after an IPC global-state race
in the concurrent run. A protocol socket-shutdown timeout also passed on serial rerun. Existing
Python and TypeScript bindings build successfully; 27 Python and 18 TypeScript tests pass, along
with mypy and TypeScript typechecking. Python's Unix-socket automation suite and live native
interaction checks were not run on this Windows host. These binding checks cover existing APIs,
not high-level overlay wrappers.

See the [normative overlay specification](../vivid_protocol/vivid-protocol-1.5-overlays.md).
