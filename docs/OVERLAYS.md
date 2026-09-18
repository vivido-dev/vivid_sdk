# Pane overlays

Rust, Python, and TypeScript expose `OverlaySession`, `OverlayWindow`, and window options.
Direct Vivido connections have window-control, vector-channel, cached Vello rendering, and
interactive-lane integration. This is a development preview: the complete cross-language overlay
and GPUI-inspired UI plan has not passed live pane acceptance.

The terminating SDK presenter and terminating gateways still reject these profiles explicitly.
Vivido offers them only for terminal targets with native viewport geometry installed: the window,
vector, input, paint, pointer, clipboard, and environment bundles, with cursor and hover
behaviour applied where the platform reports it. It reports the desktop appearance and the
display refresh rate, and reports no reduced-motion preference because it cannot read one.
Example 07 in each language draws an interactive overlay panel and pumps its input lane.

For tests, `vivid_sdk::testing::TestPresenter` serves the profile bundle headlessly: it keeps
window, focus and revision state in the protocol's own state machine, validates every display
list against the negotiated limits, and exposes the accepted lists plus pointer, wheel, key,
text, focus and viewport injection. It composes nothing, so it needs neither a GPU nor a
terminal. The production terminating presenter still refuses these profiles outright.

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
not follow terminal scrollback. `min_width` / `min_height` (Python `min_width`/`min_height`,
TypeScript `minWidth`/`minHeight`) default to one logical pixel and bound host-driven
`HitRole::Resize` gestures; initial bounds smaller than the minimum are rejected locally.

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

Canvas supports rectangles, rounded rectangles (uniform or per-corner), ellipses, Bézier paths,
solid/linear/radial/image brushes, plain and styled strokes, box shadows, opacity, affine
transforms, nested clips, host-shaped text, and hit regions.

Beyond the shape constructors, a path is built from `move_to` / `moveTo`, `line_to` / `lineTo`,
`quad_to` / `quadTo`, `cubic_to` / `cubicTo` and `close`, chained, in all three languages. A
builder is the only way to write a shape the constructors do not cover, and the only way to put
two subpaths in one path — which with the even-odd rule (`Path(even_odd=True)`, `new Path(true)`,
`Path::builder().even_odd()`) is what makes a hole rather than a second layer. A hole is a hole
to the host's hit testing as well, since it hit tests the rule the shape is filled by. All three
builders stop growing at the segment ceiling — Python raises `ValueError`, TypeScript `RangeError`
— and Rust additionally reports a coordinate the wire cannot carry from `build()`, naming the
first thing that went wrong rather than the last.

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
Text supports host font family, weight, italic, color, and optional maximum width.

## Paint commands

Connection helpers optionally negotiate `overlay-paint-v1`. Without it, a paint command is
refused locally at submission with that profile named, rather than failing the channel on the
host. Rust adds `Canvas::shadow`, `Canvas::stroke_styled`, `Path::rounded_rectangle_corners`,
`Brush::Image`, and a `color_space` on gradient brushes. Python adds `Canvas.shadow`,
`Canvas.stroke_styled`, `Path.rounded_rectangle_corners`, `Brush.image`, `Shadow`, and
`StrokeStyle`; TypeScript adds `shadow`, `strokeStyled`, `roundedRectangleCorners`,
`Brush.image`, `Shadow`, and `StrokeStyle`, with `image.id` as a `bigint`.

- **Shadows** are CSS `box-shadow`: a rectangle, four corner radii clockwise from the top left,
  a color, an offset, a blur diameter, a spread, and an inset flag. Radii that overrun their
  sides scale down together, as CSS does. A spread adjusts the radii with the rectangle. Blur
  is 0–4096 logical pixels and spread −4096–4096. The command paints only the shadow; an inset
  shadow is clipped to its own rectangle, and drawing the element over an outer one is the
  application's business.
