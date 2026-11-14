# Native vector presenter

With `presenter,vector-render`, `presenter::NativeVectorHost` joins the shared
vector compiler, native fonts, authenticated overlay windows and actual display
receipts. It creates no terminal, native window or event loop. Applications
choose their own tiling, GPU and GUI integration. This API is opt-in; ordinary
producer builds and default presenter profile selection remain independent.

Create an eventless `VirtualVivid` with an explicit overlay-enabled configuration
and attach `NativeVectorHost` before authenticating any producers. Construct and
poll the host on the native font/render worker, using an empty font family for
the platform sans-serif default. Attachment includes `NativeTextHost`; do not
attach a second text service. An event-producing presenter is rejected because
its outgoing projection path would introduce a second presentation authority.

The host loop has four responsibilities:

1. Install pane metrics on the presenter. Call `poll` regularly to service text
   requests and complete any returned platform requests. A `None` result can
   mean one text request was serviced; it does not end the loop.
2. Call `prepare(pane, scale)` to obtain an immutable `VectorFrame`. Its drawings
   are ordered back to front and its dimensions describe the physical target.
   Compilation runs outside the presenter's state lock. No presentation outcome
   is emitted by accepting or preparing a list.
3. Compose the drawings through `vector_renderer::compositor::Compositor` using
   the host's own renderer, or use the optional offscreen renderer below. Retain
   the exact `VectorFrame` with the output image through GUI upload and display.
4. Call `presented(&frame)` only after that image has actually been displayed.
   It returns `false` for a superseded, invalidated or otherwise stale frame.
   GPU submission, GPU readback and GUI upload alone are insufficient evidence.

Preparing also reserves the exact accepted source scene before compilation leaves
the state lock. A newer accepted scene cannot prematurely supersede that live
reservation. A prepared image may therefore be acknowledged after a successor
arrives, provided no newer physical scene has been published. Placement, surface,
channel/writer incarnation, resume, visibility and input-lane fences still apply.
Retained track bytes alone cannot authorize a recreated native window.

Encoded scene reservations share the existing native resource budget, with one
charge reused by duplicate prepares of the same incarnation. Frame objects and
the current displayed-scene cache keep that charge until their last reference
retires; dropping an acknowledged frame cannot force a cached peer to re-admit
its bytes. Reservation metadata holds weak references. The existing two-frame
per-pane and 64-frame host bounds, plus bounded native windows, still apply. A
source that cannot reserve new bytes closes only its own window; cached peers can
reuse their existing reservation and compiled resources. Dropping an unshown
prepared frame settles its superseded outcome on the next `poll` or `prepare`;
showing a newer frame also settles older unresolved scenes. Already-presented
scenes never receive another terminal outcome when their frame objects retire.

Display admission checks the presenter, pane, target generation, owner set,
session resume generations, complete window topology, placement revisions,
active vector tracks and exact submissions. Retained frame-object identity also
fences destruction followed by recreation with identical local IDs, channel
generation, epoch and revision. Admission preflights the entire composition
before committing its windows. Cached repeat displays never duplicate terminal
submission outcomes or advance the projection clock without a state change.

The owner set includes only live sessions that negotiated `vector-scene-v1`.
Raster-only sessions in the same pane retain independent media state and cannot
invalidate vector receipts by connecting or disconnecting. `VectorFrame::rasters`
adds their decoded scene-node images beneath the vector window plane. Use
`render_mixed(frame.drawings(), frame.rasters(), width, height)` on the offscreen
renderer, or the corresponding compositor method with a host-owned GPU device.

After admission, pointer and wheel routing use the same compiled paths,
transforms, fill rules and clips that were drawn. Native composition and
viewport checks gate input, keyboard focus and local pointer capture. Hidden,
suspended, moved or invalidated compositions cannot grant input from an earlier
image. Call `set_visible(pane, false)` before hiding its output and
`set_visible(pane, true)` when showing it. Install pane metrics first, including
for panes hidden before their producers authenticate. Both transitions revoke
outstanding receipts and held overlay input; showing requires a fresh displayed
receipt even for cached pixels. Keep polling while hidden. Timed native audio
and video retain their buffers and freeze their local clocks without changing
producer PLAY intent; Live media continues. Explicit producer PAUSE survives
showing. Presentation and playback-start/end waits and DRAIN return `NOT_VISIBLE` while held.
Use `invalidate(pane)` for focus, target or renderer loss without imposing a
playback pause.

For hosts with a separate render worker, obtain `host.visibility_control(pane)`
after installing metrics and clone the `NativePaneVisibility` controller into
the GUI thread. Call its `set_visible` directly from visibility callbacks. It
uses the presenter's bounded state/control path without waiting for fonts, GPU
work or an audio callback. Queuing visibility behind render work can let an
entire short audio clip play after the GUI has hidden its pane.

Both transitions advance a private pane visibility revision and revoke native
input. Prepared frames carry that revision and their host attachment identity;
an old frame stays invalid even if hiding and showing both happen before the
render worker polls again. A new displayed receipt is still required. These
private revisions are independent of wire surface hold serials.

Input and pinned text targets also retain the visibility revision and host
lifetime. A fresh display does not make a cached pre-hide target valid again:
held repeats, queued text and old IME bindings need fresh input admission. Text
focus comparisons include the same scope, even if the editor and pixels match.

Controllers are `Send + Sync`, pane-scoped and registered under the host's same
64-pane admission. They use weak references and become harmless no-ops after
their host retires; reusing a pane number cannot redirect an old controller.
Revision exhaustion hides and revokes that pane instead of wrapping. Ordinary
render-thread integrations can continue to call `host.set_visible`.

For native media, poll `controller.media_recovery_pending()` from the GUI thread
to show a pane-local retry control after an output failure. This read-only query
does not wait for the render worker or touch the output device. Ordinary PAUSE,
hidden panes and replacement pre-roll alone do not request a retry. Following
an explicit user retry or restored-device notification, call
`controller.recover_media()`. That releases the device hold; the producer still
replaces its retired media and satisfies synchronized startup readiness. Do not
automatically release every observed hold: an unavailable device could otherwise
cause an unbounded retry loop. Both methods become no-ops after host retirement.

