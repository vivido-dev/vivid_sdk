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
3. Start the drawings and rasters on `compositor::texture::TextureRenderer` on
   the host's shared device, or use the optional offscreen renderer below. Poll
   checked completion and retain the exact `VectorFrame` with that immutable
   output through GUI composition and display.
4. Call `presented(&frame)` only after that image has actually been displayed.
   It returns `false` for a superseded, invalidated or otherwise stale frame.
   GPU submission, GPU readback and GUI upload alone are insufficient evidence.

If the renderer isolates a failure to one prepared drawing, call
`reject_drawing(&frame, drawing.window)`. It closes only that current window and
its ordinary descendants; a late result cannot close newer content, a replaced
track or a resumed incarnation. Peer receipts remain pending until their fresh
checked image is displayed. A mixed composition failure alone does not identify
which window to refuse. The host owns bounded fault attribution and continued
peer publication; this API does not supply an automatic diagnosis loop. See the
[exact refusal contract](RAW-TEXTURE-PLAN.md#exact-drawing-refusal).

Preparing also reserves the exact accepted source scene before compilation leaves
the state lock. A newer accepted scene cannot prematurely supersede that live
reservation. A prepared image may therefore be acknowledged after a successor
arrives, provided no newer physical scene has been published. Placement, surface,
channel/writer incarnation, resume, visibility and input-lane fences still apply.
Retained track bytes alone cannot authorize a recreated native window.

Vector ingestion retains the validated epoch and revision with the immutable
encoded frame. Preparation reads those values only for that same allocation,
so repeated snapshots can identify a scene without decoding its canvas while
holding the presenter lock.

Encoded scene reservations share the existing native resource budget, with one
charge reused by duplicate prepares of the same incarnation. Frame objects and
the current displayed-scene cache keep that charge until their last reference
retires; dropping an acknowledged frame cannot force a cached peer to re-admit
its bytes. Reservation metadata holds weak references. Ordinary preparation keeps
the two-frame per-pane and 64-frame host bounds, plus bounded native windows. A
source that cannot reserve new bytes closes only its own window; cached peers can
reuse their existing reservation and compiled resources. Dropping an unshown
prepared frame settles its superseded outcome on the next `poll` or `prepare`;
showing a newer frame also settles older unresolved scenes. Already-presented
scenes never receive another terminal outcome when their frame objects retire.

`prepare_retaining(pane, scale, previous, windows)` prepares current peers while
keeping selected windows' exact committed scenes from the latest admitted frame.
Select identities from `previous.drawings()`. Their pending replacement bytes
are neither compiled nor acknowledged; retained compiled image resources remain
valid even after the producer releases the original asset. The complete returned
composition still needs checked rendering and actual display admission. The
method rejects stale topology/placement, target/visibility, input context,
resume, writer/channel or committed scene. It permits a third per-pane handoff
frame alongside the diagnostic reservation and prior display, within the same
64-frame host ceiling; ordinary `prepare` keeps its two-frame limit.

Native API acceptance retains an original red image while four fresh same-ID
peer scenes reach exact original outcomes and scene-bound Unicode. Pending
replacement receipts remain unresolved. Native Metal readback confirms each
red/blue, red/green and red/magenta composition, including after asset release.
Real socket cases replace either owner's canonical input lane without source
resume: the previous composition is rejected, fresh display succeeds, and the
same-ID peer's original presented outcome stays intact.
The compositor now integrates this prerequisite: eight paced peer revisions
display during actual diagnosis across direct/one/two-relay workers. Its one
private target is created with `Offscreen::fork`, sharing the device/queue and
failure observer while retaining an independent cache. Related private/output
jobs use `compositor::WorkBudget`, claiming dispatch credits before submission.
Exhaustion cancels without source blame; default and per-job bounds stay 64.
The compositor keeps its existing 127-attempt total and ten-second start window.
New peer displays permit only exact source-proof reservation refresh, never
rebinding to changed scene/track/resume. The ordinary two-hop window companion
also passes; continuous ordinary-window/Vivido, arbitrary rate/recovery/aggregate
mixtures and broader platform/device gates remain open.
[Retained-scene composition evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-retaining-macos.json).
[Interleaved peer evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-interleave-macos.json).

An explicit native GPU regression renders an older reserved scene after accepting
its successor, then checks exact outcomes only after simulated display admission.
It also renders the successor, retires a dropped unshown reservation, and removes
one owner while a same-ID peer keeps its original pixels and revision. These are
native offscreen readbacks, not physical display or IME acceptance. The
[local GPU evidence](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-reserved-gpu-macos.json)
records both explicit native GPU tests and the SDK verification matrix.

A separate two-hop gateway GPU regression retains unresolved scenes across
channel advance and authenticated source lease resume. It checks exact source
and same-ID peer pixels, rejects retired frames as display acknowledgements,
and admits only the fresh incarnation. GPU readback leaves source presentation
pending until that explicit admission. This qualifies the local host/relay API
path, not automatic frontend recovery or physical display lifecycle. See the
[GPU recovery evidence](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-recovery-gpu-macos.json).

The Rust producer wrapper also exposes explicit `OverlaySession::resume` with
complete window adoption. Native socket/host API regressions cover repeated
resume, pending outcome isolation, parent/child completeness, old image-handle
rejection, input renewal and a failed establishment that preserves a same-ID
peer. These tests do not submit GPU work. The local compositor's default static
producer now integrates this policy through direct and relayed native Metal;
other producer policies and physical recovery remain separate gates.
See the [automatic frontend evidence](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-auto-resume-macos.json).

The local lifecycle fixture also recovers under host-hidden visibility and logical
GPU loss, retries only the renderer while hidden, and closes a resumed owner
before fresh display admission. A test-support-only native control shutdown checks
the complete source tuple, resume generation and media-channel generation before
revoking input. SDK socket regressions reject stale scope and preserve the same-ID
root peer. Physical OS visibility notifications, GPU reset and packet loss remain
unrun.
See the [hidden frontend recovery evidence](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-hidden-resume-macos.json).
See the [producer resume evidence](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-overlay-resume-macos.json).

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
renderer, or `TextureRenderer::start` with a host-owned GPU device.

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

`RasterImage::new` wraps immutable straight RGBA8 pixels without copying their
allocation. Native raw-track preparation preserves the configured straight or
premultiplied interpretation in the immutable image and cache identity, without
converting the original pixels. Ingress checks premultiplied samples before
retained-canvas mutation. A host can retain one aggregate resource-charge token
on the image.
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

When a producer requests a change of focus to an already displayed overlay,
the action can change stacking and advance the window revision. The native host
publishes that gain on the interactive lane and through window status after it
presents the matching placement. Producers can still request focus before their
first drawing. Loss and cancellation remain immediate. A superseding focus
transition, lane replacement or invalidation retires an outstanding gain.

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
add no GPU/device-loss or physical-input qualification. The later compositor
static producer integration adds automatic lease recovery; other policies remain
the producer's responsibility.
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
For a relayed excerpt publication, claim `take_overlay_document_target(id, surface)`
while the request is pending. The presenter pins the grant at admission under the
same lock as the editor update. A foreign surface cannot consume it. Completion,
timeout and teardown retire unclaimed targets. Clears carry no target.
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

## Checked shared-device GPU textures

`vector-texture` adds `vector_renderer::compositor::texture`. It creates a
worker-owned renderer on a host-supplied device and health observer, without
replacing device callbacks or blocking the primary renderer. Start one bounded
job, retain its exact prepared snapshot under its opaque job ID, and poll for a
checked immutable texture. The host must still bind final display admission to
that snapshot. No presentation or input authority follows from GPU completion.

A persistent `TexturePool` bounds two outputs and one live/retiring worker across
replacement. Exports are copied from the checked working target before publication;
subsequent jobs cannot mutate them. `TextureImport` registers on upstream Vello
and retains allocation credit. Unregister after the last sampling submission,
then poll device progress for retirement; cached atlas metadata does not retain
an unregistered source allocation. Raw image clones become invalid on unregister.
Cancellation and local deadlines never destroy the shared device.

The native SDK regression covers exact same-ID owners and receipts, straight
alpha, retained pixels, cache reuse and recopy, pool saturation, exact-owner close,
stale display rejection, hostile overdraw, bounded refusal, empty recovery and odd
sizes. A separate worker can stay pending while upstream rendering and owner
control progress. Both map-stage cancellations and local timeout preserve that
device; a host loss observer retires work before further fine submission.
The terminal consumer has not migrated, admission is synthetic, and physical
GPU faults and other platforms/devices remain unrun. See the
[texture evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-texture-macos.json)
and [remaining integration](RAW-TEXTURE-PLAN.md).

## Offscreen GPU output

`vector-offscreen` adds
`vector_renderer::compositor::offscreen::{Offscreen, Pixels}`. It selects a native
WGPU device with software fallback and owns one asynchronous readback slot.
`render(drawings, width, height)` starts changed output. `poll` drives GPU progress
without waiting for completion and returns straight RGBA8 pixels with row padding removed.
GPU pass metrics advance after validation; sample them after successful `poll`.
Allocation/work refusal returns no image and leaves the device reusable. Dropping
a renderer with pending validation cancels further dispatches and drops its job
without polling or waiting for GPU completion. The private pinned backend accepts
callbacks whose receivers have retired; it preserves upstream Vello scene types.
Vello's straight-alpha bytes are preserved for ordinary GUI image uploads. Missing adapters,
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

The SDK vector renderer can supplement an emoji followed by combining marks
when native shaping leaves a glyph missing or splits the emoji and marks into
separate clusters. It shapes missing marks around a dotted circle, paints the
marks at the emoji and measures the repaired paragraph against the producer's
UTF-8 offsets. Retained text preserves each mark's style, wraps at the repaired
advances and fits ellipsis at original grapheme boundaries. The public
`PreparedText::layout_source` supplies the string indexed by native clusters.
`PreparedText::paint` draws the complete visible paragraph including fallback
marks. Other complex sequences continue to use the native shaper. Deterministic licensed COLR/CBDT fixtures
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
`MediaConfig::aggregate_retained_bytes`. Temporary owner/global receipt or aggregate
byte pressure returns `WouldBlock`, retaining the current scene and leaving its
original outcome pending. Call `begin_capture` before queueing or other delayed
work and retain its opaque `OverlayPresentationAttempt` across `WouldBlock`;
retry `attempt.capture()` after physical progress. The attempt holds weak original
writer/scene identities, resume, surface and channel generations, and the exact
submission. It reserves no copy credit or encoded bytes. Repeated numeric IDs
cannot retarget an old attempt or receipt to a recreated window/scene. The
one-call `capture` convenience method starts a new classification each time.
An obsolete or
permanently oversized scene is `Discard`. The gateway retries only on that source's
bounded media writer, without holding layout, revision or channel locks, and
retirement cancels the wait. These limits remain unchanged; a transiently full
owner must not permanently lose its last accepted scene.
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

Hosts replacing static outer attachments can use
`VirtualVivid::prepare_bridge_recovery_snapshot_with_viewports`. It returns a
projection and a lightweight `BridgeRecoveryFence`, atomically releasing selected
sources' superseded ingress allowance without acknowledging display or reporting
source loss. Audio/video deliveries stay intact and need a separate decoder recovery
policy. Covered queued events no longer pass `bridge_delivery_is_pending`;
later accepted events retain their normal identities and follow retained replay
on each source. Check `bridge_recovery_source_is_current` before each handoff:
presenter, complete source key, surface/channel, resume and decoder incarnation
must still match. A weak track lifetime also rejects destroy/recreate when all
numeric IDs/generations match. New media acceptance alone does not invalidate the cut.
Other owners keep their pending deliveries, including peers reusing local IDs.
The caller owns replay bounds and fresh physical admission. The local compositor
integrates this with bounded outer root replacement, while physical/packet-loss
faults and broader frontend policies remain open. See the
[automatic outer recovery evidence](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-outer-auto-macos.json).
The [asset follow-up](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-assets-macos.json)
also exercises released displayed and unshown image assets through three automatic
outer replacements, with same-ID owners and exact native readbacks. Retired frames
still render their immutable images but cannot acknowledge display. Reading pixels
leaves unshown source and peer receipts pending until fresh host admission; foreign
draw/release handles and released-handle reuse stay rejected. A separate ordinary
leased sample passes current visible-window capture, focus and close on macOS.

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
Stale prepared frames remain rejected. The long-lived burst producer waits for
each source outcome; the separate bounded streaming source below does not.

The bounded four-source Metal streaming fixture now runs with multiple source
submissions between receipt polls. Two full local runs passed 769 exact physical
keys and 24 Unicode compositions, logical device retry and source replacement.
One repeat timed out waiting for focus/editor readiness after 144 owner batches.
The compositor then fixed candidate queries that used an older GUI caret copy
instead of the current local worker snapshot. A focused handler regression checks
publication, clearing, republication and epoch retirement without GUI polling.
The subsequent 43.2-minute streaming soak passes 18,433 exact keys and 144 Unicode
compositions, GPU retry, source replacement and stale callbacks. This qualifies
the declared four-receipt, 100-ms offered batch policy; peak/higher-rate workloads,
packet-loss faults, production mux adoption and broader frontend recovery policies
remain open. See the [historical streaming follow-up](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-stream-macos.json)
and the [long soak](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-soak-macos.json).

A faster four-owner fixture requests batches every 10 ms while retaining the
overlay helper's 60-record/s declaration and token-bucket pacing. An 18.1-minute
run passes 7,681 exact keys and 60 Unicode compositions through GPU retry and
source replacement, with approximately 60 source scenes/s per owner measured
between rendered checkpoints. This covers the current declared ceiling, not
400 GPU/display FPS, other track declarations or general peak throughput.
See the [higher-offer evidence](../../vivid_compositor/docs/performance/2026-11-13-sdk-vector-fast-macos.json).

### Aggregate relay window admission

`VirtualVivid::set_overlay_window_capacity` bounds windows across all inner owners
and panes under the presenter state lock. Configure this before producers connect.
A gateway projecting several owners onto one physical outer session should use
`vivid_protocol::overlay::MAX_WINDOWS_PER_OWNER` (32). Otherwise independently valid
inner owners can collectively exceed that physical owner's protocol window limit.
Zero, a value above the protocol aggregate ceiling, or a limit below the existing
window count fails without mutation. Rejected creation preserves existing owners;
closing a window returns capacity. This is local admission policy and changes no
wire assignments or media-source budget. The compositor's ordinary static relay
installs this bound and exercises eight same-ID owners through native readback and
two outer replacements.
[Local window-capacity evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-window-capacity-macos.json).

### Mixed source capacity and transport retirement

The local static relay exercises 32 overlay windows and 32 raw raster tracks from
16 authenticated owners, with repeated owner-local IDs, on a 1024×1024 native target.
Source 65 receives a nonfatal limit refusal. Two outer reconnects preserve retained
pixels, and closing one owner of each kind leaves 56 exact peer sources. GPU
readiness observes actual pixels during asynchronous raster handoff; readback alone
does not settle the 28 pending original vector receipts after cleanup.

Presenter worker admission is `media.max_sources + 64`: the additional bounded slots
cover session control/input and authentication or retiring/replacement transports.
The addition is checked before startup. Queue pressure retains one admitted packet
per media channel without granting success credit; the wait releases the presenter
lock and rechecks exact channel and delivery identity. This applies to ordinary
media and vectors. Recovery cuts retire only selected packets, and clean goodbye
explicitly closes retained raster/image transports while preserving eligible anchored
static data. Socket regressions hold same-ID peer handles through these transitions.
These are bounded static-media checks, not sustained 64-source frame-rate, physical
input or scanout qualification.
[Mixed-capacity evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-mixed-capacity-macos.json).

### Explicit render-scale and canvas qualification

The initial local native geometry fixture passes nine cases through direct hosting and
the static local gateway: minimum/one-pixel axes, 257×513 odd-stride output,
513×257 logical pixels at scale 1.25 (642×322 physical pixels), and full 1024×1024
canvases at scales 1 and 2 (up to 2048×2048 output). Readbacks verify tightly packed
RGBA sizes and interior quadrant samples. Fractional outer-edge alpha is not used
as a readiness signal. Logical pointer coordinates and original scene revisions
survive resize and three automatic outer replacements; a same-address peer on a
separate pane retains its original pending frame through the other owner's close.

Invalid zero/negative/nonfinite/above-eight scales and invalid local worker geometry
are refused before subsequent valid admission. These are explicit host API calls
with synthetic input and display callbacks. The compositor now forwards GUI window
scale with logical extent in one mailbox, retains logical input/candidate geometry
and revokes old image/text grants until fresh admission. Its direct/relay checks
pass simulated GUI scale events at 2, 1.25, 1 and 2 with real Metal buffers, Unicode
and peer isolation. Physical DPI transitions, native candidate menus, monitor
scanout and other platforms remain separate gates.
[GUI scale evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-gui-scale-macos.json).
The compositor also passes its eight-worker saturated-offer fixture at GUI scale 2:
initial/final 130×570 logical canvases produce 260×1140 physical images, while exact
keys, Unicode, logical GPU retry, source replacement, ninth-worker refusal and
retirement remain correct. Source-rate samples stay near the declared 60 records/s
per owner with four receipts. Larger concurrent canvases, aggregate 64-source
saturation and GPU/display peak rates remain open.
[Eight-owner scale evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-capacity-hidpi-macos.json).
[Geometry evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-geometry-macos.json).

### Full target bound and tiled composition

The extended geometry fixture passes thirteen cases through each direct/relay
mode, including downscaling, 4×, fractional 8191×8191 rows and exact 8192×8192
output at scale 8. Four outer replacements include the maximum case while
original revisions, logical pointers and an independent same-address pending peer
remain correct. Oversized host/GPU targets fail before later valid admission;
GPU render/allocation counts and failure state remain unchanged on rejection.

The near-cap run exposed an initial command-storage overflow risk in Vello and
stalled readback/Metal teardown. Large composition now uses one reusable
4096-square scratch texture, at most 64 MiB, with queued copies into the existing
256-MiB-bounded full target. Each copy precedes scratch reuse, and render-pass
metrics count individual tile renders. A focused 8192-square GPU regression verifies
seams, alpha, unchanged-content cache reuse, delayed presentation receipts and
same-ID peer preservation after exact-owner close. Dense/adversarial scene costs,
physical GPU/monitor faults and other platforms remain separate gates.
[Large-target evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-large-target-macos.json).

### Straight alpha and retained assets across tiles

Vello 0.11 writes straight-alpha RGBA. The compositor's registered texture keeps
that alpha type and offscreen readback preserves the bytes without a second
unpremultiplication. A native test verifies partial RGB channels in readback and
registered-texture sampling over an opaque background. The maximum-size owner-close
regression also checks non-primary translucent channels.

The local relay fixture passes nine maximum-size readbacks of asymmetric RGBA
assets crossing both tile seams. Each compares against an untiled reference
(maximum observed channel difference one) and checks 32 known owner texel centers.
Three root replacements exercise displayed/unshown release replay, live assets,
immutable retired frames, delayed presentation and same-ID peer preservation.
Encoded-image mixes, dense scenes, physical faults and other platforms remain gates;
the current post-fix AppKit recheck still requires an unlocked session.
[Tiled asset evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-tiled-assets-macos.json).

### Maximum-target mixed-source recovery

The local compositor also passes the 64-source ceiling at scale eight and
8192-square output: 32 bitmap-bearing vector windows plus 32 raw-raster tracks
from sixteen owners reusing local identities. Two outer replacements preserve
source keys, released bitmap scenes and an unshown live wave. Closing one owner
of each media kind preserves the other 56 sources and 28 pending vector receipts;
only current replay admission settles them. Retired snapshots remain readable
without regaining presentation authority. Source 65 is refused nonfatally, exact
color/gap probes pass and tile-pass metrics reflect four dispatches. The 1×
mixed/window capacity cases still pass. Sustained peak load, dense text/encoded
media mixes, physical faults and other platforms remain separate gates.
[Maximum mixed-target evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-mixed-tiled-macos.json).

### Raw-raster alpha qualification

A mixed-source follow-up exposed premultiplied bytes being interpreted as straight
alpha. Native image/cache identity now retains the configured alpha mode and
selects the corresponding Vello image type without a CPU conversion or wire change.
The final target/readback stays straight alpha. A 64-source fixture passes 1×/8×
output, mixing transparent, quarter-alpha and half-alpha raster tracks with
vector/bitmap windows through four total outer replacements and exact-owner close.
A direct GPU regression checks both modes, transparent deltas and old immutable
pixels while preserving an unrelated same-ID peer.

Ingress validates every premultiplied full-frame sample and delta overwrite before
retained mutation. Eight malformed raw/compressed full/delta cases cover nonzero
RGB at zero alpha and RGB greater than nonzero alpha; late invalid overwrites
cannot admit an earlier valid operation. The offending track loses its channel,
while a valid peer entry in the same prepared frame can still be presented.
Translucent RGB permits one byte of GPU quantization; alpha, opaque colors and
transparent gaps remain exact. Dense content, sustained throughput, physical
platform/device faults and the locked-session AppKit check remain separate gates.
[Raster alpha evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-raster-alpha-macos.json).

### Overlapping retained content at the maximum target

The local relay fixture also combines retained host-shaped glyphs and translucent
bitmap windows from two same-ID owners above a premultiplied raster. Six full
8192-square readbacks match independent untiled renders at matching physical
origins with zero observed RGBA difference; the comparison retains its two-byte
limit. Known alpha blends and distinct opaque glyph colors independently verify
stack order and owner resources. Displayed/unshown released layouts and bitmaps
survive two root replacements. Released layout handles remain unusable for new
drawing; exact-owner close preserves the other owner's pending presentation and
the raster source. Ordinary font jobs are polled during layout/reconnect, as the
production worker requires.

Two differently translated untiled references differ at 34 glyph-edge pixels,
reproducing the original translated-comparison diagnostic. This qualifies aligned
composition and copy behavior, with arbitrary-translation AA identity, broader
fonts/encoded images, dense workloads and physical platform/device behavior still
separate gates.
A repeated native run exposed reversed overlap after reconnect: projection
exported surfaces in hash-map order. The SDK now exports ordinary surfaces in
deterministic identity order, followed by the authoritative pane window stack,
including hidden entries. The additive read-only protocol `Windows::stacked`
iterator exposes that existing order. Gateway reconstruction creates windows in
the supplied order, with parents established before children. The socket
regression covers same-ID owners, raise/lower, hidden windows, five outer
replacements and an unrelated physical peer. Five fresh native processes pass
thirty complete RGBA comparisons across ten root replacements after the fix.

[Overlapping composition evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-composition-macos.json).

The optional `--vector-relay-dense-check PATH` fixture exercises two same-ID
owners with 2,048 spatially distributed rectangular fills each (10,240 path
segments and 200,707 encoded scene bytes per owner). The SDK overlay helper keeps
its existing 256 KiB record ceiling: a valid 4,096-fill scene exceeds that ceiling
and is rejected locally before a later valid revision-one submission succeeds.
Every RGBA byte of six 8192-square readbacks is checked against an independent
calculated grid, including all transparent gaps. Alpha and opaque RGB stay exact;
translucent RGB differs by at most one byte. Two outer replacements, immutable
retired frames, exact-owner close and the surviving peer's pending receipt retain
their admission boundaries. This qualifies the distributed workload, with dense
overdraw, complex curves/clips, renderer arena exhaustion, sustained peak load and
physical platform/device behavior still separate gates.
[Distributed dense-scene evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-dense-macos.json).

The optional mixed-stream checks run all 64 sources continuously: eight vector
owners supply four windows each, and eight raster owners supply four 120-square
tracks each. Every producer offers another group every 20 ms with ordinary flow
and rate limits; vector windows retain at most four outstanding receipt objects.
Three-minute runs at 1× and 8× pass two outer replacements each while all original
source keys remain stable. Every source advances at each five-second checkpoint;
exact owner colors, transparent gaps and full 32-bit markers check every admitted
image, including cache reuse. Marker blocks have flat interiors so the 8× raster
filter does not blend sentinel edges. After sending stops, all final source markers
and original vector presentation revisions settle without resubmission.

This exposed a lost-tail defect: temporary original-presentation quota pressure
was treated as permanent discard. SDK capture now returns `WouldBlock` while
preserving the current pending scene, and the gateway retries only that source's
bounded writer. No layout, revision or channel lock is held while waiting;
supersession and retirement still end the wait. Receipt/byte limits are unchanged.
Focused same-ID peer tests cover quota and aggregate-byte recovery. The developer
fixture keeps ordinary polling, cached-image admission and display service running
while producer sends drain. Per-owner control queries continue under load.
This is bounded, development-build streaming evidence; the offer interval does
not establish guaranteed declared-peak throughput or physical display FPS.
[Mixed streaming evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-mixed-stream-macos.json).

A native receipt-pressure fixture holds four original owner scenes unshown,
observes an actual `WouldBlock`, and displays an updated same-ID peer without
releasing the blocked owner's quota. It separately resumes the authenticated
source lease, replaces a track, and closes/recreates a window. Fresh exact-color
Metal readbacks settle only the corresponding original producer submissions;
retired images retain their pixels but cannot acknowledge the replacement, and
the outer root stays unchanged. The gateway pins capture attempts before its
bounded writer queue and layout work. SDK regressions additionally model channel
advance before cleanup and role recreation with an identical numeric submission;
those two models do not claim duplicate wire acceptance. Display admission is
synthetic. The post-change AppKit check still needs an unlocked session, and
physical input/device and other-platform gates remain unrun.
[Blocked capture replacement evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-pressure-resume-macos.json).

A dedicated authenticated channel-only follow-up now fills the owner's four
receipt slots with distinct window scenes, witnesses a fifth capture waiting,
and displays its same-ID peer while those four originals remain pending. It sends
`ADVANCE_CHANNEL` for just the target track, opens its replacement bulk channel
and rejects the old channel. Source keys, sessions, resume/input-lane generations,
surface identities, peer physical window and outer root stay unchanged. The old
original submissions resolve `Superseded`; only the fresh channel revision and
three unchanged auxiliary scenes reach `Presented`. Native Metal checks five
320×64 stages with exact color/gap probes and full byte equality of the retained
image before and after recovery. Both presenter worker counts reach zero on
teardown. This is native socket/GPU qualification with synthetic display admission;
AppKit, physical device and other-platform gates remain separate.
[Channel-only pressure evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-channel-pressure-macos.json).

The first overlay lane can now open after an already-authenticated gateway's target
has changed. Previously, pre-lane viewport updates remained queued and lane opening
appended the current revision again, causing the SDK's strict revision check to
close the lane. Unsent environment/viewport snapshots now coalesce per owner; lane
opening sends the current environment and viewport once, before the remaining
input and outcomes in their original order. Two-owner queue regressions preserve
same-ID input/outcome bytes through 1,000 changes and lane reopen. Real socket and
Metal checks service three target changes before opening the first lane, then
resize a live composition. Old input is rejected at dispatch; matching fresh
image admission delivers exact original pointer press/release reports without
reaching the peer. Original source/window/root identities stay unchanged, and
retired pixels remain immutable. Display admission is synthetic; physical window
and device gates remain separate.
[Late first-lane evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-late-lane-macos.json).

The pressure fixture now services every current composition while replacement
media primes. Its former all-five-drawings gate could withhold ready windows'
receipts; it also rejected a permitted retained track image before checking display
admission. The corrected loop accepts exact known priming states, requires clear
pixels wherever no drawing exists, and rejects regression after fresh admission.
Original outcomes are checked against the scenes actually admitted. A new visible
role deliberately delays its first media until the other three windows display;
the target stays unacknowledged until its own green frame arrives. Reinstating the
old gate reproduces a five-second, four-drawing stall with capture pressure.
One hundred corrected runs pass all four scenarios, including runs overlapping
the project CPU gates. No deadline or resource limit increased, and production
runtime code is unchanged. The original timeout and isolated retained-image
assertion failures remain in the evidence; the exact original timing was not
reproduced by the diagnostic series. Physical/platform gates remain separate.
[Pressure progress evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-pressure-progress-macos.json).

Accepted unresolved scenes and unsent outcomes now share a 256-receipt budget per
owner. Credits remain charged after records enter the socket writer and retire
only after write completion or cancellation. Deferred non-outcome transitions
retain a separate 256-record bound, with current environment/viewport snapshots
outside those counters. Overflow retires only the affected overlay lane and input;
key/button order and same-ID peer authority stay independent. Authenticated socket
and native Metal regressions cover fail-closed overflow, reserved receipts and
peer pixels. The native path retires the affected window while its same-ID peer
keeps its placement and continues displaying. Physical display and post-change ordinary/AppKit acceptance remain
separate gates. See the [bounded backlog evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-backlog-macos.json).

The developer-only `--vector-relay-curves-check` qualifies a finite curved-path
workload at 8192-square output: 315 commands per owner, 32 nested clips and 256
concentrated translucent fills. Five runs pass 30 full tiled/untiled comparisons
with zero observed difference through relay replacement and owner close. Separate
pixel and compiled-hit probes cover winding holes, reflected/sheared clips and
empty intersections; winding and shear-sign negative controls fail as intended.
The references share the compiled scene and independently exercise untiled
composition. Physical input/display and dynamic GPU arena exhaustion remain open.
See the [curved-path evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-curves-macos.json).

Before checked offscreen rendering, dynamic GPU arena qualification failed. A valid
accepted 512-fill scene (33,795 encoded bytes) returned a stale 1024-square image
as a successful readback while the device reported healthy. Three native
reproductions preserved the previous image byte for byte, including stale peer
pixels. Concurrent control queries completed, and closing the heavy owner restored
peer rendering on the same device.
A direct-Vello diagnostic confirms tile-arena exhaustion; the normal API does not
surface that allocation failure. Those failures are preserved as pre-fix evidence.
Source-isolated work admission remains required.
A separate immediate lease-resume test also failed before a later full-suite pass;
that failure is retained as pre-fix evidence for the retirement repair below. See the [failed arena qualification](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-arena-failure-macos.json).

The offscreen path now reads Vello's actual GPU allocation status before copying a
tile or publishing its cache key. Area-sensitive exhaustion subdivides aligned
tiles, with at most 64 dispatch attempts per output; irreducible allocation or work
refusal retires its partial target and returns an error without an image. An empty
composition after refusal produces fresh transparent output. The 128/512/1024/2048-fill native
regression now checks exact pixels, overlapping peer control, unchanged-image
reuse, the 64-attempt refusal, recovery on the same device, odd dimensions and
pending-validation cancellation/device loss. Readback still grants no display
authority. `vector-offscreen` enables Vello's diagnostic API; construct and poll
`Offscreen` on the render worker because its validation future is not `Send`.
An unlocked-Mac AppKit check also passes real hide/minimize/cover/restore and
minimized exact-owner close with live relayed sources, fresh key/Unicode input
and matching display admission. Native screenshots now require current image
admission after Unicode commit, including the resized surviving peer.

This repairs checked offscreen output, including the compositor worker. The former public synchronous raw-texture `Compositor` and its
submission-only methods have now been removed; shared-device output uses the
checked `TextureRenderer`. Source-isolated handling of irreducible/work-limit
refusal and hostile overdraw through the two-hop relay remain open. The previous
immediate lease-resume rejection is repaired by retirement coordination below.
The earlier gateway suite
also observed `NotConnected` during outer TCP-loss replacement. A later deterministic
regression reproduces macOS repeated shutdown in the test carrier: it aborted before
closing later live sockets. The fixture now accepts only that already-closed result,
preserves other errors, closes remaining sockets and leaves an independent carrier
and replacement connections usable. Fifty final-source runs pass 150 outer replacements,
and four explicit native Metal relay recovery cases pass. This is a test-carrier
repair that did not alter production transport or resume behavior. Physical GPU
faults, scanout, monitor transitions, hardware input/IME and other platforms/devices are
unrun. See the [checked offscreen evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-checked-macos.json).
See the [outer-shutdown fixture evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-outer-shutdown-macos.json).

Immediate resume previously raced the presenter's old control retirement: a
controlled test holds witnessed EOF before suspension and reproduces the same
fatal lease-resume rejection. Before establishment, a valid proof for the exact
active lease/session/generation may now wait for retirement within the original
absolute three-second handshake deadline. The condition wait releases the global
state lock and does not cancel or replace a live owner. Invalid proofs do not park;
competing requests cannot both advance the generation. Normal establishment still
requires suspended state, unexpired grace, matching profiles and valid proof.
Four regressions cover retirement, same-ID peer control/track preservation, invalid
proof, healthy-control timeout and competing attempts. Thirty original high-level
window-adoption runs and thirty witnessed-EOF runs pass; native GPU, direct/relay
lifecycle and real AppKit checks also pass. No wire, lease state machine, key derivation,
producer retry budget or connection limit change is made. Physical network faults
and other native platforms/devices remain separate gates.
See the [resume-retirement evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-resume-retirement-macos.json).

Checked offscreen output now uses a private pinned Vello 0.11.0 backend with
cancellation-safe map callbacks and typed coarse mapped-range errors. It shares
upstream scene identity, so applications keep their original `vello::Scene`.
The explicit native test drops pending coarse validation or debug downloads while
an upstream Vello peer on the same device preserves exact pixels and produces a
fresh image. Health-guard retirement after logical device loss also passes.
Offscreen destruction no longer drains GPU work. `compositor::Error::Backend` is
an added opaque error variant; downstream exhaustive matches must account for it.

The original stock failure and isolated patch results remain preserved. Vivido now
uses a bounded checked-texture worker with independent displayed/ready images and
receipts bound to the exact import in the composed terminal scene. Four finite
Metal cases cover original pixels/receipts, primary progress during a paused job,
coalescing, exact-owner close, same-pool replacement and post-poll deadline
rejection/recovery. Full consumer
recovery/visibility, source-isolated work refusal, ordinary windows, physical GPU
faults and package publication remain open.
See the [asynchronous Vivido evidence](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-async-macos.json).
See the [original cancellation evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-shared-cancellation-macos.json)
and [adopted backend evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-backend-macos.json).


### Native AppKit visibility qualification

The compositor also passes two actual AppKit runs with streaming relay sources.
Application hide, minimize and full occlusion each hold for six seconds, revoking
image/text admission and freezing displayed streams. Restore requires fresh
images, keys and Unicode on unchanged producer processes and relay roots. A
minimized exact-owner close retires the source before peer display; the peer
retains its identity, follows the resized 2× buffer and accepts fresh input.
The fixture uses native visibility and frame callbacks; key/text reports are
synthetic and readbacks verify rendered content. Physical keyboard/IME menus,
secure input, accessibility, monitor/device faults and scanout remain unrun.
[Native AppKit evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-native-window-macos.json).

The compositor's retained high-winding follow-up now covers direct, one-hop and
two-hop ordinary windows at the maximum 4,096-segment scene. Hide, minimize,
occlusion, focus-only loss and activation queued before hide each hold for six
seconds without a producer submission, preserving both original revision-7
receipts. Actual native callbacks restore exact full-image references and current
peer Unicode under fresh delivery epochs; old callbacks remain invalid.

The last case exposed a macOS backend ordering bug. Window activation was queued
while application hide was immediate, allowing an earlier activation to bring
the window back onscreen after the later hide. Application activation and hide
now share the window executor's ordering. Effective visibility/display-link starts
also require a shown, non-minimized window in a non-hidden app, alongside native
occlusion. Independent OS witnesses confirm the original failure and the repaired
hidden holds on all three routes. The fixture never supplies display callbacks.
Broader recovery/load and physical/platform/device/publication gates remain open.
[Retained winding visibility evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-winding-visibility-macos.json).

The same compositor check also retains both maximum-winding originals across
four native content resizes per route. The actual 2× targets alternate between
2,048×1,536, 2,048-square, 2,048×1,792 in a wider window, and 2,048-square.
Complete cropped/full references include every antialiased edge; six one-bit
cropped-edge controls fail. GUI-dispatched pointer motion reaches the original
peer's local hit coordinates after each resize, while old callbacks are rejected
and fresh original-scene Unicode is delivered under new epochs/tokens. Native
callbacks supply frame completion without another producer scene. Finite width
clipping is covered below; held-pointer/busy-worker resize, display-scale/device
changes, arbitrary recovery mixtures and sustained load remain open alongside
physical/platform/device gates.
[Retained winding resize evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-winding-resize-macos.json).

The width follow-up adds 768×1,024, 768-square and 896×1,024 logical crops,
with square restoration on each of the three routes. The original peer remains
at x=960 outside the cropped viewport; bounds, scenes, original receipts and
owners stay fixed. Full comparisons index the unchanged square reference by its
original row stride, including every AA value. Nine one-bit final-column controls
fail. Each native transition changes epoch/token and rejects old callbacks;
restoration returns exact original-peer pointer hits and Unicode. Width settles
before a separate height request to account for toolbar reflow. These are finite
native transitions and synthetic input; held-pointer/busy-worker changes,
physical display-scale/device transitions and arbitrary or sustained recovery/load
remain open alongside physical/platform/device/publication gates.
[Retained winding width evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-winding-width-macos.json).

The actual `vvmux` route now preserves original presentation across IPC. Its hidden
server installs the SDK route before authentication and retains original receipt
objects; VVMX 26 carries only a separate bounded display correlation. Foreground
reports must match the current presenter client, bridge instance, full source key
and exact server-retained copy. SDK completion still fences original writer,
resume, channel and scene lifetime. `OverlayPresentationAttempt::same_original`
compares that complete opaque occurrence for retained/live deduplication; equality
grants no authority and callers still use `capture` for admission. Write/flow ACKs
remain independent. Pending attempts and bytes stay bounded, and retirement
abandons the selected copies without inventing an outcome for a same-ID peer.

The unchanged actual-mux native Metal test now passes, with both originals pending
before explicit display admission, exact pixel readback, and the surviving owner's
repaint pending until its own admission. Socket tests
reject stale/mismatched reports and preserve the peer's window update after
attachment retirement.
This does not qualify ordinary AppKit mux windows, physical input/devices, typed
action/document mux forwarding or broader recovery/load distributions.
[Actual mux original outcomes](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-mux-original-outcomes-macos.json).
The actual mux additionally preserves unshown originals through graceful client
detach/reattach and abrupt fixture-client loss. Public metadata confirms stable
original source/channel identities, hidden server and producer process while
client/bridge instances change. Each detached interval holds pending receipts;
late image callbacks cannot admit a replacement. Native pixel positions follow
two peer window moves, including a repaint after its same-ID owner closes, with
an unchanged compositor pane epoch. Those two finite headless Metal cases do not provide
lease/input-lane, ordinary AppKit mux window, sustained-load or physical-device
qualification.
[Actual mux replacement](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-mux-replacement-macos.json).
Split-process relays can now retain `overlay_relay_input_epoch` on their original
server and use `relay_overlay_input_at_epoch` for atomic forwarding. The opaque
non-authoritative stamp cannot be serialized and does not activate focus or renew
input. Foreign presenters, retired sources, resume/lane changes and incompatible
local host/action routes are refused. The mux publishes only a bounded integration
correlation through VVMX 26. Gateway physical roles retain that correlation until
retired; old input cannot inherit a replacement's stamp. A real SDK socket test
uses matching numeric IDs/epochs on a foreign presenter to verify the stamp's
presenter binding, then preserves its same-ID peer after owner teardown.

Actual macOS mux input-lane replacement now rejects stale pointer/key/text while
preserving peer old-target input and fresh input after explicit focus repaint
admission. The peer's real generic `UPDATE_NODE`, source and native window move
survive the other owner disconnecting; the client/bridge and source channels do
not restart. The frozen old mux fails the same final probe. These are finite
synthetic input/Metal checks. Actual mux lease issuance/resume remains unadopted
(pane lease creation returns `UNSUPPORTED_PROFILE`, code 3), alongside ordinary
AppKit mux windows, document, broader recovery/load and physical gates.
[Actual mux input-lane evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-mux-input-lane-macos.json).
The static-scene follow-up now qualifies direct/actual-mux focus changes and lane
replacement without another producer frame. Original revision-one scenes and
single original outcomes survive three focus changes; stale input is refused,
peer old-target input progresses and a fresh admitted target receives key/text
once. Only test metadata and an ordered read-only native focus observation are
added; existing `input_target`/`text_target` checks remain authoritative. A new
physical role must be distinguished by source identity even when its pixels match
the old role. An already-current focused image needs no artificial repaint.
The peer's real generic node update after owner loss still succeeds. This is
finite synthetic input/Metal evidence; AppKit mux, documents, lease
resume, broader recovery/load and physical/platform/device gates stay open.
[Retained mux focus evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-mux-static-focus-macos.json).
The gateway now supplies the external action-admission half of a split-process
adapter. Retain the original `OverlayActionRequest` at its original presenter;
copy only its non-authoritative owner-window barrier and captured Center bounds.
After the gateway has queried the exact independent physical window, complete
its one-shot claim receipt only with the decision from that original SDK
request's `claim`. Validate reporting client/bridge/correlation and preserve the
original deadline. No original receipt is serialized or recreated from local IDs.
The gateway bounds waits and worker use, and physical role/session retirement
fences late replies. Its socket tests use real SDK claims, all five actions and
same-ID peer node updates, including actual ten-second expiry with live peer
control/input renewals. Decision handoff is in-process. SDK code is unchanged;
actual mux adoption and native external-action qualification were still open at
that checkpoint.
[External action API evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-external-actions-macos.json).

The actual macOS mux now forwards producer-requested Focus, Raise, Lower,
Center and Close through VVMX 26. The hidden server installs the SDK action route
before producer authentication and retains every original opaque request. Only
bounded request/barrier metadata and Q32.32 Center bounds cross IPC. Claims and
outcomes must match the current presenter client, bridge incarnation and request
ID. Attachment replacement retires the original scope; generic host replies cannot
complete a typed action. The aggregate inner window limit is 32, fitting one outer
owner. Admission shares the gateway's 16-global/four-per-owner workers and keeps
control/input/media polling live.

`OverlayActionRequest::remaining_lifetime` observes the original deadline even
after its one-shot claim. The foreground subtracts the entire claim round trip
from the server's remaining lifetime before `ExternalActionClaim::complete_before`;
the worker rechecks that deadline before dispatch. A delayed positive decision
cannot start a fresh lifetime. The real SDK socket regression rejects wrong
client/bridge/request, duplicate claims/replies and retired attachments, and proves
a same-ID peer can still perform a real `UPDATE_NODE` after owner loss.

The direct-versus-actual-mux Metal probe verifies all five actions with exact
native pixels. Focus raises the requested window and receives once-only key/text;
Raise and Lower change overlap while preserving focus. Center uses the original
pane bounds: mux-local `(55,23,16,16)` becomes physical `(56,24,16,16)`, while the
direct host centers at `(504,504,16,16)`. Close removes only the requested role
before its producer transport closes. Both scenes remain revision one with single
original outcomes through these actions; the surviving same-ID owner's subsequent
node update and yellow repaint succeed on the same client/bridge. The final probe
fails at physical Focus on the frozen pre-adoption mux, with its runtime live and
no keys sent, and passes on the new mux.

A copied-executable control exposed a fixture error: its generic node commit
missed a target-generation announcement and was rejected with code 15. The fixture
now consumes authenticated target announcements and replans only an aborted
generic commit once, following SDK `PaneSession`; keys, typed actions, vector media
and successful commits are never replayed. The copied executable passes after
this correction. Redacted diagnostics retain only error kind and presenter code.

This qualifies finite headless Metal checks with synthetic display/key/text events.
Already-claimed typed actions under foreground replacement, mixed saturation and broader
recovery still need native acceptance. Document forwarding, generic/font-only
routes, mux lease issuance/resume (`UNSUPPORTED_PROFILE`, code 3), ordinary AppKit
mux windows, physical focus/IME/secure input/accessibility/scanout, display-scale
and device transitions, other platforms/GPUs/devices and broader sustained/fault
matrices remain open. Restart both mux server and client for private VVMX 26;
Vivid 1.5 wire/profile/registry and dependencies are unchanged.
[Mux window-action evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-mux-window-actions-macos.json).

A queued window action now also passes actual-mux foreground replacement.
Only the fixture-owned client is stopped with SIGSTOP. Public server diagnostics
prove one original Center is pending and unclaimed while its same-ID peer commits
a real node update. SIGKILL and explicit reattachment retire that request; it is
refused, both revision-one scenes replay without repaint, and the old Center does
not move the replacement window. A fresh Center then succeeds with the original
pane geometry. Source/channel identities and the hidden server/producer survive;
after owner loss the peer commits another real node update and native repaint.
The six mux probes and three-route Metal winding regression pass. This qualifies
that queued, unclaimed abrupt-loss case only. Already-claimed RPCs during
replacement, mixed saturation and ordinary AppKit/physical/platform/device gates
remain explicit. No additional feature work is included in this qualification.
[Queued action replacement evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-mux-pending-action-macos.json).


The prior [receipt gap](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-mux-receipt-gap-macos.json)
and [gateway outcome API](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-external-outcomes-macos.json)
checkpoints remain unchanged.


The native TCP fixture is requalified with a connected-socket MSS bound and an
exact 500-ms syscall witness through owner control and a same-ID peer's
receiver-applied scene update.
It verifies live capacity; the earlier nominal 16-KiB buffer request was not a
lifetime bound. Fifty runs pass under overlapping project/GPU checks, with the
original record, queue, credit and cancellation limits. Removing only the MSS
setting reproduces the missing sustained blockage. This is fixture-only IPv4
loopback qualification; hardware, real network faults and other platforms remain
unrun. [Sustained TCP evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-tcp-backpressure-macos.json).

The final direct Vivido ordinary-window fixture now passes on an unlocked Mac.
It verifies exact same-local-ID ownership and input scene revisions, six-second
unmapped/minimized holds with live PTY/control and pending original receipts,
restore, Retina resize, exact captured RGB, isolated key/Unicode input and fresh
peer output after exact-owner close. Independent OS captures include only its
own window; both fixture processes retire cleanly. This finite case does not
qualify broader two-hop hostile-work/recovery mixtures, visible GPU stalls,
physical input/IME, scanout, monitor changes or other devices/platforms. The same
checkpoint removes legacy submission-only compositor APIs; shared-device output
now requires checked completion and immutable import retirement.
[Checked-only and final window evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-checked-only-macos.json).

The compositor worker now uses exact drawing refusal after cooperative native
probes. Real direct/single-local-relay process workers keep their pane epoch,
relay session and last checked peer image through a proven work refusal; the
failed source is Superseded, fresh peer admission and Unicode pass, and owned
processes retire. Successful probe images remain private and cache hits use the
same real-frame publication/warm-up logic as readback. Unattributed compositions
are suppressed without guessing an owner. Aggregate admission, advancing peers
during diagnosis, the Vivido consumer, two-hop/ordinary-window refusal and the
broader physical/platform gates remain open.
[Automatic refusal checkpoint](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-automatic-refusal-macos.json).

The subsequent two-hop host API checkpoint uses two real authenticated relay
runtimes and the production compositor diagnosis primitive with native Metal.
Same-local-ID leaf owners retain independent original receipts: only the proven
bad source closes as Superseded, and the survivor waits for explicit root display
admission. Both relay sessions and old checked peer pixels survive; fresh peer
revisions, exact original-address/scene Unicode and duplicate-free cached
redisplay pass. The driver supplies host display callbacks. Full two-hop process
workers, ordinary-window refusal, advancing peer frames during diagnosis,
aggregate-only failure, Vivido integration and physical/platform gates remain
open.
[Two-hop refusal checkpoint](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-two-hop-refusal-macos.json).

Vivido's checked shared-device worker now separately diagnoses proven work
refusal and closes only an exact still-current source. A direct-admission native
Metal case keeps old checked peer pixels through private probes, then replaces
them with matching checked survivor output in the same draw. Original outcomes,
exact-revision Unicode, another healthy update and the existing device/two-export
pool pass. Its local encoding patch reclaims atlas residency absent from the
complete incoming scene so a full-size immutable replacement is visible on its
first frame. Authenticated hostile-work/recovery mixtures, advancing peers,
ordinary-window refusal, actual aggregate-only/Arena failure, other workspaces
and packaged/physical/platform/device acceptance remain open.
[Vivido refusal checkpoint](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-refusal-macos.json).

Two real authenticated SDK sessions subsequently pass the same native refusal,
including newer peer media accepted after diagnosis begins. Deriving the
original survivor reservation before abandoning the failed mixed frame preserves
its exact pixels/receipt; newer output is admitted separately. Exact-scene
Unicode, cached redisplay without duplicate outcomes, fresh content under the
same refused-owner runtime and zero final sessions/export charges pass. Cold
font initialization now occurs outside the shared overlay lock, preserving
existing input renewal deadlines. This does not qualify continuous peer output
during probes, topology/descendant and broader recovery mixtures, two-hop or
ordinary-window refusal, hardware input/IME or other platforms/devices.
[Authenticated Vivido refusal](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-auth-refusal-macos.json).

The subsequent ordinary macOS Vivido window passes bounded hostile-work refusal
at 2862×1608 with real authenticated producers. Original bad Superseded and peer
presentation outcomes, exact UI-routed key/Unicode, green then magenta peer
pixels and unchanged native window/sessions pass. Own OS captures show the red
source removed. A candidate wake coalesced during rendering previously left the
newer peer pending; ready-image consumption now rearms one check, then becomes
idle when unchanged. Relay/compositor process-worker windows and broader hostile
recovery/visibility, continuous peer output, aggregate/Arena, hardware input/IME,
scanout and other platform/device gates remain open.
[Ordinary Vivido refusal](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-window-refusal-macos.json).

The actual compositor process worker also passes direct, one-relay and two-relay
8192² Metal refusal with two authenticated same-local-ID owners. The second
production relay runs in an owned child process. Actual root work refusal orders
a newer peer submission across independently ordered media connections. Only
the proven bad window closes as Superseded; old peer pixels/control and stable
sessions survive, original peer presentation waits for root display admission,
and exact-scene Unicode plus child/worker/permit retirement pass. This fixture
drives ordered display/input callbacks. Ordinary compositor/two-hop windows,
continuously presented peer frames during diagnosis, hostile recovery/visibility
mixtures, aggregate/Arena failure and physical/platform/device gates remain open.
[Two-hop process-worker checkpoint](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-two-hop-process-refusal-macos.json).

The ordinary compositor macOS window now passes the bounded refusal case directly
and through one and two real relays, using actual AppKit frame callbacks. The
existing 1024² logical limit and real 2× display produce a checked 2048² image.
Only the hostile source closes as Superseded; the original green peer receipt,
exact-scene synthetic key/Unicode, later magenta content and child retirement
pass with unchanged runtime/relay identities. Own-window OS captures agree.
Real native activation and fresh image admission precede input. An earlier
activation-restore timeout remains recorded; controlled static-scene focus and
visibility recovery and recurrence qualification are still open. Broader hostile
recovery mixtures, continuous peer presentation, aggregate/Arena, physical
input/IME/scanout and other platform/device gates remain explicit.
[Ordinary compositor refusal](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-window-refusal-macos.json).

The subsequent ordinary-window fixture controls both full occlusion and native
focus loss while visible around the unchanged revision-three survivor. Each
hold lasts six seconds. Display/input admission is revoked under full cover;
a visible inactive window retains current native presentation. Old key/text
callbacks remain rejected after restoration, and a new native delivery precedes
current exact-scene input without a producer repaint. Direct, one-relay and
three two-relay runs pass ten transitions, original receipts/pixels/control,
unchanged runtime/session/window identity, fresh magenta content and retirement.
The earlier uncontrolled activation timeout is not reproduced or explained by
these finite passes; cause/recurrence and broader hostile recovery, aggregate/
Arena, continuous peer output and physical/platform/device gates remain open.
[Static-peer native recovery](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-static-restore-macos.json).

The separate cross-application check passes on final source directly and through
one/two relays. An independent owned native helper receives no Vivid credentials;
readonly OS PID queries verify its foreground identity during a six-second
unchanged-peer hold and root foreground restoration while the helper is alive.
Original revision-three bytes/receipt, stale-callback rejection, fresh current
input, later magenta output and retirement pass with original session/window
identity. A prior interruption is observed live as Chrome takes foreground:
root input remains inactive and the original peer stays presented and healthy.
That explains the later interruption, not the older uninstrumented timeout.
Arbitrary foreground contention and broader recovery/platform/device gates stay
explicit.
[Cross-application evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-app-restore-macos.json).

The compositor's native component matrix also resumes either the hostile leased
owner or its healthy same-ID peer while production work-refusal diagnosis has a
private job pending. Both orders pass directly and through one/two relays.
Stale refusal cannot close the recovered incarnation; resuming only the peer
still permits exact refusal of the unchanged offender. Old input/display bindings
are rejected, generation-two media retains logical IDs, and original unaffected
receipts remain pending until fresh checked display admission. Old receipts fail;
a preserved old `Superseded` event is validated separately by exact submission.
Renewed-lane Unicode, an ordered cached-display barrier, relay-session identity
and GPU health pass. These are native Metal readbacks with explicit host display
callbacks, not ordinary-window or physical-input acceptance. Standalone lane
replacement without source resume has separate coverage below; broader
recovery/platform/device gates remain open.
[Resume during diagnosis evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-diagnosis-resume-macos.json).

Standalone canonical input-lane replacement during diagnosis also passes both
owner orders directly and through one/two relays. Input generation advances to
two while authenticated control/resume identity and media generation one stay
live. Old input/renewal/reopen are rejected. Direct unchanged media keeps its
proof; relay remapping retires the affected physical attachment and requires
fresh checked refusal of unchanged hostile bytes. The original peer receipt
remains pending through private jobs and resolves only with checked green output.
Exact original outcomes, live track queries, renewal past the initial watchdog,
magenta output and current Unicode/cached-display barrier pass. This is native
offscreen component coverage with explicit host display admission; ordinary
windows, repeated/arbitrary recovery mixtures, continuous peer display and
physical/platform/device gates remain open.
[Lane replacement during diagnosis evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-diagnosis-lane-macos.json).