- **Styled strokes** add caps (`butt`, `round`, `square`), joins (`miter`, `bevel`, `round`), a
  miter limit of at least one, and an optional dash pattern: 1–32 alternating on/off lengths,
  each positive and at most 4096, with a dash offset. Plain `stroke` is unchanged.
- **Image brushes** fill any path with an uploaded image at its natural pixel size, optionally
  repositioned by a six-term affine transform, with `pad`, `repeat`, or `reflect` sampling
  outside its extent. The image belongs to the same window, and a scene naming an asset the
  channel never carried fails whole rather than rendering a placeholder.
- **Gradient color space** selects sRGB (the default) or Oklab interpolation. sRGB scenes encode
  exactly as before, so a host that has not adopted the profile sees no change.

Shadows and dashed strokes cost more host work than a fill: a host may approximate a blur, and
the wire value stays exact so a better renderer improves it without a protocol change.

## Pointer behaviour

Connection helpers optionally negotiate `overlay-pointer-v1`. A hit region may name the cursor the
host shows while the pointer is inside it; omitting it asks for nothing and keeps the region
encoder-identical to before, so a scene that never wanted a cursor does not require the profile.
Rust passes `cursor: Some(CursorShape::Pointer)` on `Command::Hit`; Python adds
`Canvas.hit(..., cursor="text")`; TypeScript adds `hit(id, path, role, edges, "text")`. The set is
closed — `default`, `pointer`, `text`, `move`, `crosshair`, `not-allowed`, `grab`, `grabbing`,
`wait`, `progress`, `resize-left`, `resize-right`, `resize-up`, `resize-down`, `resize-up-left`,
`resize-up-right`, `resize-down-left`, `resize-down-right`, `resize-left-right`, `resize-up-down` —
and an unknown name is refused rather than quietly becoming an arrow. Vivido ranks a region's
cursor below its own chrome and above the terminal's, and keeps it for the whole of a drag or
resize even once the pointer leaves the region.

`Event::Pointer` gains `clicks` and `pressure`. `clicks` is 1–3 for a press and 0 otherwise,
counted by the host from the same 400 ms threshold the terminal uses, so a producer never
reproduces platform timing; a fourth press restarts at one. `pressure` is `Option`-valued because
no sensor and a sensor reading zero are different claims — Vivido reports `None` on hardware it
cannot read. The wire carries these in two trailing elements that a plain motion or release omits,
so both sides agree on the common case.

`Event::Hover { region, entered }` is emitted whenever the hovered region changes: entering,
leaving the window, crossing between windows, and when a newly published scene removes or
replaces the region under a stationary pointer. A producer can drive hover styling without
diffing pointer events and without missing a transition that happened while it was not looking.
Python exposes `HoverEvent`; TypeScript adds `kind: "hover"`.

## Clipboard writes

Connection helpers optionally negotiate `overlay-clipboard-v1`. Rust adds
`window.set_clipboard(&str)`, Python `window.set_clipboard(text)`, TypeScript
`await window.set_clipboard(text)`.

It is deliberately narrow, because a clipboard is shared with every other application on the
machine and is frequently where a password or a command line briefly lives. The API is
**write-only**: no method reads a clipboard back, so an overlay can never observe what the user
copied elsewhere. Paste already reaches a focused overlay as ordinary committed text through
Vivido's normal paste policy, so nothing here is needed to receive one.

Vivido honors a write only when all of these hold, and reports a refusal as an error rather than
dropping it silently:

- the window exists and holds the pane's eligible focus;
- a key press or pointer press was delivered to *that* window no more than two seconds earlier;
and
- the text is non-empty and at most 65536 UTF-8 bytes. Empty text is refused rather than treated
  as a clear.

The gesture requirement is what ties a write to something the user did. Without it an overlay
could replace the clipboard at an arbitrary moment, which is the setup for a paste-hijack: the
user copies a command, an overlay replaces it, and the user pastes something they did not copy.
A gesture delivered to another window is not spendable, and neither is one older than the
ceiling, so a producer cannot bank a gesture and use it later.

