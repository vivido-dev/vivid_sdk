# Optional desktop GUI runtime

Enable `gui` to use `vivid_sdk::gui` and `vivid_sdk::gui_platform`. The framework
is migrated from the local Zed checkout into an independent workspace in `gui/`;
[UPSTREAM.md](../gui/UPSTREAM.md) records its revision and extraction scope.
The source is copied, not a dependency on the developer's Zed checkout.

`gui-runtime-shaders` enables Apple's runtime Metal shader compilation when the
Xcode Metal command-line tool is unavailable. `gui-testing` enables deterministic
GPUI test contexts. Existing protocol-only builds do not compile GUI dependencies.

The separate optional `gpu-share` feature provides an experimental macOS
WGPU-to-IOSurface frame bridge. With `gui`, immutable frame leases can paint
directly into SDK views and remain retained through GPU completion. See
[GPU sharing](GUI-GPU-SHARING.md) for its bounds and prototype limitations.

`gui-offscreen` enables production `Window::render_to_image` and standalone GPU
renderers independently of `gui-testing`. A frame is limited to 64 MiB of packed
RGBA; invalid or overflowing geometry is rejected before readback allocation.
Metal readback checks completion with a five-second deadline. Readback blocks
its caller, so continuous remote applications must run it in their own producer
runtime rather than the compositor's event loop.

Linux `gui_platform::headless()` windows use a real WGPU atlas and renderer when
this feature is enabled; opening fails if GPU initialization fails. They need no
Wayland/X11 server, but require a working GPU adapter or software Vulkan driver.
There are no native frame or resize callbacks: drive `Window::draw` explicitly,
clear its returned arena token after capture, and call `Window::bounds_changed`
after a programmatic resize. macOS and Windows read back their native renderer;
a hidden macOS window still uses AppKit and native system services. Windowed
Linux capture and a native-window-free macOS backend are not provided here.

The Rust-only [offscreen round trip](../examples/rust/gui_offscreen_roundtrip.rs)
uses the production dispatcher, timers, native text shaping, and GPU rendering.
It checks distinct frames, text pixels, resize, and exact raster delivery through
an in-memory-authenticated loopback presenter. Network work stays on a separate
worker, and the finite example queues at most two frames. For example:

```sh
cargo run --example sdk_gui_offscreen_roundtrip \
  --features gui-offscreen,gui-runtime-shaders,presenter -- /tmp/sdk-frame.png
```

The output PNG is optional. This verifies the frame path. The independent app
backend below builds on it, including in-memory lease recovery. Production
vector-overlay composition remains separate work.

## Embedded application methods

`gui_host::HostedApp` can advertise a static list of `automation::Method` values
and implement `automate` alongside its view and lifecycle callbacks. The default
advertises no methods. Applications define and validate their own argument and
result schemas; the SDK provides bounded objects and owner-scoped completion.
This is a trusted host API, separate from Vivid wire input and session authority.
It does not expose a process endpoint or grant remote applications control.

The host calls `AppHost::automation_methods(id, cx)` for discovery and
`AppHost::automate(id, request, cx)` to obtain an asynchronous `automation::Reply`.
Use the complete `InstanceId`, including its random host identity. Queries may
inspect a hidden running app. Commands require the active visible owner and are
revoked on deactivation or hide. Close, replacement and host destruction cancel
both kinds before app teardown. A foreign or retired ID cannot select another
instance that reused a local serial. Calling a method never activates a pane.

The app receives an opaque `Responder`. Call `response.run(|| ...)` for short
GUI work, or move the request and responder into a bounded native worker queue
and call `run` there. Decode with `request.data().decode::<YourSchema>()` and
construct the result with `Data::new(serde_json::json!({...}))`. A dropped
responder reports unavailable; dropping the reply cancels queued work. Do not
perform blocking work on the GUI thread or start work outside `run`.

Requests and responses must be JSON objects, at most 32 KiB encoded, with the
SDK's configuration depth and node bounds. Metadata allows 32 unique methods,
64-byte registration names and 256-byte descriptions. Admission retains at most
four calls per owner and 64 per host. Both the reply and any queued responder
hold the reservation: caller cancellation, owner replacement and completed but
unconsumed replies cannot bypass these limits. Retiring workers continue to
count against host capacity until they actually release their work.

Calls expire two seconds after admission. `Responder::run` checks cancellation,
owner state and expiry before entering app work and before publishing a result.
Work that passed admission can already have changed state when cancellation or
expiry wins. A failed or lost command response is therefore an uncertain
outcome; never automatically retry commands. These receipts do not acknowledge
rendering, physical display, PTY delivery or Vivid presentation.

`Data`, `Request`, `Responder` and `Reply` redact application content in `Debug`;
schema errors use content-free stable `automation::Error` codes. Hosts remain
responsible for authenticating their endpoint and protecting successful result
data. The compositor's [local control contract](../../vivid_compositor/docs/CONTROL.md)
shows one embedding of this API. Independent-process and Wayland adapters do
not acquire app methods automatically.

## Independent application processes

The independent-process overlay path can use the optional
[shared vector renderer](VECTOR-RENDERER.md), also used by Vivido. Its compiler
and GPU cache are independent of the GPUI event loop. Enabling them does not yet
connect vector windows or host text measurements to this remote GUI adapter.
The SDK's separate `presenter::NativeTextHost` can now supply real text services
to an explicitly overlay-enabled terminating presenter; that attachment does
not itself connect the raster-based independent-process adapter to vector windows.

`gui-remote` includes production offscreen rendering and exposes
`gui_remote::run(config, build)`. Call it on the native main thread with a
`ProducerConfig` and the same root-view constructor used by a standalone app.
The constructor establishes initial focus. The app owns its native runtime;
control/input and media each use an independent worker. There is no test
dispatcher in this producer path, and no protocol data enters a terminal PTY.

[gui_remote_counter.rs](../examples/rust/gui_remote_counter.rs) demonstrates one
root in both modes. Build it with:

