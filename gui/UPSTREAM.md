# GPUI source provenance

Source: https://github.com/zed-industries/zed

Revision: `4a6121bb0f10c6e8037b741b7dc0a4cd639ae289`

Extracted by `../scripts/extract_gpui.py`; includes the recursive local build, test, and optional dependency closure of gpui and gpui_platform. Original crate names and license files are preserved. This workspace is independent of the SDK protocol workspace.

Local additions: `TestAppContext::build_with_renderer` constructs a deterministic
GUI test context with native text and real GPU scene rendering. Its renderer
factory uses the existing platform abstraction; no private platform type escapes.

`ElementInputHandler::with_apple_press_and_hold` lets terminal views request raw
Apple key repeats without replacing previously committed terminal text.

`Window::hide_cursor_until_mouse_moves` scopes native typing-hide requests to an
active window's unoccluded hitbox. Test contexts expose cursor styles and a hide
request counter for hosted application acceptance; they do not simulate physical
pointer visibility. Local regressions cover hitbox occlusion.

`VisualTestContext::debug_bounds` accepts borrowed selectors, so acceptance checks
can locate controls named by runtime instance, workspace, or tab identities.

The optional `offscreen` feature exposes production frame readback and standalone
Metal/WGPU renderers without enabling test dispatchers. Linux headless windows
own a real WGPU sprite atlas and renderer under this feature; callers drive their
frames explicitly. Shared checked geometry caps each RGBA frame at 64 MiB, and
Metal readback waits at most five seconds for GPU completion. Native Windows
readback uses the same size validation before staging allocation. SDK-level
production examples verify native text, changed content, and exact authenticated
raster transport on macOS and Linux without `test-support`.

`Window::needs_draw` and a checked `render_revision` let a production remote app
observe native draws and avoid idle readback. `dispatch_cancelled_input` carries
held-state releases after authority loss while suppressing default pointer and
keyboard activation clicks. A core regression verifies pending clicks clear and
later ordinary activation still works.

`Window::dispatch_keystroke_with_repeat` shares ordinary synthetic keystroke
dispatch and text insertion while retaining `KeyDownEvent::is_held`. The remote
producer checks its original input binding and watchdog before synthesizing
typematic; this GUI helper supplies no input authority.

`Window::advance_offscreen_frame` drives local next-frame callbacks for production
headless callers. It drains one callback generation per turn and does not claim
remote presentation. The SDK counter example updates through this callback path.

`App::read_clipboard_text_async` accepts a selection and byte ceiling. Text entry
lengths are checked before concatenation. Linux admits one external transfer per
platform, retaining that permit until actual I/O finishes even if the awaiting
view cancels. Wayland captures an exact offer and starts its pipe request on the
GUI thread, then reads bounded UTF-8 off-thread. X11 uses its existing clipboard
connection machinery off-thread, with bounded normal/INCR properties and an
absolute selection-event deadline. Selection waits use one second; X11 server
connection/reply waits still follow x11rb's transport behavior. The new path
preserves text bytes, including CRLF, and does not request images. Legacy full
clipboard reads retain their existing API.

`App::read_clipboard_uri_list_async` transfers bounded local file metadata through
the same Linux external-read permit and deadline as text. Only explicit file
entries or Wayland `text/uri-list` offers are accepted. Shared helpers validate
at most 256 absolute local paths, 32 KiB per native path and 1 MiB encoded data,
without filesystem access. They reject remote hosts, parent segments, malformed
escapes and ambiguous URIs, preserving non-UTF-8 Unix filenames. Wayland source
MIME types distinguish text from files; file bytes are serialized as URI records
instead of flattening filenames into text. Nonblocking offer metadata queries
let hosts advertise files only while a real file-list offer exists. Native
macOS, Windows and X11 bounded file transfers remain unavailable; test contexts
support explicit file entries for isolated acceptance.

Wayland clipboard output uses the existing Smol runtime outside the GUI thread,
with four retained task handles, a 1 MiB byte ceiling and a one-second absolute
deadline shared by ordinary/primary clipboard output and drag-source sends.
Dropping the clipboard cancels its tasks. Only pipe/socket descriptors are
accepted. This replaces the previous GUI event callback, which could block on
a slow receiver and retain stalled transfers indefinitely. Retained external
MIME metadata is limited to 32 unique ASCII names of at most 128 bytes each.