A successful reply means the host accepted the text, not that nothing else observed it: on a
shared display server the clipboard may be published to other applications immediately.

## Host environment

Connection helpers optionally negotiate `overlay-env-v1`. The host publishes its defaults on the
interactive lane beside the viewport snapshot: the font a plain `Text` or an empty `TextStyle`
family resolves to, the desktop appearance, the user's motion preference, and how often the
display refreshes. Rust, Python, and TypeScript surface it as an `Environment` on the lane event
(`OverlayLaneEvent::Environment`, `EnvironmentEvent`, `kind: "environment"`).

It is a record of its own rather than more keys on the viewport snapshot, because appearance and
motion preference change independently of geometry. Layout cached per viewport revision must not
be rebuilt because the user switched their desktop theme, and sharing one revision would force
exactly that. Revisions strictly increase when any field changes; queued snapshots may coalesce.

`reduced_motion` is absent rather than false when the host cannot read the preference — a host
that has no such signal must not assert one, exactly as with pointer pressure. A producer that
receives `None`/`undefined` decides for itself, and animating is a reasonable default. Vivido
currently reports absence here, because it has no reduced-motion signal to read.

`refresh_interval_us` is the display's, and absent when the host cannot tell. A producer must
still respect its own track's record ceiling, which may be lower: a 60 rec/s track does not
become faster because the display refreshes at 144 Hz.

## Application semantics

Connection helpers optionally negotiate `overlay-a11y-v1`. It publishes a bounded tree describing
what the overlay is showing, so a screen reader sees named controls instead of a single opaque
canvas. Rust calls `window.set_semantics(&semantics)`, Python `window.set_semantics(semantics)`,
and TypeScript `await window.setSemantics(semantics)`. Unsupported hosts fail explicitly with
`presenter does not support overlay-a11y-v1`.

A tree describes exactly one published scene, and says which: `scene_revision` must be the
revision of the scene currently shown. A tree naming any other revision is refused rather than
stored, so a producer cannot describe a frame the user is not looking at. Publishing a new scene
retires its tree until the next one is set — a stale description is worse than no description,
because it would announce a button that no longer exists at a position where something else is.

Node IDs are the application's own, nonzero and unique within the tree. They are what comes back
when a user acts, so they need not relate to hit-region IDs or to scene revisions. Roles are a
closed set — generic, application, group, heading, text, button, switch, checkbox, radio button,
text input, slider, spin button, progress indicator, list, list item, image, link, dialog, tab,
separator — and an unknown role is rejected. Actions are a closed set of seven: default, focus,
click, increment, decrement, expand, collapse. Each has a counterpart in the toolkits a host
builds on; nothing is advertised that a host would then have to ignore.

The tree is kept acyclic and complete by construction rather than by a traversal:

- A child index is **strictly greater** than its parent's index, so no cycle is possible.
- Every node except the root is claimed as a child exactly once. There are no orphans, so the
  root reaches everything.
- Depth is bounded separately, because index ordering alone does not bound it. A chain of 256
  nodes each numbered above its parent satisfies the first rule and is still refused.

The remaining bounds are 256 nodes, 32 levels, a 256-byte label, three optional numerics
(value, minimum, maximum) that must be ordered, and a bounded action list per node. Set
membership (`position_in_set` / `size_of_set`) is checked against its own size. Bounds and every
child index are validated with checked arithmetic before anything is stored or walked.

An action the user takes arrives on the interactive lane as `OVERLAY_INPUT_EVENT` type 10 carrying
the node ID and the action. Rust reports `OverlayLaneEvent::Accessibility { address,
scene_revision, node, action }`, Python an `AccessibilityEvent`, TypeScript `kind:
"accessibility"`. The node ID is the application's own, so a handler dispatches on it directly.
An action is only ever delivered for a node the window's live tree still names, so an asker
holding a tree from before a scene was replaced is refused rather than sending an action for a
control that is no longer on screen. The `scene_revision` on the event says which scene the node
belonged to.