```sh
cargo build --example sdk_gui_remote_counter --features gui-remote,gui-runtime-shaders
```

Run the example without arguments for a native standalone window. To host it,
configure the compositor's `sdk-app` command with the absolute executable path
and `--remote`. The app reads `gui_remote::config_from_env()`; the host supplies
its private discovery endpoint and either root or lease activation authority
through the child environment. Do not copy capabilities into launch
arguments, configuration, or logs.
The example updates through next-frame callbacks and supports Ctrl+Q for a
graceful exit in either mode.

This correctness backend supports one desktop canvas per process, logical
dimensions from 1 through 1024, and physical RGBA frames up to 2048 per axis.
Geometry changes revoke input before resize and replace immutable raster tracks;
the old track retires after the new output becomes ready. The host withholds
input until presented dimensions and the producer's mapping generation acknowledge
its current target, even after an equal-size resize. Frames use the negotiated
encoded-byte/rate contract, capped at 30 Hz. Mailboxes retain only the
latest frame; media flow credit never blocks the control/input worker. Idle
windows avoid GPU readback using `Window::needs_draw` and `render_revision`.
The producer advances `Window::advance_offscreen_frame` once per turn so
`on_next_frame` updates and animations progress without a native display link.
Callbacks queued during a callback wait for the following turn. This local
frame clock does not acknowledge presentation on the remote host.

The final GUI injection boundary checks the exact armed binding tuple, classes,
and receipt-time watchdog while serialized with revocation. It bounds queued
input to 64 events. Expiry, focus/geometry invalidation, or connection loss clears
queued events and releases held keys/buttons. `Window::dispatch_cancelled_input`
delivers release listeners while suppressing GPUI's default activation clicks.
Custom release handlers can inspect `Window::is_input_cancelled` before
committing a release-triggered action.
Basic US physical keys, modifiers, pointer buttons/motion, and scroll are carried.
Native repeats are suppressed. The producer applies bounded local typematic:
500 ms initial delay and 50 ms interval by default, with at most one repeat per
GUI turn and no catch-up burst after a stall. Repeats pass through the ordinary
key-binding and text-insertion path with `KeyDownEvent::is_held = true`.
They require the still-live original binding and keyboard class at dispatch;
revocation, watchdog expiry, geometry change and disconnect release held input
and clear the timer. Each producer owns its timer even when peers reuse local IDs.

`gui_remote::KeyRepeat::new(delay_ms, interval_ms)` accepts 100–5,000 ms delay and
33–1,000 ms interval, or two zeros to disable repeat. A local launcher calls
`KeyRepeat::configure(&mut command)` to supply `VIVID_GUI_KEY_REPEAT` only to its
child. `run` and `run_persistent` validate this strict bounded JSON envelope before
starting GUI or transport workers. This is a local launch policy, not wire layout
negotiation; remote GUI pacing can make observed intervals longer than requested.
The local-process text clipboard bridge is described below.
Negotiated layouts, Caps Lock state,
IME, clipboard images/files, popups, accessibility, and multiple windows are not implemented
by this initial remote backend. Linked/native applications retain their native
platform services.

### Local-process text clipboard

`gui_remote::clipboard::Host::bind` starts a private same-owner native service
for one child. Its launch environment is independent of Vivid authentication;
this is a local hosting service, not a new Vivid profile. The compositor installs
the child's exact actually-presented input binding and services `next_request`
on the GUI thread through ordinary `gui_host::clipboard` leases. A new binding,
focus/visibility loss, resize, reconnect or process replacement revokes queued
and in-flight native work. Admission is serialized with revocation, including
focus return to an identical numeric binding. Peer processes have independent
channels, leases and resource budgets.

`run` and `run_persistent` install an explicit `gui::HostedClipboard` provider
before constructing the root view. Use `App::read_from_clipboard_async` or
`read_clipboard_text_async` for paste and `write_to_clipboard` for copy. Clipboard
text is limited to 1 MiB, with four queued/in-flight producer requests and one
native transfer per owner. Each transfer has a one-second budget. Native socket
I/O runs on dedicated workers; GUI, rendering, media and Vivid input never wait
for it. Producer delivery checks the original binding's watchdog and control
lifecycle. Dropped/expired replies cannot acquire a newer focus grant.

Synchronous clipboard reads return None on this backend. Primary, Find, image
and file-list APIs have no native fallback. With no private service endpoint,
clipboard access is unavailable; partial credentials fail startup. The channel
checks a random launch credential in constant time, strict version/sequence
framing, native peer ownership and private Unix directories. Native service
failure disables this process's clipboard until Restart without stopping media
or a peer. A write API queues work and does not acknowledge native completion;
never automatically retry an uncertain copy. This is process fault isolation,
not an OS sandbox against a producer calling native APIs itself.

The remote counter examples accept Ctrl/Cmd+C to copy their count and Ctrl/Cmd+V
to restore a bounded decimal count. macOS automated acceptance covers real
producer processes and IPC with the host's test clipboard. Physical keyboard
shortcuts and interoperability with other native applications remain separate acceptance gates.

macOS bounded text reads now request only UTF-8 text and check the native data
length before allocating a Rust copy. They ignore file, image and metadata
formats. Pasteboard I/O runs on the background executor with one actual read per
platform, including a read whose awaiting view cancels. A one-second deadline
rejects late results; an in-progress AppKit/provider call cannot be interrupted
and retains its permit until it returns. Native provider allocations are outside
the Rust byte budget. Unique native pasteboard tests cover Unicode, exact limits,
invalid UTF-8 and ignored images without modifying the ordinary user clipboard.

The compositor currently launches each app with a dedicated authenticated
presenter. A stalled or crashed app does not share the shell's event loop or
another app's transport. This is a process crash boundary, not an OS sandbox.
Root-authenticated producers reconnect after transient transport loss while
retaining the same GUI process and root view. Discovery and root authority are
pinned once, then every reconnect performs fresh authentication and recreates
surfaces, tracks, and input lanes. Connection generations distinguish identical
target dimensions and reused local IDs; old frames and input are discarded.
Held input also checks the control lifecycle at the injection boundary.