Native attachment removes `timed-media-sync-v1` from the offered profiles before
authentication. Production attachments continue to support baseline PLAY.
The internal synchronized-start scheduler and retained-decoder visibility hold
publisher have [conformance coverage](AUDIO.md#native-hold-publisher), using
test-only negotiation. Hold checkpoints now survive lease reconciliation;
lease-driven replacement requests and fresh group admission are covered too.
[Host and device retirement](AUDIO.md#native-host-and-device-retirement) now use
the same transactions. Vivi's first-party producer recovery policy has native
conformance coverage. The compositor's vector frontend exposes a pane-local retry
control and has a test-only rendered Vivi recovery check. Platform lifecycle and
device-availability acceptance remain necessary before enabling the profile.

The outer application must preserve physical event ordering. Keep display
acknowledgements and input in one ordered dispatch stream, or retain an
equivalent exact target with each event. A delayed event must never be retagged
to the latest displayed scene. Bind each queued event to its GUI image delivery
and admit it only when that delivery's display receipt succeeded. If a rejected
receipt describes the image physically on screen, clear that delivery's input
admission and invalidate the pane; an older admitted composition cannot authorize
input for the rejected image. Held releases retain their original owner/binding
or are cancelled on revocation. The compositor's `sdk-app` now integrates this
path through an explicit `rendering = "vector"` preview mode. Its default raster
backend remains separate; wider platform/recovery services remain integration work.

## Raster scene nodes beneath vector windows

Native preparation snapshots active raster tracks and their visible scene nodes
under the same presenter lock as the vector snapshot. It converts terminal
32.32 cell coordinates to logical pixels with that pane's cell dimensions. The
compositor uses pixel-sized cells and requires no terminal runtime. Raster nodes are ordered by z-index,
then complete owner/context/node identity. The overlay window plane stays above
them. Image fit (fill, contain, cover or none), nearest/linear sampling, node
opacity and optional clips are applied before vector windows are composed.
Contain, cover and unscaled images are centered in the destination rectangle.

`RasterImage` wraps immutable straight RGBA8 pixels without copying their
allocation. A host can retain one aggregate resource-charge token on the image.
Native hosting charges cached images against its existing drawing-resource
budget, including references held by old frames and the GPU cache. Hidden or
inactive raw raster nodes retire cache ownership independently;
encoded tracks keep their primed output until track/source invalidation.
Preparing a neighbor does not evict another pane's live resources. A rejected
image or placement is skipped without closing a peer's vector window. Images
are limited to 8,192 pixels per axis and 64 MiB. The renderer admits at most
256 raster nodes and 64 MiB of unique raster images per composition, and checks
the combined raster/vector encoding budget before GPU mutation. Invalid layers
leave the previous GPU cache intact.

Accepting media bytes, preparing a frame, rendering and GPU readback do not mark
a native raster as displayed. `presented(&frame)` checks each raster's complete
source identity, surface/channel generation, canonical channel writer (including
clean EOS detachment), resume generation, node configuration and immutable
retained pixels before advancing its actual presented frame ID. Stale raster
receipts are skipped without rejecting an otherwise valid vector display. A new raster
frame, node change or owner teardown cannot invalidate an unrelated vector
receipt or grant raster input through an overlay target. `QUERY_TRACK` and
`WAIT_TRACK` use the actual displayed raster frame ID rather than the latest
accepted media ID. Delta frames use the presenter's validated full framebuffer;
the GPU path never invents a second delta chain.

Use `VectorFrame::same_content` for GUI image reuse: it compares both render
planes and their private receipt contexts. `same_input_context` deliberately
continues to describe only vector input grants. Clean EOS retains static raster
pixels after channel detachment. Source-loss recovery, video decoding and desktop
input for raster-only surfaces remain separate work.

## Static PNG and JPEG scene nodes

Enable `presenter,vector-images` to decode encoded-image tracks into the same
background plane. `vector-images` includes `vector-render` and optional `image`,
`moxcms` and `crc32fast` dependencies. It does not enable GPUI or a GPU adapter.
Default producer/presenter builds retain their existing dependency graph. Native
track probes and creation reflect the compiled codecs and decoded image limit;
the generic presenter/relay role keeps its existing host capability behavior.

Length and optional SHA-256 validation precede decoding. Native preparation
queues work without decoding under presenter or GUI locks. One lazy worker
admits at most two jobs across active work, queued work and unpolled results.
Waiting sources are selected in rotating complete-identity order across panes.
Call `prepare` regularly even when no active scene is drawable: it also primes
inactive, unplaced encoded tracks. Successful decode publishes decoder-initialized
and output-ready milestones before a producer's atomic activation. Media-byte
acceptance alone publishes neither milestone. A failed decode never reports
output ready. Primed tracks remain independently charged until retirement or
invalidation, so activation does not depend on a visible-node decode cycle.
Decoded pixels are charged against the shared image budget before submission;
old GUI frames and GPU references retain that charge after cache eviction.
Completed work is installed only for its matching job and current source,
channel, writer, resume generation and immutable encoded bytes. Cleanup cannot
redirect late results to another owner reusing local IDs.

The decode profile limits each axis to 8,192, encoded and decoded RGBA bytes to
64 MiB each, and pixel conversion workspace to 128 MiB. Decoder allocations have
an independent 64-MiB limit. PNG chunks use checked lengths and CRC validation;
APNG chunks are rejected. JPEG rejects MPO markers, more than 100 scans, and
concatenated/trailing pictures. Both formats require the exact declared pixel
dimensions. Orientation metadata is ignored and no external resources are fetched.
PNG keeps straight alpha; JPEG output is opaque.

Untagged input is interpreted as sRGB. Supported embedded RGB/gray ICC profiles
are converted through `moxcms` into sRGB using bounded row scratch storage. PNG
cICP takes precedence, followed by ICC, sRGB, then legacy gamma/chromaticities,
following the [PNG color-chunk ordering](https://www.w3.org/TR/png-3/#11cICP).
This decode profile supports full-range RGB cICP; other ranges fail decoding.
ICC input is below 4 MiB with at most 128 tags, 256-KiB CLUTs and 4,096-entry
tone curves. Unsupported/malformed profiles fail decoding; in particular a
CMYK ICC profile is refused because the image decoder has already converted
its original CMYK channels. It is never incorrectly applied to RGB output.

A decode failure loses only the still-current source track and sends its
decoder error; peers keep rendering and receiving input. The failure is retained
to avoid decoding the same invalid bytes every frame. Teardown discards queued
jobs and joins the worker after its current library call finishes. There is no
hard per-decode time limit or mid-call cancellation. This cache reuses an active
track's immutable decoded image; context/hash cache lookup currently takes the
normal miss-and-transfer path.

Accepting or decoding an image does not report presentation. Only acknowledgement
of the exact displayed `VectorFrame` advances an encoded track to presentation
ID 1. Replaced, hidden or detached sources cannot borrow that receipt, and an
encoded-image-only composition grants no vector input authority.

## Held input across repaint

After successful `presented`, call `input_target(&frame)` and associate the
returned opaque `VectorInputTarget` with that exact GUI image delivery. Route
physical reports through `dispatch_input(&target, VectorInputEvent)`. The target
retains identity metadata and weak channel references, not compiled scenes or
frame-receipt capacity. New presses and independent committed text require the
current displayed target. `retire_input_target` prevents further new input from
an image while preserving its existing held transitions.

Keep the original target on each held key and pointer gesture. A later image
can replace the scene while key repeats/releases and pointer motion/releases
continue to the original live window. Releases over another window retain the
original region and cannot click through. Optional key text is dispatched under
the same lock as its physical report; focus changes cannot split those events
between owners. Rejected reports must be discarded without substituting a newer
target. `Delivered { text }` indicates host consumption and text enqueueing, not
an acknowledgement of producer-side application handling.

Placement changes defer held reports until matching geometry has actually been
displayed. `poll` and `flush_input` retry the bounded queue, preserving transitions
and coalescing adjacent pointer motion. A blocked pane does not block another
pane's queue. The queue holds at most 64 reports; exhaustion invalidates that
pane and returns `WouldBlock`. Validate reports before mutation. Invalid geometry,
modifiers, key/button codes or text exceeding 4,096 UTF-8 bytes return errors.
`has_pointer_grab(pane)` lets hosts avoid replaying idle hover during an active gesture;
`retains_input_target` identifies obsolete delivery metadata that can be dropped.

The host remembers the last accepted pointer position per pane. When the final
held button is released, it refreshes hover against the current admitted pane
composition, including other owners. The release itself still reaches its original
window. Refresh emits hover transitions without manufacturing another physical
pointer report. Input-target creation after displayed repaint also refreshes a
stationary pointer; rejected reports cannot change that position. Invalidation
forgets it. Drag/resize completion clears hover if it changed placement; hover
returns only after displaying and binding a receipt for that placement.

Live validation includes the full source and window identities, surface and
channel generations, session resume generation, canonical bulk and input-lane
writers, focus, eligibility, target generation and host invalidation epoch.
Loss or replacement cancels or rejects held input without sending it to a new
incarnation. Host focus/visibility/target loss must still call `invalidate`.
Overlay input leases use authenticated interactive-lane generations and
presenter monotonic time. Authentication alone grants no input; the first valid
renewal activates the lease. A duration of 250,000–5,000,000 microseconds is
required. Ordinary renewal retains the current GUI image context. Expiry, lane
loss and replacement disable that owner's windows for input, release capture and
focus, remove modal blocking and clear gesture state while retaining its pixels.
A revoked generation cannot be renewed or reopened; recovery authenticates a
strictly greater generation and requires a fresh display receipt.

`VectorFrame::same_input_context` distinguishes equal pixels that need different
input admission. Include it in GUI delivery/cache comparisons. A receipt prepared
under another input generation is rejected. Replacement between display admission
and `input_target` can also return `None`; invalidate and prepare a fresh receipt.
The presenter binds each authenticated overlay owner to one immutable pane and
keeps a protocol window manager per pane. Focus, modal blocking, capture, held
keys, hover, click sequences and clipboard gestures cannot cross that boundary.
`cursor(pane)` returns only that pane's admitted pointer shape. The presenter
retains at most 64 pane policy states; owner and window limits remain global
(16 owners and 256 windows), with at most 32 windows per owner. Revoking a pane
removes its policy state. Native frame admission retains its separate lifetime
limit on distinct pane IDs.

Native pane deactivation prevents both physical input and producer focus requests
from reactivating it. Focus requests remember a candidate for subsequent host
activation. Each actual window focus transition advances a checked generation;
held input and clipboard gestures retain the original generation. A rapid focus
round trip cannot restore a cancelled hold or banked clipboard gesture. Exhaustion
fails closed. Editor caret metadata remains scoped to its full window identity
and scene revision, and only the pane's current eligible focus is exposed.

Image and scene caches use complete owner/context/surface/track/channel scope.
Asset namespaces retire independently; rejecting one malformed scene closes its
own window and descendants while healthy peer owners continue. Actual channel
connection identity fences late cleanup, including recreation that restarts its
numeric channel generation. Retained images and font-layout tokens stay charged
while old compiled frames still reference them. Preparing one pane does not
evict another live pane's cache.

Compiled encodings, hit paths and resource-token storage are bounded. Retained
text-layout expansion is checked before scene append, and aggregate GPU encoding
is checked before composition. The compiler ceiling is 64 MiB per scene or
composition, while the native host additionally enforces its configured
aggregate retained-media budget. Native text has its own bounded layout/source
accounting. At most two outstanding frame objects per pane and 64 across the
host are permitted; clones share one receipt. Drop obsolete frames promptly.
The current host admits at most 64 distinct pane IDs over its lifetime. Physical
targets are at most 8,192 pixels per axis and use a finite scale greater than
zero and no greater than eight.

Dropping the native host resolves unfinished native windows and detaches its
text services. It never restores synthetic display acknowledgements. A frontend
can retain the presenter and authenticated sessions, then call
`NativeVectorHost::attach_replacement` after the old host has fully retired.
The replacement restores the original audio opt-in policy and receives a fresh
attachment identity. All panes start hidden: install metrics and explicitly call
`set_visible(pane, true)` when each viewport is ready. Recreate lost media tracks
and submit fresh window content; old controllers, input targets and frame receipts
cannot authorize the replacement. A live attachment, absent retirement history
or exhausted attachment generation is rejected.

For a device/backend failure without replacing the host, restore local
availability and call the pane controller's `recover_media()` when native audio
or video is enabled. It releases the pane's device gate without opening devices
or resuming retired PLAY operations. Negotiated synchronized producers must still
complete fresh-track recovery. Baseline sessions retain baseline behavior.

## Clipboard writes

A platform host may explicitly add `overlay-clipboard-v1` to its presenter
configuration before authenticating producers. It is write-only; it provides no
clipboard read or paste API. After `poll` returns a `SET_OVERLAY_CLIPBOARD` request,
claim it with `VirtualVivid::overlay_clipboard_write(request.id)` and pass the
opaque `OverlayClipboardWrite` to the platform thread through a bounded queue.
It can be claimed once. Dropping it refuses the producer's correlated request.

Check the outer application's current activation, visibility and delivery epoch,
then call `perform` with a bounded platform write callback. The callback runs under
the presenter state lock and must not wait or reenter the presenter. The SDK checks
the original window generation, focus generation, input-lane writer, session resume
generation and gesture age at execution. Current displayed geometry and an eligible
focus are required. A newer gesture cannot extend a request's original two-second
deadline, and a focus round trip cannot revive it. Input loss, teardown, refusal
and abandonment complete with correlated errors. A successful reply follows the
actual write rather than queue admission. At most one write is pending per producer;
the existing global host-service budget also applies. No accepted-write history
is retained on this native path.

The compositor's vector adapter implements this using a four-entry mailbox and
the same GUI clipboard operation used by its terminal and Wayland hosts. It
continues rendering, input handling and watchdog maintenance while a write waits.
Its counter example accepts Ctrl/Cmd+C to copy the current count.

## Host paste and asynchronous text

Capture `NativeVectorHost::text_target(&image_target)` from an admitted display
before starting an asynchronous clipboard read. The resulting opaque
`VectorTextTarget` pins the original window, canonical source/input writers,
resume generation, focus generation and host input epoch. It retains no compiled
scene or frame receipt. Ordinary repaint may retire the image while the text
target remains live. A focus round trip, owner/track/lane replacement, metrics
change or explicit host invalidation rejects the old destination.

A host may cache this target for the displayed editor focus. At a paste gesture,
pin that exact cached target and validate it through `editor_state` before
admitting the platform read. This avoids losing the gesture while a newer raster
image lacks admission during ordinary repaint. A valid focus can temporarily
have no caret or ready geometry; text commitment then defers. A rejected cached
grant must not be replaced with another window's target for the same gesture.

Pass bounded UTF-8 text to `dispatch_text`. The ceiling is
`MAX_VECTOR_TEXT_BYTES` (65,536 bytes), and validation precedes mutation. Text is
split into at most 4,096-byte protocol records on UTF-8 boundaries under one
presenter lock. Every chunk targets the same window and current displayed scene;
the producer still receives ordinary committed-text events. `Deferred` emits
nothing and permits retry after actual geometry display or earlier physical input
completes. The host owns retry storage and deadlines. A rejected text target does
not cancel another input grant or release a newer pointer capture. Empty text is
a no-op. Delivery reports host enqueueing, not producer acknowledgement.

Use `queue_text(target, text, deadline)` when later physical input must remain
behind a deferred transfer. For this method, `Deferred` transfers ownership to
the SDK; do not retry it. The shared 64-entry input queue preserves ordering with
physical reports and IME commits. An expired queued transfer is discarded without
cancelling other input. Queue exhaustion revokes the affected pane.

The compositor reserves its platform's Ctrl/Cmd+V shortcut and supports four
pending paste actions per application. The GUI pins its already-displayed SDK
editor focus and application epoch at the gesture. Its ordered worker validates
that exact text target, then admits a bounded asynchronous GUI clipboard read. Reads and
paste commits preserve paste-action order, with a one-second transfer budget.
Each continuation retains its original application epoch and request ID; focus
loss, visibility change, close, timeout or a stale response discards the transfer.
Oversized reads are refused without truncation. Input, font service, rendering
and watchdogs continue while the read waits. No producer clipboard-read request
or new wire record is introduced. Completed reads use `queue_text`, retaining the
original one-second deadline while waiting for matching displayed geometry.

## Native composition and candidate geometry

`VectorImeEdit` carries an optional commit, replacement preedit and optional
selection expressed as UTF-8 byte offsets. Preedit is bounded to 4,096 bytes and
commits to 65,536 bytes. `dispatch_ime` validates all lengths and selection
boundaries before mutation. A commit clears the old preedit, emits committed text
chunks, then installs any replacement preedit under one presenter lock. Empty
preedit clears composition. `Deferred` transfers ownership to the SDK's ordered
input queue; adjacent pure preedit updates may coalesce, while commits and
physical transitions retain order. Deferred keyboard reports also retain their
original focus generation, so focus round trips cannot revive them.

Capture an immutable `VectorTextTarget` for each native input-handler session.
`same_focus` compares the full source, lane and focus grant while permitting
ordinary repaint. `editor_state` returns locally cached normalized candidate
geometry while that original destination remains live. Geometry may be absent
between a new scene display and producer republication; this does not discard
the composition. A stale destination returns no editor state.

Producers publish `OverlayWindow::set_editor_geometry` for an actually displayed
scene revision in the focused, input-eligible window. The presenter caches its
focus generation, clips the caret to the window and pane, and never revives old
geometry after a focus round trip. `NativeVectorHost::poll` acknowledges these
validated cache updates without GUI-thread or producer round trips.

The compositor installs a separate immutable GUI input handler for each focus
grant. Native callbacks query only its bounded local composition and cached
candidate rectangle. It converts UTF-16 ranges to checked UTF-8 boundaries,
preserves composition across repaint, and rejects stale callbacks after focus,
visibility, lane or runtime loss. Physical character keys defer text insertion
to the native input method when requested, avoiding duplicate committed text.
The counter renders preedit separately from committed-text byte counts.

### Surrounding documents and native replacement

Local native hosts can explicitly offer `overlay-editor-v1` in
`PresenterConfig.supported_profiles`. It requires `overlay-text-v1`; generic,
font-only and legacy terminating relay hosts decline it. An explicitly configured
[gateway document route](../../vivid_gateway/README.md#document-relay) can re-originate
it through a capable physical native host. The Rust `OverlaySession`
helper accepts it through `ProducerConfig.optional_profiles`; request it explicitly
when handling document events. Producers that publish no document context retain
the composition-only behavior above. Python, TypeScript and Lua do not automatically
request this profile or expose typed document operations yet.

After displaying a focused scene, publish `OverlayWindow::set_text_context` with
an `overlay::wire::text::editor::Context` (available through `vivid_protocol`).
The excerpt excludes preedit, includes the entire selection, and contains at
most 4,096 UTF-8 bytes. Cursor/anchor are checked UTF-8 byte offsets and may form
a reversed selection. Sensitive contexts carry no surrounding text and zero
offsets; this masks native document queries, without promising OS secure-input
mode. Passing `None` disables document editing and retains the identity floor.
Re-enabling it or changing sensitivity requires a newer editor incarnation.

Advance the nonzero `editor` incarnation on field switches and external document
or selection changes. Ordered native edits retain it and advance `revision` when
they change text/selection. An unchanged revision must publish identical context.
Republish after each displayed repaint; queries retain the previous snapshot
while edits wait for that scene's context. The presenter retains at most 64
contexts separately from drawing/layout budgets.

`VectorEditorState::document` exposes the bounded local snapshot under the same
immutable `VectorTextTarget`. Capture its editor incarnation on the native handler.
`dispatch_document` takes a `VectorDocumentEdit`, queues deferred operations under
the existing 64-report ceiling, and checks editor, source, channel, surface, focus,
visibility and host lifetime again before delivery. Absolute selection/replacement
requires the exact snapshot revision. Relative typing may retain an earlier
revision from its own ordered replies. Commits/preedit are each limited to 4,096
bytes in this profile. Producers receive `OverlayLaneEvent::EditorEdit`, check
`editor_event_targets`, and validate against their current context before mutation.
They remain responsible for bounded document storage and advancing identities.

The compositor's native handler maps document and preedit UTF-16 ranges to checked
UTF-8 boundaries. It supports surrounding text queries, reversed selection, document
selection, replacement covering the current selection, partial preedit replacement
and composition spanning document text. Absolute operations wait for the producer's
new context after a commit/selection; no native callback queries a producer
synchronously. A field-incarnation change installs a fresh handler and rejects old
callbacks. The counter demonstrates bounded producer-owned document mutation.

Automated native Metal checks exercise these callbacks with real producer processes.
Physical input sources, candidate menus, OS secure-input behavior, VoiceOver,
non-macOS platforms and production mux integration remain unrun gates. The local
compositor now offers an ordinary opt-in `vector-gateway` route; its Metal
qualification uses synthetic callbacks and simulated display completions.
The opt-in gateway core separately passes two-hop transport and native presenter API
checks; these do not exercise GPU submission through the gateway frontend.
The compositor's separate [focus follow-up](../../vivid_compositor/docs/performance/2026-11-12-sdk-vector-focus-macos.json)
uses an explicitly bound window-action route with headless Metal and simulated GUI frame
completions. Its peer receives keyboard/editor focus from the producer request,
without a pointer gesture. The physical host remains authoritative; the SDK
acknowledges the independent inner window revision without creating a focus grant.
See [producer window-action forwarding](../../vivid_gateway/README.md#producer-window-action-forwarding)
for original-session admission, cancellation and the remaining production gates.
The [window-action follow-up](../../vivid_compositor/docs/performance/2026-11-12-sdk-vector-actions-macos.json)
adds raise/lower stacking without focus changes, pane-relative translated center,
parent/child close and same-ID peer isolation. The SDK API is now
`attach_overlay_action_relay` with `OverlayActionRoute`/`OverlayActionRequest`,
replacing the focus-only names. Producer `request_close` dismisses the group and
fences its submissions; `close` still releases retained resources after dismissal.
The route reuses Vivid 1.5 actions and introduces no wire or registry changes.
The [local macOS pressure follow-up](../../vivid_compositor/docs/performance/2026-11-12-sdk-vector-pressure-macos.json)
qualifies bounded action saturation and callback deadlines through authenticated
sockets. Relayed actions share the SDK's 64 host-worker/eight-per-owner ceiling;
the control reader remains live. The gateway admits four jobs per original owner
within its 16-job pool. `OverlayActionRequest::is_pending` supports non-consuming
queue expiry checks; only `claim` authorizes dispatch. Expired requests retire even
without a surviving projection, and late acknowledgements cannot advance the
original window. An already-admitted physical result remains uncertain after a
timeout and is never automatically retried. This adds no GPU stress or physical
input/device qualification.
The [source recovery follow-up](../../vivid_compositor/docs/performance/2026-11-12-sdk-vector-resume-macos.json)
adds real two-hop source lease resume and canonical input-lane replacement through
the native presenter API. Lane revocation retires queued/claimed focus, raise and
lower admissions promptly; center/close retain control authority. The action route
exposes opaque `OverlayInputEpoch` comparison metadata and one-shot
`OverlayRelayInput` receipts. The gateway requires `take_overlay_routed_input` for
bound action routes; forwarding rechecks the original incarnation and one-second
local deadline, including after input has been drained. It drops feedback from
changed incarnations and reconciles fresh physical identities with retained frame
replay, preserving same-ID peers. Opening/revoking an SDK input lane advances
the projection revision. Leased producers adopt retained handles after resume,
advance their media channel and authenticate a newer input lane. These API checks
add no GPU/device-loss or physical-input qualification, and the developer adapter
still does not implement a producer's automatic lease recovery policy.
`timed-media-sync-v1` remains withheld.

The [local Metal first-key follow-up](../../vivid_compositor/docs/performance/2026-11-12-sdk-vector-first-key-macos.json)
resolves three ordering failures in the developer relay. `OverlayActionWindow` is
read-only owner/context/surface/generation/revision metadata, not authority.
`OverlayActionRequest::preceding_windows` captures the bounded original owner window
barrier; changes retire the admission and `claim` checks again. Project those roles
before physical actions so popup creation cannot steal the requested focus.
`OverlayActionRoute::window_stamp` lets the relay hold vector media until that
original generation's physical role exists. Neither check waits for media or peers.
Revision-only source acknowledgements no longer republish identical physical options;
real geometry changes still retire stale input targets. Repeated runtime-shader
Metal launches verify four synthetic key downs for the exercised source, admitted once each,
with no peer keys, retries or increased action deadline. Physical input, sustained
production startup/load, static shaders and other platform/device gates remain unrun.
The [Unicode composition follow-up](../../vivid_compositor/docs/performance/2026-11-12-sdk-vector-unicode-macos.json)
addresses the overlapping-workload timeout with two controlled two-hop regressions.
An opaque `OverlayDocumentTarget` preserves its original editor grant across ordinary
repaint, retaining the window/source, track/channel/epoch, focus, lane and authenticated
session incarnation. Delivery still checks the edit's exact mapped displayed scene;
stale scenes and focus round trips fail. Each gateway publication retains a separate
target under a fresh physical excerpt revision and installs its mapping before
physical publication, covering input between context application and acknowledgement.
No callback samples replacement authority, retries an edit or extends its deadline.
Finite overlapping runtime-shader Metal checks cover the corrected local path;
sustained production workloads and physical platform/device gates remain open.

## Live video

Optional `vector-video` adds native live video scene nodes to the raster plane.
It enables the shared `video-decode` support and `moxcms`; image decoders, GPUI and
a GPU backend are independent features. The current native profile accepts the
canonical H.264, HEVC, VP9 and software AV1 packetizations in Live mode with
declared reorder depth up to 64, subject to the batch and aggregate budgets.
Audio is a separate, explicitly enabled experimental `vector-audio` integration;
see [native audio](AUDIO.md#native-live-hosting). Matching Live tracks use the
[linked Live A/V clock](AUDIO.md#linked-live-av); timed A/V uses the audio-master
contract below. Decode errors fail the source track.
Custom hosts must select `PresenterConfig.supported_profiles` to match their
implemented services, including `timed-media-v1` for clocked video or standalone
timed audio and omitting `timed-media-sync-v1`; offer audio gain only when native
audio was explicitly enabled.
`terminal_with_overlay` is a generic presenter preset, not that selection. The
compositor already filters its offered profiles to its implemented services.

Admission reserves two complete output buffers, a serialized decode batch of
`maximum_reorder_depth + 1` complete images and one conversion row against the
same aggregate native drawing budget as
vector, raster and encoded-image work. A source's reservation is charged before
its runtime begins. Each image is independently limited to 8,192 per axis and
64 MiB. The complete decode batch is additionally limited to 256 MiB, with checked
total reservation sizes. Probes observe current budget
availability; they do not reserve it. Native codec reference memory, transform
tables and other library-internal allocations are additional memory.

Each channel performs native decode and color conversion on its own existing
media worker, outside presenter/GUI locks. Its decoder uses one native thread,
bounded reordered batches, no RGBA recycling and FFmpeg's native pixel bound.
Its per-image byte ceiling is independent of its batch byte ceiling, so allowing
delayed output cannot enlarge an individual image. All returned dimensions are
checked against the immutable configuration. Each live batch coalesces to its
greatest output PTS after preserving codec reference state; only that picture
needs color conversion and publication. EOS also drains delayed pictures this
way. Packet PTS and decoded-picture PTS can differ when reordering is declared.
Immutable output slots remain occupied while a retained GUI/GPU image owns them,
including across channel replacement. An additional work lease serializes old
and replacement decoders' RGBA work. Full slots wait on the source's worker;
after its configured maximum latency, obsolete output is discarded while codec
references remain valid. Cancellation is rechecked during waits and before
installation. Control, input, fonts, rendering and other owners use independent
dispatch paths. An in-progress native codec call has no hard cancellation deadline.

Packet and EOS admission capture the original epoch, decoder reset, surface and
resume generations, and authenticated channel writer under the admission lock.
The worker carries that scope across the unlocked handoff instead of recapturing
it from newer state. FLUSH can therefore retire admitted work before decode starts
without old pixels or an old drain marking the new epoch ready or ended. A stale
packet returns only its original allowance to its still-owning channel. The same
channel can recover with a key unit in the new epoch; unrelated owners continue.

Declared matrix/range conversion is followed by cached `moxcms` conversion from
BT.709, BT.601 625/525-line or BT.2020 primaries, with BT.709 or sRGB transfer, into
straight sRGB RGBA. Color conversion reuses one bounded row and retains no second
complete image. Converted pixel storage is shared into the renderer without a
copy. Decoder initialization/output-ready milestones follow actual native work;
media validation alone does not establish readiness.

After actual GUI display, the receipt checks the complete source identity and
original track lifetime (including destruction/recreation with identical IDs),
surface/channel and decoder-reset generations, session resume, canonical writer,
active slot, placement and node visibility. A newer decoded picture does not
erase proof of the older immutable image actually displayed. Presentation IDs
are checked monotonic native picture serials; they are separate from packet IDs.
Queries distinguish decoded and displayed PTS. EOS drains before buffered-ended
can be observed, and the last image remains available after clean detachment.
Accepted bytes, decode, GPU submission and upload grant no display or input
authority. Raster/video-only scenes cannot create vector input targets for a
producer window. Dropping the native host retires its video channels and keeps
deferred presentation policy in effect.

The compositor's `--sdk-vector-video` sample places this real H.264 stream beneath
an independent vector window. `--video-render-check` exercises it alongside a
JPEG/vector peer and a linked counter, including input, IME, clipboard, resize,
crash, restart and teardown. The new socket suite covers delayed display PTS,
channel replacement, malformed-source isolation, hidden placement, EOS,
retained-budget admission, host retirement, identical-ID recreation after clean
EOS and saturated output crossing owner-scoped replacement or same-channel
FLUSH. Two deterministic handoff regressions also cover FLUSH between packet
admission and decode, and between EOS admission and drain. The explicit
GUI scenario also reproduced and now guards a held-click cancellation when a
producer repaint overtakes an in-flight video image. That stale image receives
no fresh input grant; held transitions retain their original owner/focus binding
and wait for matching geometry instead of revoking the whole pane.
Reordered H.264 regressions also verify delayed EOS PTS, exact display receipts,
full-batch admission through retired GUI references and replacement with delayed
native output, while another owner reuses the same local IDs and keeps decoding.

## Timed video

The same `vector-video` feature supports video-only `timed-media-v1`. Baseline
PLAY uses rate 1.0, drop-presentation late policy, no looping and start policy 1.
Production attachment does not negotiate synchronized PLAY until its complete
hold/recovery contract is available; the internal scheduler is covered separately
in the [audio integration notes](AUDIO.md#synchronized-start-scheduler).
Timed raster/image/vector tracks are refused.
The compositor advertises the timed profile when its `video` or `audio` feature
is enabled. Video-only playback has a monotonic software clock per track. With
explicitly enabled `vector-audio`, the same surface's active timed audio slot
supplies its master clock; see [linked timed A/V](AUDIO.md#linked-timed-av).

Creation reserves a queue of
`ceil(maximum_rate_millihertz * maximum_latency_us / 1_000_000_000) + 1` images,
at most 256, in addition to the two retained images, full decode batch and
conversion row described above. The declared frame rate must be nonzero, and
checked arithmetic and the aggregate native budget gate probes and creation.
PLAY's maximum latency cannot exceed the admitted configuration. A source whose
pre-roll needs more pictures than its rate/budget claim permits fails its own
track. Paused/full queues exert backpressure on that media worker while control,
input and other owners remain independent. Retired images keep the complete
reservation charged until their last GUI/GPU reference is released.

Decoded timed pictures retain individual PTS and durations in a bounded queue.
The first picture may serve as a pre-PLAY preview and establish output-ready.
PLAY maps its exact start PTS to clock start when the minimum buffered horizon
exists, or completed EOS drain proves that no more pre-roll can arrive. An
initial future picture does not rebase the clock. Pre-target media is discarded;
only due pictures become render candidates, and obsolete due pictures coalesce
under the late policy. PAUSE freezes the clock and retains the current picture.
Resuming at that frozen position preserves its remaining duration. FLUSH clears
the queue and decoder state, pauses, and requires a new-epoch key unit. Channel
replacement, owner retirement and surface/reset changes fence queued output.
A greater media epoch also clears queued and delayed codec output and returns
timed playback to idle until another PLAY. Delayed pictures keep the surface
generation of their original packet rather than that of a later decoding call.

FLUSH replies independently of media-connection order. A structurally valid old
packet still unread at that point is rejected with nonfatal `STALE_EPOCH` on its
original channel. It consumes its wire allowance and returns reusable credit
once, without changing new-epoch readiness or satisfying the key-unit requirement.
The same channel can then accept a fresh key packet. FLUSH also wakes media
parked behind projection readiness; already-primed live audio is not charged a
second time. These paths preserve the complete owner and channel scope.

Each PLAY advances a private playback generation. An old preview or in-flight
receipt cannot acknowledge a later play operation. Presentation IDs advance when
the clock selects an image, independently of decode order and packet IDs. Decoded
target pictures selected for paused priming also acquire the current PLAY tag
and a fresh presentation ID, including pictures decoded before that PLAY.
Their fresh display receipt does not advance the paused clock. Decoded
and physically displayed PTS remain distinct. EOS drains reordered output and
stops the clock at the known end while final display proof is pending; a late EOS
never rewinds an already-observed clock. Buffered-ended requires both elapsed
media duration and actual display of the final retained picture, or an empty
stream. No queued/decoded image synthesizes that display proof.

Call `NativeVectorHost::poll` periodically even without a repaint. Polling,
preparation, authenticated control observation and media-worker progress advance
the software timeline without performing codec work under the presenter lock.
Playback-state records report transitions and EOS changes; status queries include
clock PTS, buffered horizon and late-drop count. Ordinary clock drift does not
produce an event per frame or continually wake pending waits.

`vivid_compositor --config examples/timed-video.toml` launches an independent
H.264/vector sample that requests 80 ms of pre-roll. Its producer command is
`--sdk-vector-timed`; `--timed-video-render-check` exercises real clocked
composition alongside an image/vector peer and linked counter. The sample checks
its single sender's current credit before writing, so media backpressure does
not park its vector event loop.

## Offscreen GPU output

`vector-offscreen` adds
`vector_renderer::compositor::offscreen::{Offscreen, Pixels}`. It selects a native
WGPU device with software fallback and owns one asynchronous readback slot.
`render(drawings, width, height)` starts changed output. `poll` drives GPU progress
without blocking and returns straight RGBA8 pixels with row padding removed.
Partial alpha is unpremultiplied for ordinary GUI image uploads. Missing adapters,
device loss, map errors and readback timeout return errors.

Keep polling idle renderers to service native device-loss callbacks.
`Offscreen::is_failed()` observes captured device/readback failure separately
from rejected geometry or an occupied slot. A device polling error also retires
the renderer. The flag belongs to that renderer; an old callback cannot fail a
replacement device.

Consume the readback before starting another: an occupied slot returns
`WouldBlock`. A `false` render result reuses the previously read image; preserve
those pixels on the host. The GUI still needs the matching fresh display
receipt. Immutable compiled-object identity participates in GPU cache reuse, so
a replacement scene redraws even when its numeric metadata is identical. The
cache retains those objects until replacement or renderer teardown.

GPU targets and readback allocations use checked dimensions and the shared
256 MiB target ceiling. Readback completion grants no protocol authority. On
renderer failure, invalidate the pane, retire the old renderer and pending
receipts, and require a newly prepared frame after recovery.

A host can retain `NativeVectorHost`, its font services, authenticated apps and
healthy decoders while replacing only `Offscreen`. First hide the pane through
its visibility controller and revoke frontend image/input deliveries, then drop
the failed renderer outside GUI/presenter locks. Retry device creation explicitly.
Pre-render the retained scene while hidden so cold GPU setup does not consume
timed media. That warm-up frame has no presentation authority. Restore the latest
requested visibility and prepare a fresh frame/receipt after showing; a visibility
revision change invalidates the hidden preparation. Producer PAUSE and separate
failed-audio holds keep their own scope.

With explicit `testing` support, `destroy_device_for_test()` destroys only that
renderer's logical WGPU device through its ordinary loss callback. The compositor's
`display-recovery-testing` checks exercise this native teardown, replacement,
fresh GUI receipts and owner isolation without resetting a physical GPU.

## Verification

From `vivid_sdk/`:

```sh
cargo test --no-default-features --features presenter,vector-render --test native_vector_presenter --test native_vector_channel --test native_vector_input
cargo test --no-default-features --features presenter,vector-offscreen --test native_vector_presenter -- --ignored --nocapture
cargo test --no-default-features --features presenter,vector-images --test native_vector_images
cargo test --no-default-features --features presenter,vector-images --lib images::
```

The first command exercises real authenticated sockets, owner isolation,
supersession, receipt bounds, recreation and leased recovery. The explicit GPU
check requires a working adapter and checks owner-scoped image colors, alpha,
transparent gaps, immutable-scene cache invalidation and retained native glyphs.
It does not skip a missing adapter. Linux software acceptance uses Mesa 25.
The image suites exercise real PNG/JPEG transfers, bounded worker admission,
static-only containers, sRGB conversion and exact display/owner isolation.


## Partial font coverage

Native SDK text and Vivido share the pinned Parley fallback correction in
[`vendor/parley`](../vendor/parley/UPSTREAM.md). A complete font match, including
canonical normalization, always wins. If no queried font covers the complete
cluster, selection preserves its first contributing character before comparing
remaining coverage and configured family order. An unsupported accent therefore
keeps a supported emoji base instead of replacing it with the primary font's
missing-glyph box. Complete matches avoid an extra base charmap lookup.

This policy does not compose one grapheme from multiple fonts. Marks absent from
the selected font may still show missing-mark glyphs. Source text and measurement contracts remain intact; custom providers use the
SDK facade for the bundled layout type identity. Deterministic licensed COLR/CBDT fixtures
cover base selection with one and multiple marks; the same fixtures keep the
vendor's existing layout/metric tests runnable without its unpublished test helper.
Archive, patch and fixture provenance can be verified with the bundled script.

Custom `TextShaper`/`StyledTextShaper` implementations should import layout and
font types through `vivid_sdk::vector_renderer::parley`. This public facade keeps
the exact dependency identity without requiring a consumer to name an SDK-private
filesystem path. Vivido uses that facade and no longer declares Parley separately.
A compiled rustdoc example verifies the public custom-provider contract.

### Deferred source presentation through a terminating relay

`VirtualVivid::attach_overlay_presentation_relay` binds one exclusive
`OverlayPresentationRoute` before any producer authenticates. It requires host
relaying and refuses local native/font hosts or another route. Capture an opaque
`OverlayPresentationReceipt` for the exact source frame before forwarding, then
complete it with the corresponding downstream presentation outcome. The gateway's
`overlay_document_editing` installs this automatically; callers must build the
bridge before starting inner producers.

Acceptance coalesces only the latest unforwarded submission per window. A scene
already forwarded retains its own physical outcome even if successors are accepted.
`capture` returns `io::Result<OverlayPresentationCapture>`: forward `Forwarded`
with its receipt, forward `Replay` without another original ACK, and consume
`Discard` without forwarding or reporting source loss. Complete each receipt or
explicitly `abandon` it when its physical attachment retires. Abandoning the final
copy keeps the newest accepted scene available for replay; dropping a receipt
alone supplies no physical outcome.

Copies and live receipt metadata are bounded to four per owner and 64 globally.
Additional retained forwarded canvases are charged by encoded size against
`MediaConfig::aggregate_retained_bytes`; admission failure discards the new copy.
This is an additional encoded-scene budget, not a decoded allocation measurement.
Duplicate-copy supersession waits for other copies, and late older display
outcomes cannot move original scene identity backward. Only a matching outcome
advances original display identity and ACKs. This preserves valid document edits
for the physically displayed scene while a repaint is in flight. Receipts check
original writer, resume incarnation, full source identity, surface and channel
generation, plus exact submission. Ordinary stale-scene checks remain in force. Track retirement revokes its document targets immediately. Cancellation
supersedes pending submissions, retires focus and leaves the mode closed; dropping a
route never restores synthetic display proof. Retained replay cannot acknowledge an
original submission twice. Same-ID peer cleanup and lease/channel replacement are
covered by local socket regressions. Physical scanout and automatic product recovery
policy remain separate responsibilities.

The compositor's display-loss status now overlays its retained app, preserving
the viewport through failure and retry. The previous layout banner resized the
producer twice and could invalidate the first restored keyboard target. Local
Metal checks for direct and relayed hosts now send that key once at first retained
state display admission, without waiting for editor republication. Canonical
focus, source, scene and geometry validation remain unchanged; external geometry
or authority changes still require the appropriate fresh display grant. See the
[retry follow-up](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-retry-tcp-macos.json).

Multiple accepted/forwarded revisions have local authenticated socket and native
host API regression coverage, including early Unicode commit, same-ID peers,
coalescing, copy/byte limits and repeated outer-attachment replacement. These
fixtures call the native presentation API without GPU scanout. Channel advance
now retires that exact source track's pending/forwarded scenes and encoded charges;
lease suspension drops unresolved scenes for only that owner. A native socket
regression fills four forwarding slots, advances the channel, fills four more,
resumes the lease and admits source revision 12 while a same-ID peer retains its
unshown original scene. Physical writer retirement releases its own receipt copies
outside the mapping lock, and a writer-specific fence prevents late reinsertion.
Stale prepared frames remain rejected. An aggressive multi-submission producer
under Metal remains unqualified; the long-lived Metal burst producer still waits
for each source presentation outcome.

The bounded four-source Metal streaming fixture now runs with multiple source
submissions between receipt polls. Two full local runs passed 769 exact physical
keys and 24 Unicode compositions, logical device retry and source replacement.
One repeat timed out waiting for focus/editor readiness after 144 owner batches;
that intermittent failure is unresolved. The prepared-scene outcome and resource
invariants have focused coverage, but continuous native editor readiness is still
an open qualification gate. See the [streaming follow-up](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-stream-macos.json).