Vivido maps the tree into its existing AccessKit adapter: overlay nodes become children of the
window root, and a user action routes back to the producer as the event above. That path is Linux
and Windows only. On macOS the native adapter builds its own tree from the terminal snapshot and
never reads the overlay nodes, so a tree set there is accepted, validated, retired with its scene
and reported in `inspect`, but reaches no assistive technology and returns no actions. Publishing
one is not an error there; it is simply not yet visible.

Vivido offers the profile on macOS regardless, and deliberately. A profile says what this wire
carries, not what one platform's adapter happens to read today, and everything the profile claims
does happen there — only the last hop into AppKit is missing. Withdrawing it would put every
producer on a permanent fallback on that platform and make them all change back the day the hop
lands, in exchange for knowledge none of them could act on: there is no other channel a producer
could describe itself through. The gap is observable in `inspect` rather than silent, and
`the_overlay_bundle_is_offered_wherever_the_wire_carries_it` pins the decision so it is not
quietly reversed.

## Host text measurement and editor geometry

Connection helpers optionally negotiate `overlay-text-v1`. Measurement and editor operations
fail explicitly if the host did not negotiate it. Vivido shapes measurements off the UI loop
with the same Parley/font-fallback implementation used to paint Canvas text.

- Rust: `window.measure_text(&Text)` returns `TextMeasurement`.
- Python: `window.measure_text(text, size, family="", weight=400, italic=False, max_width=None)`.
  Await the same method in the asyncio facade.
- TypeScript: `await window.measureText(text, size, { family, weight, italic, maxWidth })`.

Results contain logical-pixel width, height, line geometry, and visual-order cluster geometry.
Geometry includes text ranges, bounds, baselines, and cluster direction. Rust ranges use UTF-8
byte offsets; Python uses character indexes; TypeScript uses UTF-16 indexes. Measurement starts
at `(0, 0)` independently of Canvas text origin and color. These are measurement snapshots,
not retained layout handles: use matching text/style when painting. For guaranteed
measurement/painting consistency, use the retained layout service below.

After a submission is presented and its window has focus, publish the editor's window-local
caret rectangle with `set_editor_geometry(revision, Some(rect))` in Rust,
`set_editor_geometry(revision, rect)` in Python, or
`await setEditorGeometry(revision, rect)` in TypeScript. TypeScript revisions remain `bigint`.
For example, with an existing Python window and Canvas:

```python
measurement = window.measure_text("Hello", 20)
receipt = window.submit(canvas)
if receipt.wait(5) == "presented":
    window.request_focus()
    window.set_editor_geometry(receipt.revision, Rect(20, 20, 1, measurement.height))
```

Include any producer transforms and scrolling in that rectangle. Vivido clips it to the window
and pane, applies current DPI scale, and updates the native IME cursor area when the window moves.
Publish geometry again after changing the scene revision. Clear it with `None` in Rust/Python
or an omitted rectangle in TypeScript. Focus loss, hiding, closing, lane loss, and stale scene
revisions also clear it. Terminal draws cannot overwrite an active overlay editor rectangle;
clearing it restores the latest available terminal IME area.

Requests are bounded to 4,096 UTF-8 text bytes and replies to 1,024 lines and 1,024 clusters,
subject to negotiated control-record limits. Vivido permits one outstanding shaping job per
owner and at most 16 globally; overload fails explicitly without blocking input or rendering.

### Batched measurements and retained styled layouts

Connection helpers also optionally negotiate `overlay-text-layout-v1`. `StyledText` describes a
paragraph of `TextRun` values with `TextStyle`: size, family, weight, italic, color, underline,
and strikethrough. Runs shape together with host font fallback. Paragraph options provide maximum
width, start/center/end/justify alignment, wrapping, and a maximum line count. Disabling wrapping
preserves explicit line breaks. By default, width overflow and extra lines are clipped. The
optional typography service below adds ellipsis insertion. With a maximum width, the measured width is the paragraph box width; geometry
still reports actual cluster positions, including horizontally clipped content.