New attempt admission is bounded to 30 seconds per outage, with exponential
backoff from 100 milliseconds to two seconds. Native establishment retains its
own I/O deadline. Explicit authority/protocol errors terminate the producer;
nonfatal capacity/rate errors may retry within the same budget. Neither retry
delays nor blocked media prevent the GUI from advancing its local callbacks.
The host shows a reconnecting placeholder and bounds recovery supervision.

Root sessions create a new logical session under the same principal. With the
compositor's `options.resumable = true`, startup uses a private delegated lease
with ten seconds of disconnect grace. A complete set of
`VIVID_GUI_LEASE_CONTEXT`, `VIVID_GUI_LEASE_ID` and `VIVID_GUI_LEASE_SECRET`
selects activation in `config_from_env`; partial or malformed values fail closed.
Custom launchers may supply explicit lease activation through `ProducerConfig`.
The presenter denies leased desktop input by default. A host must explicitly
permit classes with `VirtualVivid::set_lease_desktop_input_classes`, and all
ordinary profile, surface, focus, presentation and binding checks still apply.

An activated producer retains a bounded operation graph: one canvas, one fixed
scene node and at most two raster tracks during resize. Resume queries an
owner-bound snapshot and adopts retained objects before opening fresh channels.
An uncertain create uses its exact original idempotency key and body to recover
the receipt; unknown objects or inconsistent counts fail closed. Lost tracks
are explicitly destroyed and replaced, preserving the logical owner and canvas.
Pending scene transactions are discarded on suspension. Input lane generations
advance with resume, producer input epochs survive revocation, and host input
tokens include the resume generation. Old frames and input cannot authorize
the resumed connection; fresh presentation and binding remain necessary.

A failed prepared activation or resume attempt retries its exact handshake until
confirmation. A confirmed activation is never reused, and lease failure never
falls back to root authority. Expired/revoked leases remain fatal. Starting a new
process with only resume authority is unsupported: the operation graph and GUI
state live in memory. Optional [durable GUI state](GUI-STATE.md) restores app data
into a fresh process/session through `run_persistent`. Explicit Restart uses the
slot's launch restoration state; a manual checkpoint captures newer data for a
later session restore. CPU-copy performance acceptance remains pending.

The terminating presenter now serves bounded core reconciliation through
`Session::query_session_page`. Pages use the shared protocol codec, include
context/surface/track/active-slot summaries, and bind continuation to one owner
and revision. See [session reconciliation](SESSION-STATUS.md). GUI resume bounds
its complete snapshot to eight summaries and eight pages, and rechecks it after
object adoption before sending media.

## Nested Wayland applications on Linux

`gui-wayland` exposes `gui_wayland::WaylandHost` on Linux, using Smithay 0.7 and
Pixman. The [vendored dependency](../vendor/smithay/UPSTREAM.md) includes required
surface-transaction fixes and records their provenance. It creates a private
display for one xdg toplevel, its subsurfaces and bounded xdg popups. The caller owns its
dispatch loop and child supervision, supplies the child's absolute `WAYLAND_DISPLAY`
and private `XDG_RUNTIME_DIR`, and keeps dispatching while a view is hidden. No
Vivid authentication or protocol authority crosses this transport. The server
does not modify the parent's environment or require a DRM device.

The display advertises `wp_viewporter` and `wp_fractional_scale_manager_v1`.
`WaylandHost::resize_with_scale(width, height, scale_120)` changes logical geometry
and the virtual output's scale atomically. Scale uses 120 units per one, bounded
from one through four. Logical output dimensions reserve room for integer-only
clients' rounded-up scale inside the 2048-pixel buffer ceiling. Invalid requests
leave the current presentation/input intact. A valid scale change requires fresh
configure and host presentation even when logical dimensions do not change.
Preferences apply to existing and newly created surfaces; no physical outputs
or seat devices are managed by this backend.

All eight core buffer transforms are supported for root, child, popup and cursor
SHM surfaces. Quarter turns swap physical canvas dimensions; logical pointer
coordinates and cursor hotspots remain in surface space. Transform-only commits
reuse the retained raw poster and require fresh presentation before root input
can resume. Synchronized children retain ordinary parent-commit semantics.

Viewport source rectangles use checked 24.8 coordinates after buffer transform
and integer scaling. The source must fit the exact transformed extent, including fractional
extents such as a 65-pixel buffer at scale two. Destination dimensions stay
bounded to 2048 logical pixels. Cropping, destination scaling, metadata-only
changes and viewport destruction follow ordinary double-buffered surface and
synchronized-child commits. `Frame::logical_size` defines the painting aspect
and input coordinates; its physical poster dimensions can have another aspect.
The compositor paints and hit-tests the same logical viewport and withholds
presentation when the application image did not paint successfully.

SHM readback retains an owned RGBA poster per surface. A single applied commit
with a full-buffer source and matching dimensions, transform, integer scale, viewport,
stride, offset and format patches the clipped bounding rectangle of its
surface/buffer damage. Surface damage maps back through the viewport, scale and
transform to raw pixels;
buffer damage already names pixels. Empty visible damage reuses the
poster without reading SHM pixels. First attachments, layout changes and coalesced
synchronized commits read the complete buffer; the latter can contain damage
from different intermediate mappings. Transform/scale/viewport-only commits remap the owned poster
and revoke old geometry/input until the host presents the new frame.

Cropped attachments also read the complete raw buffer. Pixels outside a crop can
change without surface damage, then become visible through a viewport-only commit.
Capturing them before buffer release preserves that future view. Full-source
destination scaling retains partial readback; viewport-only remapping needs no
additional SHM access in either case.

Pixman still guards SHM reads and returns owned mappings. Patching uses copy on
write when an older poster is shared, preserving immutable snapshots.
`WaylandHost::readback_stats` reports saturating full/partial/reused counts and
logical RGBA bytes copied from imported SHM. These are diagnostic counters, not
memory-bus measurements: owned-poster copies, scene composition and GUI uploads
can still cover the full frame. End-to-end damage optimization and performance
comparison remain pending.