Two power-test lint expectations are removed because this extracted workspace
does not carry the upstream disallowed-method configuration. The tests retain
their real D-Bus process cleanup and wall-clock timeout.

`TestAppContext::simulate_window_hover_change` delivers the test platform's
existing window hover callback. Pointer motion alone does not emulate a native
window-enter event on Linux/Windows; hosted cursor tests now establish that
state explicitly without changing production cursor policy.

`CursorStyle::Hidden` is a persistent, hitbox-scoped invisible cursor, distinct
from typing-hide-until-motion policy. Native Wayland uses `set_cursor(None)` even
when cursor-shape is available, including re-entry and transient-hide restoration.
X11 reuses its cached invisible cursor; AppKit installs a retained transparent
cursor in the ordinary cursor rect; Windows uses its null cursor and Web uses
CSS `none`. `Hitbox::is_hovered_ignoring_last_input` exposes the current-frame,
occlusion-aware hit test for software cursor painting through keyboard modality.
The regression covers movement, keyboard input, occlusion and leaving the hitbox.

Cursor styles also cover the complete version-two Wayland cursor-shape set:
help, progress/wait, cell, move/no-drop, individual diagonal resize directions,
all-scroll, zoom, drag-ask and all-resize. The Wayland client negotiates up to
version two and substitutes context-menu/move for its two newer shapes on a
version-one host. Linux theme lookup and Web CSS cover the added names; Web uses
the correct NWSE/NESW diagonal axes. Windows uses its native equivalents where
available. AppKit uses public cursors where available, checks zoom-selector
availability and otherwise uses the ordinary arrow for unsupported styles.

Polychrome image sampling clamps UVs to texel centers within the selected atlas
tile in WGPU, Metal and HLSL. Linear filtering previously sampled neighboring
assets when scaling an image; uploading a yellow cursor made a yellow fringe
visible on the application's edge. A real WGPU regression checks scaled image
edges beside another tile and fails against the previous shader. Empty tile
extents produce transparent output. Sprite layouts and atlas allocation sizes
are unchanged.

`RenderImage::new_premultiplied` and `is_premultiplied` retain compositor/GPU
pixels without unpremultiplying each texel on the CPU. Polychrome sprites carry
that mode in the previously unused padding word, preserving the record size and
all later offsets. WGPU, Metal and HLSL unpremultiply the filtered sample before
the destination's existing alpha blend, preserving correct transparent-edge
interpolation. Straight-alpha images keep the ordinary constructor. The WGPU
regression checks straight/premultiplied equivalence and an alpha ramp.

The `bench_metrics` page-fault test uses a fresh anonymous mapping rather than
assuming a reused heap allocation causes over 100 page faults. Its test-only
`memmap2` dependency uses the workspace's already resolved 0.9.12 version; the
lockfile records that dependency edge. The check still requires positive faults
and retains its context-switch assertion. Full workspace test runs use
`RUST_TEST_THREADS=1` because frame timing collection is process-global, while
foreground benchmark journals are thread-local. The environment variable also
works with the workspace's Criterion benchmark harnesses.

The public GUI facade also names AccessKit Node/NodeId and scalar text-selection
types for bounded synthetic text subtrees in the SDK. TestWindow retains the
ordinary accessibility callbacks, and TestAppContext can simulate activation,
deactivation and node actions through them. Callbacks run outside the TestWindow
state lock to permit reentry. Production adapters and wire profiles are unchanged.

Apple `PaintSurface` optionally retains a `SurfaceLease`, and
`Window::paint_surface_with_lease` carries it into the scene. The Metal renderer
accepts premultiplied packed BGRA/RGBA CoreVideo buffers alongside NV12, checks
leased surface device identity, and retains buffers, mappings and producer
leases through GPU command completion. Packed surfaces have their own cache;
their cache retirement does not flush the existing NV12 cache. A separate packed
surface shader follows the renderer's existing alpha blend convention. Native
regressions cover packed premultiplied/zero-alpha pixels and the NV12 path.