| Operation | Rust / Python | TypeScript |
|---|---|---|
| Measure one batch without retention | `measure_text_batch` | `measureTextBatch` |
| Measure and retain one paragraph | `layout_text` | `layoutText` |
| Measure and retain a batch | `layout_text_batch` | `layoutTextBatch` |
| Add a retained layout to a Canvas | `draw_text_layout` | `drawTextLayout` |
| Release future use of a layout | `release_text_layout` | `releaseTextLayout` |

Python's asyncio facade awaits each operation; TypeScript operations are asynchronous. A retained
layout exposes `measurement()` in Rust and `.measurement` in Python/TypeScript. Wire IDs stay
opaque. Line/cluster ranges use the same language-specific offsets as single measurements.
Example with an existing Python window:

```python
from vivid_sdk.overlay import Canvas, Point, StyledText, TextRun, TextStyle

paragraph = StyledText(
    (TextRun("Hello ", TextStyle(size=20)),
     TextRun("world", TextStyle(size=20, weight=700, color=0x66CCFFFF, underline=True))),
    max_width=260, alignment="center", max_lines=2,
)
layout = window.layout_text(paragraph)
scene = Canvas()
window.draw_text_layout(scene, layout, Point(20, 20))
receipt = window.submit(scene)
if receipt.wait(5) == "presented":
    window.release_text_layout(layout)
```

The host compiles a retained glyph scene from the exact measured layout. Repainting, movement,
and replacement tracks reuse it without shaping again. Existing layouts keep their fonts and
glyph positions if host font settings change; create a new layout to adopt those changes.
Canvas transforms, clipping, and opacity apply normally. Handles belong to one window and remain
valid across its track replacements. Other windows, including another producer's reused local
IDs, cannot use them.

Release removes future lookup, preserving scenes that already resolved the layout. Wait for a
presented receipt before releasing when the submitted scene must keep it: control release and
bulk submission use separate lanes. The SDK rejects new submissions of prebuilt Canvases referring
to released layouts. Closing/dismissing the window or losing its owner/input lane removes its
namespace. Released resources stay charged while pending or displayed scenes reference them.
Python cancellation during retained batch creation waits for completion and releases the results.

A batch has 1–32 paragraphs, at most 64 runs and 4,096 UTF-8 text bytes in total, and at most
1,024 returned line-plus-cluster entries. Each family is at most 256 bytes. Whole batches fail
atomically on malformed input, overload, or oversized replies. Retention is bounded to 128 layouts
per owner and 2,048 globally, including released layouts still used by scenes. Single and batch
requests share the existing worker limits. Terminating SDK presenters/gateways decline this profile.

### Ellipsis and typography

Connection helpers optionally negotiate `overlay-typography-v1`. Rust configures
`StyledText.typography` with `Typography` and `TextOverflow::{Clip, Ellipsis}`. Python uses
`StyledText(overflow="ellipsis", letter_spacing=..., word_spacing=..., line_height=...,
ligatures=..., kerning=...)`; TypeScript uses the corresponding camelCase fields. These options
work with both measurement batches and retained layouts. Unsupported hosts fail explicitly.

- `overflow="ellipsis"` requires `max_width` / `maxWidth`. It fits a prefix plus `…` within the
  width and `max_lines` / `maxLines` (one line when omitted). It preserves grapheme sequences,
  original RTL/LTR direction, and the last kept run's style. If even the marker cannot fit,
  no text is drawn. The default overflow remains `"clip"`.
- Letter/word spacing add 0–1,024 logical pixels across the paragraph; defaults are zero.
- Absolute line height accepts positive values up to 4,096 logical pixels; omission uses host
  font metrics. Small values may clip tall glyphs within the measured box.
- Ligatures and kerning default to host behavior. Setting them false disables optional
  ligatures/contextual alternates or kerning, while preserving required script shaping.