`Frame::cursor` carries a `CursorImage` under the frame's full target identity:
the host default, a named SDK cursor style, hidden, or an owned premultiplied RGBA
cursor. Smithay checks the requesting pointer's focus/client and enter serial
before accepting changes.
Cursor snapshots require an active, presented root and current pointer focus.
Cursor surfaces are limited to 256 physical pixels per axis; hotspot coordinates
and offset-adjusted hotspots are bounded to ±8192 surface-local units. Integer
scale, cursor attachment offsets and bounded subsurface trees are supported.
Cursor layers never enter the application's hit-test tree or change its geometry
identity. Initial role assignment also checks any already-imported poster.

Render the cursor snapshot with the application frame before calling `presented`.
Visible cursor-tree callbacks join the same bounded presentation batch as the
application; hidden/inactive cursor callbacks remain withheld. Old snapshots stay
immutable through pixel changes, hide/remap and surface destruction. The shell
uploads changed cursor revisions separately, clips software cursors to the
client viewport and qualifies them with the current target and admission epoch.
Its platform pointer uses scoped `gui::CursorStyle::Hidden`, which persists
through motion. Cursor and application textures use `RenderImage::new_premultiplied`
so alpha is applied once and interpolation preserves translucent edges.
`wp_cursor_shape_manager_v1` version two accepts all 36 named shapes, including
version-one clients. Invalid/too-new shapes fail only their connection; stale or
future pointer-enter serials are ignored even during an implicit grab. Named
and bitmap requests replace each other, while destroying a shape device retains
its requested shape. Named styles release bitmap assets and use the same scoped
hitbox and epoch/target guards. Hardware cursor planes are not advertised.
Physical pointer acceptance remains pending.

Dispatch requires the Rust Wayland server backend. Construction rejects a
feature-unified `wayland-backend/server_system` before creating display resources;
the system backend cannot dispatch one connection independently. A system-backed
Wayland client remains supported. Ready connections are dispatched individually,
with the mapped root first and helpers in rotating order. An owned watchdog
shuts down the exact connection socket after a 250 ms continuous drain, including
core requests that never invoke application callbacks. A helper timeout retires
that helper; an existing root's timeout revokes input and returns `TimedOut` for
the caller to fail/reap its application. Dispatch yields after the current drain
when a turn has consumed the budget, rather than spending a full budget on every
ready helper. Idle displays do not wake the watchdog periodically.

`dispatch_control()` returns a cloneable lifetime-specific `DispatchControl`.
`cancel()` rejects new input and interrupts the currently draining connection
without acquiring the backend dispatch mutex. Already admitted operations may
finish; the embedding owner must also revoke its platform services and drop the
host. Subsequent dispatch returns `Interrupted`. An old handle cannot cancel a
replacement display. Dropping the host closes retained socket duplicates and
joins its watchdog. Host-generated disconnects run backend resource cleanup even
when the client is idle. These are liveness measures, not hard real-time bounds:
they cannot preempt renderer/application callbacks or destructor work and do not
establish complete protocol-object, descriptor, memory or request-rate quotas.

`dispatch` returns owned packed RGBA snapshots. `presented` acknowledges a delivery
only after the embedding host rendered it; that releases its exact frame callbacks.
Only a current, presented, active `Target` admits keyboard or pointer input. The
opaque target includes a random display identity and checked root/geometry serials.
Resize, root replacement, and buffer mapping changes invalidate old targets.
Focus revocation releases held transitions and cancels pointer grabs. Captured
pointers move outside the canvas before release so ordinary widgets cancel their
click. A previous delivery rendered during resize may finish its callbacks while
remaining ineligible for input. Dropping that callback can deadlock clients waiting
to draw a new size. A client may also acknowledge configure and commit its unchanged
buffer. The server repaints owned pixels with a fresh target after that commit and
still requires new presentation before restoring input.

This backend supports required ARGB/XRGB SHM formats, integer buffer scale,
all eight buffer transforms with zero attachment offset, a virtual output, physical
evdev keys, pointer buttons, motion and scrolling. Buffers are capped at 2048 pixels
per axis. SHM row/offset/
length arithmetic is checked before the C-backed importer; pool reads are capped
at 64 MiB. Smithay handles concurrent pool truncation. This crate creates no Rust
reference into mutable SHM and retains its `forbid(unsafe_code)` policy. The frame
mailbox, in-flight presentation, and callback batches are bounded. Private socket
permissions are 0600 inside a 0700 directory.

SHM admission runs before mapping or growth. Each display shares a 128 MiB mapped
storage ceiling across all its client/helper connections, with 64 MiB per pool,
64 backing pools and 256 retained buffer descriptors. Pool charges remain live
after `wl_shm_pool.destroy` while buffers still reference that storage. Growth is
reserved before remapping, failed work rolls back its reservation, and bounded
pools unmap/close on their owning worker before returning capacity. They do not
enter Smithay's global asynchronous destruction queue. These limits concern SHM
storage and metadata; retained decoded/composed pixels have separate budgets.

Quota rejection posts the core no-memory error and disconnects only that native
connection. `root_failed()` retains an observed protocol failure of the root's
actual client lifetime across role teardown/replacement. A helper-only rejection
does not mark the root failed. The compositor uses this signal before treating
a successful child exit as clean, so a rejected root enters its failure/restart
path even if the process exits with status zero.

Each connection also admits at most 2,048 retained protocol-ID table slots,
including its display object and both client-created and server-created IDs.
Destroyed slots remain charged until reused; repeatedly creating and destroying
ever-higher callback IDs cannot grow the table indefinitely. Core sync/registry
requests, registry binds, typed constructors and selection offers share this
admission check. The display admits at most eight connections, hence at most
16,384 logical slots. Vector capacity and object payloads are separate overhead;
this is not an exact process-memory ceiling. Exhausted internal connection/object
generations fail instead of wrapping into an old owner's identity.

The private native stack is bundled under `vendor/wayland`, with archive hashes,
licenses, and a reproducible patch. Path dependencies carry the modified backend
into consumers without requiring their workspace to supply Cargo patches. This
stack is distinct from the GUI platform's registry-based Wayland client stack;
the host boundary uses SDK-owned pixels, input and platform services.

Request admission also covers core sync/registry traffic before payload parsing.
Each connection has independent request-count and declared wire-byte buckets;
it must also fit a shared role budget:

| Allowance | Request burst / refill per second | Wire-byte burst / refill per second |
| --- | --- | --- |
| One connection | 32,768 / 16,384 | 4 MiB / 2 MiB |
| Protected root group | 32,768 / 16,384 | 4 MiB / 2 MiB |
| All helpers together | 65,536 / 32,768 | 8 MiB / 4 MiB |

Shared allowances survive reconnects. Accepting the root's toplevel selects its
protected group; retiring that root role/surface demotes the connection to the
helper group. Helpers cannot spend the root's shared allowance. Group switches
do not reset either bucket. Idle time refills only up to the burst ceiling, and
fractional refill credit is retained. A fragmented header/body or descriptor wait
charges the request once. Wire bytes refer to the declared request size, not SHM
pixels or clipboard bytes sent through separate descriptors.

Exhaustion disconnects only the requesting connection through core no-memory.
The 250 ms drain watchdog and cancellation controls remain independent guards;
a continuously flooding client can hit rate admission first. These are protocol
traffic limits, not preemption or a bound on the cost of one renderer callback.

Each connection also bounds its incoming and outgoing userspace descriptor queues
to 64 entries independently. Incoming descriptors are admitted across receive
calls before queue growth; outgoing descriptors are counted before duplication,
with a flush/recheck when capacity is needed. Consuming a request or flushing an
event returns queue capacity. Eight connections bound a display to 512 entries
in each direction. Descriptors already owned by SHM/clipboard services have their
separate budgets; these counts do not cover child-owned or kernel-queued resources.

Receives enforce a logical batch limit of 28, including alignment-padding space
that could physically fit more, and reject truncated ancillary data. Rejection
drops the temporary batch and queued transport descriptors before attempting the
core no-memory error. A blocked recipient might not receive that error; the exact
client's protocol-failure record still drives root failure/restart independently.

The subcompositor global supports synchronized/desynchronized child commits,
parent-owned positions and stacking, nested children, premultiplied alpha, and
integer scales that differ between surfaces. Composition clips to the root's
canvas. A new or replacement role becomes visible only with a parent commit.
Grandchild updates wait for their direct parent's commit; switching to effective
desynchronization releases only already committed caches, including descendants.
Reverse stacking and committed input regions select the exact native
surface and local coordinates. Tree membership, positions, stacking, dimensions,
input regions and role replacement revoke prior input targets; pixel-only updates
retain their mapping. A buffer-scale change requires fresh presentation even if
logical dimensions stay equal. Only callbacks for surfaces intersecting the delivered
canvas join that delivery's presentation batch. Completely clipped children
receive no manufactured clock.

Each display admits at most 32 surfaces, eight nested child levels, offsets within
8192 logical pixels, 64 rectangles per input region, and 64 MiB of retained decoded
surface pixels in addition to its composed canvas. Replacement pixel charges are
checked before import/copy. Committed callback queues are bounded to 64 per surface
and 256 across the scene, with one additional in-flight presentation batch of at
most 256. Parent stacking and position batches are bounded to 256 operations each.

Frontend admission also runs before a surface commit or region assignment:

| Retained frontend resource | Ceiling |
| --- | --- |
| Add/subtract operations in one `wl_region` | 64 total |
| Damage rectangles across one surface's pending/cached/current state | 256 total |
| Frame callbacks across that frontend state | 64 total |
| Queued commit snapshots per surface and cached state type | 64 |

Both damage coordinate spaces share their ceiling. Commit/merge moves damage and
callbacks between states rather than resetting their charge. Applying snapshots
returns snapshot capacity; input/opaque region copies remain bounded by the region
and snapshot ceilings. These frontend counts are separate from callbacks already
transferred to the SDK scene or an in-flight delivery. Limits are enabled through
the vendored `CompositorState::with_limits`; the SDK always uses this constructor.
Quota errors use the same exact-client no-memory/failure path as SHM admission.
Unrelated, unrendered helper-surface cleanup cannot revoke the root's input target.

These bounds do not yet provide complete quotas for other protocol payloads,
kernel-queued descriptors or the cost of individual callbacks. They do
not constrain storage the child allocates outside the server's SHM mappings.

The compositor's `wayland-app` factory shares its eight-runtime budget with SDK
processes and Vivido. CPU copies run on a per-display worker. Client callbacks
follow host presentation, so hidden apps do not receive manufactured frame clocks.
Client pixels retain their aspect ratio; input maps to the painted image, excluding
letterbox space. Geometry and focus epochs qualify queued input.

Popup placement uses Smithay's anchor/gravity and flip/slide/resize constraints
within the root canvas. Parent and popup window geometry account for buffer
shadows and mapped subsurfaces. Nested popups and their subsurface trees share
the same alpha, clipping, hit-testing and callback path. Reposition tokens retain
their individual configure responses; ACK alone does not move pixels. Reactive
positioners follow client canvas changes. Popup pixels remain clipped to the
pane's root canvas.

Nongrabbing popups keep keyboard focus on the root. An explicit grab requires the
display's seat and a recent, actually delivered key/button serial from the same
live client and root generation; at most 16 proofs survive for five seconds.
Nested grabs must extend the current grabbing parent. Keyboard focus follows the
topmost visible grab only after its frame is host-presented. Host focus loss,
resize, root retirement, and explicit `Input::DismissPopups` dismiss popups and
revoke input immediately, including a fast focus return before the next dispatch.
The compositor sends that dismissal for a click in letterbox space. Outside
presses cancel popup grabs without activating the client. Descendants receive
`popup_done` before parents. Unmapping requires fresh configure/ACK negotiation;
old pending configures and acknowledgements cannot remap pixels.