```python
paragraph = StyledText(
    (TextRun("A long status message that may not fit", TextStyle(size=18)),),
    max_width=220, max_lines=2, overflow="ellipsis",
    letter_spacing=0.5, word_spacing=2, line_height=26,
    ligatures=False, kerning=True,
)
layout = window.layout_text(paragraph)
cut = layout.measurement.truncated_at  # None when the complete text fits.
```

Rust returns `TextMeasurement.truncated_at` in UTF-8 bytes; Python returns character indexes;
TypeScript exposes optional `truncatedAt` in UTF-16 units. All cluster ranges refer to the
original text. The inserted marker has an empty range at the cutoff and positive visible geometry;
it is not part of the application's editable string. The retained layout paints the exact fitted
result, including spacing and decorations, without reshaping on later frames. Prefix fitting has
bounded work and does not guarantee the longest possible prefix with unusual contextual font metrics.

`wait_event(timeout)` / `waitEvent(timeout)` use **seconds**, bounded to 0–60. Event iteration
uses bounded waits and ends after connection loss or explicit session close. Typed events cover
pointer, wheel, physical key, committed text, IME, focus, geometry, dismissal, cancellation,
and viewport changes (`ViewportEvent` / `kind: "viewport"`, with `revision` and `viewport`).
Python provides event dataclasses; TypeScript provides a discriminated union on `kind`.
IME selections use Python character indexes or JavaScript UTF-16 indexes into the preedit string.
TypeScript scene revisions and application hit IDs are `bigint` throughout. Window, session,
and asset wire identities stay opaque. Closing sessions invalidates their remaining windows.

Physical keys, pointer buttons, and modifier masks are protocol values, never a host's platform
encoding, and they match `desktop-surface-v1`'s assignments. Rust re-exports
`overlay::{keys, buttons, modifiers}`; Python exposes `overlay.Key`, `overlay.MouseButton`, and
`overlay.Modifiers`; TypeScript exports `overlay.Key`, `overlay.MouseButton`, and
`overlay.Modifiers`. A physical key is a USB HID keyboard-page usage in `0x04..=0xe7`, with zero
for a key the page does not name. Buttons are primary, auxiliary, secondary, back, forward, then
`5..=31`. Modifiers are shift, control, alt, super, caps lock, and num lock; every other bit is
reserved and an event that sets one is rejected rather than dispatched.

Wheel events add `precise` (true for trackpads, false for detented wheels whose detents the host
has already converted to logical pixels) and `phase` (`"none"`, `"began"`, `"changed"`, `"ended"`,
`"cancelled"`). Deltas stay in logical pixels, so ignoring both fields still scrolls correctly.

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
release, unsolicited viewport changes, host text measurement, focused-editor geometry, styled
measurement batches, retained layout colors, replacement-track reuse, release, and cleanup.
Each subprocess has a bounded deadline.
Capabilities are passed through child environments, never command arguments. A Vello adapter
is required; this test fails rather than silently skipping its rendering assertions.

## Live acceptance status

Vivido's `inspect` reply exposes the active overlay window/resource counts, compilation and
presentation totals, Vello overlay render/skip passes, target allocations, native IME cursor area,
and accessibility focus state. This makes cache, cleanup, queue, and focused-editor checks
repeatable without exposing producer content or capabilities.

The bounded live probe is `bindings/tests/overlay_live_acceptance.py`. It presents retained text,
requests focus, publishes editor geometry, reports typed key/text/IME/focus events, and exits on
Escape. Its hit region also reports pointer and wheel events for native interaction checks.
UI-routed automation keys now traverse the same overlay path as native keys and synthesize a
complete HID press/text/release sequence. UI-routed paste traverses Vivido's normal paste policy:
it reaches the focused overlay as committed Unicode text and falls through to the terminal only
when no overlay or Vivido UI mode consumes it.