The display retains at most eight registered popup roles, including dismissed
roles awaiting destruction, and at most 16 queued reposition requests and 16
outstanding configure responses per role. Positioner inputs are bounded before
constraint arithmetic. The vendored frontend also checks initial positioner
geometry before calling SDK handlers, so overflowing input cannot panic first.

`WaylandHost::with_clipboard` optionally advertises ordinary clipboard and primary
selection globals backed by a `gui_host::clipboard::Client`. The platform host
must activate/service that channel. `WaylandHost::new` still creates no clipboard
globals. Native selections require a recent delivered key/button or keyboard-enter
serial from the current focused client and presented root. Focus, mapping and
selection revisions scope offers and jobs independently, including fast focus
return. Old offers, selection serials and queued platform writes cannot regain
authority. Clearing native focus clears selection authority before publishing
offers for a new owner.

Transfers accept UTF-8 text and optional local file URI metadata, at most 1 MiB each, four concurrent jobs per
display, and 16 KiB of work per job per dispatch. Native I/O is nonblocking with
a one-second absolute deadline; stalled, malformed and oversized sources do not
block input or update the platform clipboard. Output descriptors must be pipes
or sockets. Source MIME metadata admits at most 32 ASCII names of 128 bytes each.
The SDK re-originates completed content through the platform service; it does not
forward another owner's native offers. DnD is refused before installing a grab,
and data-control globals are absent.

A provider created with `gui_host::clipboard::Host::with_file_lists` can bridge
`text/uri-list` on the ordinary clipboard. Primary selection remains text-only.
It refreshes `set_file_list_available` from native offer metadata, so a text
clipboard never advertises files just because its backend supports them. File
transfers share text's jobs, byte limits, deadlines and revocation. Invalid or
remote URI lists leave the platform clipboard unchanged. The service handles
path metadata only; it never opens files or grants filesystem permissions.
Changing file MIME availability retires the ordinary selection scope, so a
withdrawn offer cannot regain permission when files later return. Primary
selection and peer owners retain their independent scopes.
Devices bound after the client gains focus receive the current ordinary/primary
offer immediately; creating a device does not require a focus round trip or
re-offer another client's selection. Native clipboard consumers still wait for
their asynchronous selection event before issuing paste requests.
Startup activation requires a nonempty matching presentation receipt before
keyboard or clipboard focus is granted. Two absent targets are not a valid
match. The regression defers the first client buffer behind a local test gate,
activates the host, proves no keyboard enter arrived, then verifies clipboard
transfer only after actual presentation and preserves a peer owner.

The display also advertises text-input-v3 version 1. `text_input()` returns the
current presented editor's bounded surrounding text, selection, clipped candidate
rectangle and platform assistance preferences. `text_edit()` submits an atomic
preedit/commit/deletion transaction through an opaque `TextTarget`. Enable,
disable, content/cause changes and focus follow committed native state. Editor
identity is distinct from surrounding-text revision: selection-relative queued
edits can continue across their own replies, while absolute replacements require
the exact context revision and must include the complete current selection.

Only the ordinary text-input global is exposed; the embedding GUI supplies the
input method. The SDK uses the existing typed Wayland dispatcher because
Smithay's generic text-input helper requires an external privileged input-method
client. No input-method service, virtual keyboard or Vivid input authority is
advertised. The backend bounds instances to 16 per display, strings to 4000 UTF-8
bytes and candidate inputs to 8192 logical units before checked clipping. Invalid
byte boundaries, enum values and geometry disconnect only the offending native
client. Destroyed objects release capacity without reusing editor authority.
Native done counters use the protocol's per-object modulo-u32 commit count.

Focus, root mapping, editor replacement and external text changes invalidate old
targets. External changes cancel an active preedit rather than leaving a preview
in the client. The shell converts SDK UTF-8 coordinates to platform UTF-16,
registers an immutable per-editor/focus input handler and uses the existing bounded
input queue/admission epoch. Old handlers cannot acquire a new editor after fast
focus return. Candidate geometry maps through the actual painted native viewport,
including letterbox offsets. Sensitive editors expose only their current preedit
to platform text queries; assistance is suppressed. Physical input-method and
secure-input behavior still require native platform acceptance. V1/v2 text-input
and text-input-v3 version 2 behavior are not advertised.

Additional toplevels, image clipboard transfers, drag-and-drop,
DMA-BUF, end-to-end damage
optimization, full protocol-object/resource quotas, physical desktop acceptance, and a direct
seat/display backend remain later milestones. DMA-BUF globals are absent;
extra toplevels receive close requests.

## Trusted linked applications

`gui_host` provides bounded trusted application hosting with factories, root
views, qualified instance identities, activation, and explicit shutdown.
It provides no process isolation and does not alter Vivid protocol authority.
The first consumer is `../vivid_compositor`; its demo and standalone counter use
the same application view implementation.

`AppDescriptor` supplies an owned registration ID and display name. `AppRegistry`
owns factories, validates metadata, rejects duplicate IDs atomically, and has an
explicit registration capacity. IDs contain at most 128 ASCII bytes, starting
with a lowercase letter and continuing with lowercase letters, digits, dots,
underscores, or hyphens. Display names have a 128-byte ceiling; unavailable-app
explanations have a 512-byte ceiling. Metadata rejects control characters.
Unavailable registrations remain discoverable while launch attempts return
`Unsupported`; unknown applications return `NotFound`.

Factories now implement `descriptor`, `validate`, and `create(config, window, cx)`.
`AppConfig` holds an object with at most 256 KiB of encoded JSON, 16 nested levels,
and 1,024 value nodes. It implements Serde for host configuration formats and
`decode` for application-owned schemas. Bound serialized source input before
generic Serde decoding; the compositor limits each TOML file to 1 MiB. Values
remain absent from SDK decode errors and Debug output. Validators should reject
unknown fields, keep diagnostics free of configuration values, and perform no
runtime/platform initialization. Validation may run more than once.

`AppHost::mount` accepts an explicit `AppConfig` and checks host capacity and
identity exhaustion before factory code. `AppRegistry::mount` adds ID lookup to
that path. Registration and launch validation never call `create`. Hosts retain
the registry for their shared factory resources and route close/shutdown through
the instance lifecycle. The compositor uses the same path for standalone windows.
This registration contract supports linked Rust factories. A process adapter can
implement it, as the compositor's `sdk-app` registration does. Dynamic library
loading is not provided.

For example, after registering a counter factory, a GUI host can launch it with:

```rust
use vivid_sdk::{gui::{App, Window}, gui_host::{AppConfig, AppHost, AppRegistry}};

fn launch(registry: &AppRegistry, host: &mut AppHost,
          window: &mut Window, cx: &mut App) -> std::io::Result<()> {
    let config = AppConfig::new(serde_json::json!({ "initial_count": 10, "step": 2 }))?;
    let instance = registry.mount(host, "counter", &config, window, cx)?;
    host.focus(instance, window, cx);
    Ok(())
}
```

The compositor's [built-in registrations](../../vivid_compositor/src/apps.rs)
show factory schemas, while its optional registry rendering check registers an
additional application without changing shell dispatch.

`HostedApp::focus` restores the application's input focus independently of its
activation lifecycle. `AppHost::focus` activates a different instance first, then
always invokes that callback, including when returning from shell controls to the
already active application. Host operations select instances by their full
qualified identity.

`HostedApp::close_status` supplies nonblocking advisory `CloseStatus`: Ready,
Pending, or Confirm with RunningWork/UnsavedChanges/Unknown. The default is Ready.
`AppHost::close_status` queries only the complete owned identity and includes
hidden instances. Hosts must bound Pending checks, freeze the requested scope,
revoke input while presenting consent, and reject stale consent after an instance
is replaced. The status contains no command arguments, file contents, or secrets.
`close`/`shutdown` remain unconditional disposal primitives for confirmed user
actions, rollback, and forced cleanup; the SDK does not display dialogs itself.

Each attempt supplies a fresh nonzero request generation, reused only for its
polls. Zero is treated as unknown without calling application code. Asynchronous
apps must match readiness samples to that generation, so a recent answer from
an earlier close attempt cannot authorize a later one.

`HostedApp::visibility` lets applications retain state while presentation is
hidden. Instances start visible. `AppHost::set_visible(false)` first deactivates
the named active instance, then delivers the visibility callback; hidden
instances cannot be activated or focused through the host. Showing does not
restore activation automatically. Repeated visibility values do not repeat
callbacks, and foreign identities cannot alter visibility. Replacement preserves
the old slot's visibility. `AppHost::deactivate` revokes activation without
selecting or closing another application. Applications should stop frame
transfers and view-local input/clipboard work while hidden; background application
work may continue. This is independent of native window occlusion and process
isolation.

`AppState` holds optional application-owned restoration data with the same
object/256-KiB/depth/node limits and redacted diagnostics as `AppConfig`.
`HostedApp::save_state` captures owned data without blocking; its default returns
None. `AppHost::save_state` scopes capture to a complete owned instance, including
hidden views. Factories implement `validate_state` and `create_restored` to opt in.
The defaults reject supplied state explicitly. State validators must be pure and
content-free in diagnostics; hosts validate every saved launch before creating
startup resources.

`AppLaunch` borrows options and optional state. `AppHost` and `AppRegistry` expose
`mount_launch` and `replace_launch`; existing fresh-launch methods delegate to
them with no state. Capacity and identity exhaustion are checked before factory
work. Restoring always creates a fresh instance identity, and atomic replacement
still preserves visibility/activation on failure. Saved state must never contain
capability secrets, session keys, input/clipboard grants, or live runtime handles.
The compositor's optional manual checkpoint format also bounds its whole source
file before decoding and validates each app's state schema.

`HostedApp::status` reports `AppStatus::{Starting, Running, Exited, Failed}` and
defaults to Running for applications without a separate runtime. It must remain
nonblocking. `AppHost::status` rejects foreign identities; the embedding host is
responsible for observing status or polling at a bounded rate. Status carries no
application content or protocol authority.

`AppHost::replace` and `AppRegistry::replace` create a replacement before closing
the old instance. Validation or creation failure leaves its identity, view, and
activation intact. Success delivers Deactivated/Closing to the old active
instance, then Mounted/Activated to the new one; an inactive replacement does not
change its neighbor's activation. The replacement receives a fresh qualified
identity. This works at the mounted-instance limit, including a one-slot host;
one additional application can be under construction during the synchronous
call, so factories must continue to enforce their live/retiring resource budgets.
Hosts must replace the corresponding layout identity after success. The SDK does
not restart applications automatically or choose which statuses permit restart.

`gui_host::ResourceBudget` supplies nonblocking shared capacity for expensive
hosted resources. A successful acquisition returns a non-cloneable RAII permit;
capacity returns only when that permit drops. Workers can retain permits through
asynchronous teardown, preventing removed panes from bypassing runtime limits.
`try_acquire_many` reserves a checked number of units, enabling byte budgets as
well as resource counts. The compositor uses budgets for live/retiring terminal
workers and queued clipboard-paste bytes.

`gui_host::clipboard` bridges a worker to platform-thread clipboard services.
Each `Host`/`Client` channel belongs to one application. Only the host can activate
or close it. Text is limited to 1 MiB, capacity to four queued/in-flight requests,
and platform work to one in-flight request per owner. Worker reads wait at most
one second, checking revocation every ten milliseconds; the GUI never waits for
the worker. `Client::begin_read` returns a nonblocking `PendingRead` for dispatch
loops. `Client::scoped` adds independently revocable descendant authority, with
at most eight nested scopes. Dropping/revoking a scope cancels only its requests
and descendants; siblings and the GUI owner remain usable. Cancelled queued
requests release their capacity before fresh admission.