On the Windows test host at 125% DPI, live visual acceptance confirmed pane clipping, retained text,
overlay focus, Unicode typing, and a physical IME cursor area derived from the focused editor. The
reported logical editor rectangle `(112,186,2,24)` became physical `(140,232.5,2.5,30)`. The native
accessibility adapter now remains installed on Windows and Linux with a bounded window tree even
though retained terminal scrollback is disabled there; while an overlay owns focus, the terminal
is no longer exposed as the focused accessibility element. Canvas windows describe nothing on
their own: an overlay that negotiates `overlay-a11y-v1` publishes a bounded tree, and assistive
technology reading it has not yet been verified on either platform.

The release-mode movement benchmark
`cargo test --release -p vivido vivid::tests::overlay_window_movement_performance_measurement -- --ignored --exact --nocapture`
rendered 240 changing window transforms with one target allocation and no display-list
recompilation. This host measured 537 microseconds p50, 686 microseconds p95, and 3,876 microseconds
maximum for the control update plus Vello overlay preparation, below a 16.667 ms 60 Hz frame at
p95. The benchmark fails if p95 exceeds that budget or if movement reallocates the target or
recompiles content.

Remaining platform acceptance is native IME candidate UI and assistive-technology interaction on
Windows, macOS VoiceOver and IME, Linux Wayland IME/accessibility, and pressure-capable pointer
hardware. Nothing is inferred from low-level drawing commands, so an overlay that draws a button
without describing one is not announced as a button.

The declarative UI engine is `vivid_ui/`, and it now supplies what that sentence anticipated:
bounded semantic trees built from its element tree and republished for each presented scene,
editable text with selection and copy, and the six optional profiles driven from one place.
Every one of its examples is exercised end to end against the SDK's in-process presenter, which
is the real handshake, the real framing and the real display list — but not a real display. None
has yet been run in a live Vivido pane, so the following remain unconfirmed on any platform:
that the display lists it builds render as intended, that its semantic trees reach a screen
reader through Vivido's AccessKit adapter and that actions come back, that its editor geometry
places a native IME candidate window, and that its frame pacing holds against a real compositor
rather than against a test presenter's timings.

The new socket integration regression exercises two producers reusing local IDs, window movement,
modal focus protection, popup dismissal, IME event delivery, capture, and independent cleanup.
It also performs actual Vello GPU readback, verifies DPI placement and clipping, and checks that
idle frames and window movement preserve cached texture identity. This is not a substitute for
live native pane interaction acceptance. A Windows live-pane probe verified editor geometry
reaching the window backend, following window movement at 125% DPI, and clearing on request.
Native IME candidate-popup UI remains unverified; the locally cached platform rectangle and its
DPI/window-placement updates are verified.

A separate Windows live-pane check verified retained styled layouts at 125% DPI: centered mixed
sizes/colors, wrapping, italic, underline, and strikethrough. Moving the window and releasing its
layouts preserved the displayed glyph scenes. The socket/GPU harness additionally verifies both
run colors through Python blocking, Python asyncio, and TypeScript. Native Rust regressions cover
atomic batch rejection, quota recovery, in-flight release accounting, and two-owner cleanup.

Typography validation includes LTR/RTL ellipsis, combining sequences, emoji grapheme boundaries,
mixed styles, spacing, absolute line height, and narrower-than-marker boxes. Python blocking,
asyncio, and TypeScript socket/GPU workflows validate original-string truncation offsets and
retained painting. A Windows live-pane check at 125% DPI confirmed styled and RTL marker placement,
two-line fitting, spacing, and the empty result for a 1-pixel box. Other-platform visual checks
remain unavailable in this session.

Windows binding validation passes for Python blocking, Python asyncio, and native TypeScript,
including the Vivido socket/GPU test above. The Python SDK, presenter, and overlay suites have
33 passing tests; TypeScript has 22. Python's Unix-socket automation suite and live native pane
interaction checks were not run on this Windows host. Socket-delivered IME events do not prove
platform IME candidate UI on hardware outside the Windows test host.

See the [normative overlay specification](../vivid_protocol/vivid-protocol-1.5-overlays.md).