Platform adapters use `Lease::while_live` for bounded native operations and
starting asynchronous reads, then recheck authority after awaiting. The closure
must not wait or reenter revocation. Scope/owner revocation serializes with final
platform admission. `App::read_clipboard_text_async` supplies bounded text and
optional primary reads; Linux external I/O runs off the GUI thread with one
actual transfer per platform, including work whose view has cancelled. Focus
changes, timeout, and closure invalidate
leases, and late replies cannot restore authority. Clipboard contents are omitted
from diagnostics.

`Host::new` supports text and optional primary selection. `Host::with_file_lists`
also enables explicit `FileList` reads/writes on the ordinary clipboard; providers
dispatch `Request::format` and `Request::write_files` and keep MIME availability
current with `Host::set_file_list_available`. `Client::begin_read_files` returns
a nonblocking typed read. Both formats share all owner/scope/capacity limits.
`FileList::from_paths` and `from_uri_list` validate at most 256 absolute local paths,
32 KiB per native path and 1 MiB encoded URI bytes without filesystem access.
URI conversion rejects remote authorities, credentials, relative/parent paths,
NULs, query/fragment components and malformed escapes, and retains non-UTF-8 Unix
path bytes. Diagnostics omit names. Plain text resembling a file URI never
becomes a file list.

`App::supports_uri_list_clipboard` and `clipboard_has_file_list` distinguish
backend support from current offer metadata without external I/O.
`App::read_clipboard_uri_list_async` supports explicit test entries and native
Linux Wayland offers, with the same external-read permit and deadline as text.
Native macOS, Windows and X11 bounded file transfers remain unavailable. Image
clipboard and drag-and-drop contracts remain pending.

Linux Wayland platform clipboard output also runs off-thread: at most four
pending pipe/socket writes, 1 MiB each, with one-second absolute deadlines.
Ordinary and primary output share that capacity; dropping the platform clipboard
cancels its tasks. External offer metadata retains at most 32 unique ASCII MIME
names, each at most 128 bytes. Slow recipients cannot block the GUI thread.

Hosted text views use `gui::EntityInputHandler` with `ElementInputHandler`,
registered through `Window::handle_input` during paint. The wrapper derives
native IME-first routing from the view's `accepts_text_input` result. Terminal
views can call `with_apple_press_and_hold(false)` on the wrapper to request raw
Apple key repeats; the default preserves native accent selection for text editors.
The compositor's Vivido adapter implements this contract with bounded composition,
Unicode commits, focus cancellation, and native caret geometry.

`Window::hide_cursor_until_mouse_moves` lets a hosted view apply its own typing
policy during paint. It accepts a hitbox and sends a native hide request only
when that window is active, owns the pointer, and the hitbox is unoccluded under
the mouse. Input modality and pointer capture do not expand that region.
Platforms restore the cursor on movement/activation. Hosts with per-application
policies should select `CursorHideMode::Never` for GPUI's global automatic policy.
The compositor qualifies asynchronous requests by activation epoch and input
serial before calling this API. Shape requests use ordinary element cursor styles.

`TestAppContext::cursor_style` and `cursor_hide_requests` inspect the test platform's
requests without affecting a physical cursor. The latter is a counter, not a
simulation of OS visibility or restoration.

The migrated crates retain upstream names and license files. The extraction
script includes local build, test, target-specific, and optional dependencies.
They are maintained as source within the SDK. Release packaging and publication
of this support-crate family must be resolved before publishing GUI-enabled SDK
artifacts; the current integration is intended for repository builds.

Verification:

```sh
cargo test --lib --features gui-testing,gui-runtime-shaders gui_host
cargo clippy --all-targets --features gui-testing,gui-runtime-shaders -- -D warnings
```

Production presenter overlay composition remains separate work. The `gui`
feature alone provides a toolkit; `gui-wayland` adds the optional Linux server
described above. Neither replaces `vivid_ui`.

`gui-testing` also enables platform test support. The migrated
`gui::TestAppContext::build_with_renderer` accepts a native text system and a
renderer factory for deterministic event tests with real GPU output. The compositor
uses it for its optional rendering checks, alongside a separate native-window
snapshot path. Hidden-window snapshots do not establish on-screen acceptance.

## Readable text and hosted activation

`accessible-text` enables a dependency-free `accessible_text` model without a GUI
or native adapter. `gui` includes it. A `Builder` accepts normalized physical rows
and Unicode scalars with per-character x positions and widths, including zero-width
combining marks and newlines. It validates finite unit-square geometry, unique
local row IDs, selection boundaries and aggregate limits before accepting a row:
512 rows, 262,144 scalars, 1 MiB text and a 4 KiB label. Failed rows preserve the
builder's previous state. Document/row debug output omits terminal text and labels.

In a GUI element's `a11y_synthetic_children` callback, call
`gui_host::accessibility::populate` with a validated document and a uniquely
identified text-role parent. It adds read-only TextRun children and scalar-based
selection, mapping normalized geometry into that parent's physical bounds. IDs
derive from the full parent identity plus local row keys, so distinct applications
may reuse the same keys. No actions or transport authority accompany the document.

`HostedApp::set_activation_request` receives a GUI-thread `ActivationRequest` for
that exact mounted instance. An app can enqueue a focus request from an accessible
action; it does not move focus directly. `AppHost::take_activation_requests` drains
at most one request per owner, ordered by its latest request. The shell applies its
own modal/workspace policy before calling `focus`. Hidden owners refuse requests
and lose queued requests. Closing/replacing an owner permanently revokes its old
handle; host destruction retires the queue. Handles from another host never select
an instance with a coincident local serial.

Test contexts expose `simulate_accessibility_activation` and
`simulate_accessibility_action` through the actual window adapter callbacks. Pump
the executor and draw after activation. These test utilities do not install an OS
screen reader or establish physical platform acceptance.
