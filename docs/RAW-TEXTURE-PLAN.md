# Checked raw-texture integration

The SDK provides checked shared-device immutable GPU output behind
`vector-texture`. Vivido now uses it on a dedicated bounded overlay worker,
retains displayed output independently and binds admission to the actual imported
image in the composed terminal scene. The legacy public synchronous compositor
and obsolete tile dispatch have been removed. Source-isolated refusal and broader
consumer recovery/visibility qualification remain work. Do not complete those gates from finite primitive
or consumer tests.

## Implemented SDK primitive

`compositor::texture::{SharedDevice, TexturePool, TextureRenderer}` creates no
adapter or event loop and installs no device callbacks. Construct and poll the
renderer on its worker; its validation future is not `Send`. The shared device
uses a host-provided failure observer, checked before future polling and output
publication. The offscreen and texture paths call the same checked tile renderer,
including aligned subdivision and the 64-attempt work bound.

`start(drawings, rasters, width, height)` returns an opaque `JobId`. Keep the exact
prepared snapshot with that ID. `poll` returns `Completed` only after checked
rendering and a copy into a distinct immutable RGBA8 export finish. A cache hit
can reuse an already-complete export; if its last reference retired, the working
texture is copied without rerendering. Empty output is freshly transparent.
GPU completion still grants no Vivid display or input authority.

Reuse one `TexturePool` across replacements. It allows two charged outputs and
one live/retiring worker. Pending jobs reserve output credit before target
allocation; clones and registered imports retain it. Cancellation drops the
future and waits asynchronously for a queue fence before working-target reuse.
Dropping a worker keeps its pool claim until submitted GPU work retires. Neither
cancellation nor a local deadline destroys the host device or changes host health.
Keep servicing device callbacks while resources retire, including during recovery.

`Texture::register` returns `TextureImport` for the original upstream Vello
renderer on the same device. Keep it while drawing `import.image()`. Explicitly
`unregister` after its last sampling submission; this invalidates all import
clones and fences source allocation retirement behind submitted work. Vello's
atlas retains image metadata, so merely dropping import guards is insufficient.
Unregistered metadata releases the source charge even while its cached image
record survives. Raw image clones are invalid after unregister.

Native macOS tests use two authenticated owners reusing local IDs. They check
straight alpha, every retained pixel across replacement, both cache paths,
original receipts, stale admission after owner close, a full output pool, hostile
2048-fill output, exact 64-attempt refusal, transparent recovery and odd sizes.
A held render worker permits independent upstream rendering and owner control;
both map-stage cancellation and a local timeout preserve the device. Explicit
logical destruction is observed before another fine submission. These are GPU
readbacks and synthetic display admission, not a migrated terminal/window path.
[SDK texture evidence](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-texture-macos.json).

## Required design

`NativeVectorHost::prepare_retaining` now provides exact committed-window reuse
with fresh peer receipts, bounded to a third diagnostic handoff frame within the
unchanged host-wide ceiling. Native Metal/API tests retain released image
resources, advance four same-ID peer scenes with exact outcomes/Unicode and
leave the selected owner's pending replacement unresolved. This is a composition
prerequisite. The local compositor now integrates one forked private target on
the same device/queue/health, with shared dispatch admission across private and
output jobs. Eight paced peer revisions display during actual hostile diagnosis
on all three process paths within the unchanged 127-attempt/ten-second bounds.
Exact source-proof refresh preserves changed scenes, tracks and resumed owners.
The ordinary two-hop window companion passes; continuous ordinary-window and
Vivido/shared-device-consumer integration, arbitrary hostile/rate/aggregate
mixtures and physical/platform acceptance remain required.
[Retained-scene evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-retaining-macos.json).
[Interleaved peer evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-interleave-macos.json).

`TextureRenderer::start_budgeted` now shares the same dispatch allowance across
cooperating immutable-texture jobs. Vivido integrates one private target beside
its output target, on the same worker/device/queue/health. Exact latest-admitted
retained snapshots advance peers within its unchanged two-reservation and
public-export limits. Actual primary-scene sampling admits each peer before the
private probe proceeds. Eight native peer imports and a late healthy-source
replacement pass the finite component cases. A later finite authenticated burst
adds eight original wire submissions during actual private diagnosis, with exact
receipts, pixels and Unicode; sustained streams, ordinary-window overlap and
broader recovery/rate/aggregate mixtures remain open.

A reproduced Metal GPU address fault exposed unchecked Vello shader accesses
following arena overflow. The private backend now uses default WGPU runtime
checks and discards failed coarse work before fine dispatch. The SDK rejects a
successful zeroed allocator download, since every checked composition includes
a nonempty clip. Tiling, allocator sizes, deadlines and work limits are unchanged.
This does not qualify generic Metal command-error propagation/recovery or physical
GPU faults; the observed failure and bounded correction are recorded separately.
[Shared-device interleaving evidence](../../vivid_compositor/docs/performance/2026-11-15-vivido-vector-interleave-macos.json).
Near-limit qualification exposed packed eight-bit winding overflow in the checked
MSAA8 shader. The original 127/128-contour failure and rejected analytic-AA
candidate remain recorded. The pinned backend now uses full-width counters for
nonzero fills with at least 128 tile segments, preserving the original MSAA8
coverage lookup table, fast/parity algorithms, bindings and dispatch limits.
Its 9,552-byte shared storage fits below the existing coarse shader requirement.
The offline generator and patch pin the original provider hash; no registry or
application-startup shader rewriting is used.

All eight native arena cases pass, including maximum cubic/clip/segment pressure,
four-owner isolation, full reference pixels across six winding counts and signed
cancellation/parity. Native textures, presenter/raster/image consumers, Vivido
recovery and ordinary windows, and compositor curve/dense/composition and
refusal/peer-stream paths pass separately. The old ordinary window reproduced
6,461 wrong outside pixels after original receipts were Presented; the repaired
window classifies over four million pixels with zero mismatches at each count.
These finite cases do not close broader sustained/source-kind/aggregate recovery
or physical/platform/device gates.
[Original failure](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-winding-failure-macos.json).
[Wide-winding repair evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-wide-winding-macos.json).
[Authenticated burst and completion-wait evidence](../../vivid_compositor/docs/performance/2026-11-15-vivido-vector-stream-poll-macos.json).
A finite ordinary AppKit follow-up also admits eight original peer revisions
during private diagnosis with exact native target pixels and scene-bound text.
Two final-source runs use 72 credits each; the observer is opt-in, bounded and
absent from normal builds. Sustained rates and native recovery overlaps remain
open. [Ordinary Vivido streaming evidence](../../vivid_compositor/docs/performance/2026-11-15-vivido-vector-window-stream-macos.json).
Native hide/minimize and exact even resize during actual private work now pass
with healthy same-owner replacements, original receipts, peer control/input and
exact pixels. A separate component fixture expires the actual ten-second
consumer clock with completed primary readbacks and fresh recovery. These finite
cases do not qualify persistent hostile bytes across arbitrary native recovery
or physical GPU hangs. [Window recovery and elapsed-stall evidence](../../vivid_compositor/docs/performance/2026-11-15-vivido-vector-window-recovery-macos.json).
Four further ordinary-window cases retain the exact scene, bounds and original
receipt across hide, minimize and resize. Fresh checked work isolates the same
offender after normal hidden GPU release or a large retarget, preserving both
sessions/tracks and the healthy peer. A smaller target legitimately completes
the same original, with exact black/green pixels and input; prior work refusal
does not blacklist that scene for every target. Arbitrary repeated recovery,
native lease/input/epoch/device overlaps, uninterrupted retarget visibility and
sustained/physical/platform gates remain open.
[Retained-scene recovery evidence](../../vivid_compositor/docs/performance/2026-11-15-vivido-vector-retained-recovery-macos.json).
Two ordinary-window cases also resume a real leased source during actual private
work, replacing its input lane and advancing only its media channel. Complete
logical identity and the same-ID peer remain intact. Old pending receipts retire,
focus requires fresh output, stale cancellation cannot kill the resumed owner,
and exact fresh pixels, receipts and input pass. A hidden variant crosses normal
GPU release with both fresh originals pending. Standalone lane takeover, media-
epoch/descendant/device loss, arbitrary repeated recovery and sustained/physical/
platform combinations remain open.
[Native leased recovery evidence](../../vivid_compositor/docs/performance/2026-11-15-vivido-vector-leased-recovery-macos.json).

Use a worker-owned renderer for checked overlay jobs. Vivido's existing primary
Vello renderer is shared across windows and must remain available for terminal
text, media and unrelated panes while an overlay waits for GPU validation. Create
and poll the diagnostic future on its worker; it is not `Send` in Vello 0.11.

Keep one pending job and coalesce later candidates rather than queuing every
update. Retain the exact compiled drawings and display receipt with each job.
Check all GPU allocation flags, retain the existing 64-attempt work bound and
16-pixel-aligned subdivision, and return no successful image on irreducible or
work-budget failure. A source-isolated refusal policy is still required.

Publish an immutable RGBA8 texture only after checked rendering and the output
copy complete. Copy the verified working target into a distinct export; a later
job must not mutate a texture currently sampled by the primary renderer. This is
a GPU copy, not CPU readback. Imports use the host's same logical device and
straight-alpha registration. Keep at most two retained exports plus one bounded
working target/scratch; preflight sizes and export leases before allocation.
Host-held clones/imports must retain their allocation charge. A full export pool
returns `WouldBlock` instead of accumulating textures.

A shared-device constructor must use the host's health observer without replacing
its error or loss callbacks. Check health before polling a pending future and
before publication. Job timeout, cancellation, hide or close must retire only that
job; they must not destroy the shared logical device. The
[pinned cancellation backend](upstream/vello-0.11-cancellation.md) is adopted for
checked offscreen output. Its native regression cancels both map stages while an
upstream peer retains and refreshes exact pixels. The offscreen destructor no
longer drains the GPU. The terminal's primary renderer stays upstream Vello; untrusted overlay paths are
now checked on their own worker and reach it as one completed immutable image.

## Vivido migration

`src/display/overlay.rs` now uses a dedicated worker that creates and polls the
non-`Send` checked texture renderer on that same thread. A command slot and a
result slot each hold at most one job. Later candidates are coalesced in the
existing source state until the job completes. The persistent per-pane
`TexturePool` retains the two-export and one-live/retiring-worker limits through
clear and renderer rebuild. Imports are explicitly unregistered after their last
sampling submission; queue fences retain their charges while GPU work retires.
The worker observes the original host health callbacks and never destroys the
shared device. Cancellation signals the owning worker; the terminal thread does
not join it. A retiring worker services its old device's callbacks even after a
host rebuild moves to a different device. Idle workers block on their command
channel; pending work uses a cooperative poll interval and the original absolute
ten-second job limit. The existing frame scheduler keeps terminal polling bounded
and reuses cached terminal scenes while the overlay image is unchanged.

Vivido reserves at most two explicit immutable snapshots. Each request retains
its original snapshot beside an opaque request identity mapped to the SDK's exact
job identity. A completed result is checked for original request, dimensions and
full source/window/target admission before import. The ready image and displayed
image remain independent. Final composition inspects the actual Vello image
resource reference: only that exact live import may publish its reserved hits
and original receipts. Blank or older cached terminal scenes cannot acknowledge
a ready replacement. Admission rejects retired bindings and a successful image
replaces the complete hit-test set. Moving an overlay creates a new immutable
image generation, invalidating terminal scene caches.

The initial reservation prerequisite and its controlled old-code failures remain
in [reservation evidence](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-reservations-macos.json).
The new finite Metal cases cover original pixel/receipt identity, a separately
reserved replacement, blank-frame refusal, eight primary frames during a worker
paused after a pending GPU poll, coalescing later candidates, a two-output limit,
exact-owner close, nonblocking worker retirement and fresh same-pool peer output.
A controlled expiration after real GPU completion rejects the result before
publication and preserves original pixels/receipts; a changed candidate recovers
on the same device. Removing only the post-poll checks reproduces failure.
The real SDK socket fixture now waits for actual completed output and its original
presentation receipt, rather than assuming preparation completes synchronously.
Python blocking/async, Node and Lua socket producers also pass on the installed
bindings (Node 24 is selected explicitly because the default Homebrew binary has
a missing dylib). The existing 240-frame movement bound passes with checked
completion and final composition included: p95 15.389 ms against 16.667 ms,
maximum 18.745 ms; this is a finite single-window measurement.
See [asynchronous consumer evidence](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-async-macos.json).

Additional offscreen native consumer cases now qualify channel advance/rebind,
retained-session suspension followed by rebind, active-slot replacement and
97×33 retargeting with pending or already-complete output. They reject stale
publication, preserve exact peer state/pixels and settle fresh current receipts
once. Actual logical WGPU device destruction during a pending job refuses old
render/readback, then rebuilds and produces two fresh updates against the same
pool. The original meter observes two charged outputs while pending, one after
each recovered display and zero after clear. An independent renderer on another
logical device keeps its original pixels and renders fresh colors throughout.
See [lifecycle evidence](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-recovery-macos.json).

Those lifecycle fixtures drive registered scene methods directly. A subsequent
[authenticated wire checkpoint](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-wire-resume-macos.json)
uses real leased SDK overlay sessions and channel-open proofs while a checked
Metal job is pending. Both quiescent and immediate resume pass; the immediate
case holds prior retirement, observes a waiting valid handshake and proves peer
control progress before release. Suspension retains metadata and accepted floors,
closes old media/input transports, revokes stale hits and requires fresh output
before input. Exact runtime checks reject late old-lane cleanup when a resumed
runtime reuses lane generation one. Invalid proofs do not wait, and a valid proof
cannot take over a healthy control or exceed the original handshake deadline.
Fresh generation-two images/receipts and committed Unicode input pass with a
same-ID peer. A model regression covers revealing a stale retained hidden window.
Still exercise actual hide/minimize/resize visibility, elapsed-time stalls and
ordinary-window recovery. The logical-device case is not physical GPU removal or a proof that
other renderers on the destroyed logical device should remain healthy. Source-isolated irreducible/work-limit refusal
remains essential: an invalid mixed image is withdrawn in full while a fresh
composition is prepared, and a failed unchanged composition is not retried on
every terminal frame. This is not yet a guarantee that an unrelated source keeps
visible pixels and advances through another source's irreducible workload.
The primary terminal renderer's final submission is not a physical scanout or
GPU-fault guarantee.

The caller audit found only one remaining legacy use, an ignored Vivido GPU
regression. It now starts and polls checked jobs, samples immutable imports and
explicitly retires their charges while preserving the original clipping,
same-ID owner, cache, resize, rejection and retained-text assertions. The SDK's
public `compositor::Compositor` (including `generation`, `metrics`, `clear`,
`render` and `render_mixed`) and `Error::Render` have been removed. Private working
textures are never registered as mutable images; CPU readback and immutable
texture export share one checked implementation. Existing `TextureImport`
registration on the host's primary renderer remains required. The offscreen
alpha regression now recomposites its public readback bytes, replacing an
internal mutable-target reference. Native texture tests retain direct GPU import
and straight-alpha sampling coverage.

See the [checked-only checkpoint](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-checked-only-macos.json).
This API cleanup does not qualify source-isolated refusal or physical platforms.

## Exact drawing refusal

`NativeVectorHost::reject_drawing(&frame, drawing.window)` closes one prepared
window after the host has isolated an actual rendering failure to that drawing.
It checks the attachment, pane target/visibility, exact latest retained bytes,
window placement, track/channel, resume generation and original writer before
mutation. A peer may change placement independently. Late refusal cannot close a
healthy replacement scene, track or adopted resumed window, and an already-shown
frame cannot be refused. Ordinary descendant close policy still applies.

The refused frame grants no presentation proof. Its same-ID peer keeps its
original pending receipt and compiled resources until a new checked composition
is actually displayed. Socket regressions include independently retained images,
concurrent peer placement and stale/foreign/displayed/retargeted/hidden frames.
A native Metal test renders only the offending 2048-fill drawing at an 8192²
target, observes the exact 64-attempt work refusal while peer control progresses,
then refuses that drawing and renders fresh transparent/blue peer output on the
same device. This is explicit attribution and API recovery, not automatic mixed
frame diagnosis, uninterrupted frontend peer publication or a two-hop relay.

Never pick a source merely because its mixed composition failed. The compositor
CPU-readback frontend now cooperatively probes individual drawings and refuses
only a proven failure. Probe pixels stay private, the previous GUI image remains
retained, and exact peer receipts resume on a fresh real composition. Probe starts
have bounded work/time admission; unidentified failures suppress the unchanged
candidate without closing any owner. Direct and single-local-relay process-worker
cases pass, including unchanged pane/relay incarnation, Unicode and cleanup.
This does not qualify aggregate-capacity admission, continuous independent peer
advancement during diagnosis, the Vivido consumer, two-hop or ordinary-window
refusal. Those parent gates remain open.
[Automatic compositor refusal](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-automatic-refusal-macos.json).
[Exact refusal checkpoint](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-drawing-refusal-macos.json).

The subsequent host API case qualifies the production diagnosis primitive through
two real authenticated relay runtimes on native Metal. Original same-local-ID
leaf receipts, exact bad-owner refusal, retained checked peer pixels and relay
sessions, fresh peer output/input and duplicate-free cached redisplay pass.
Display callbacks are supplied by the driver; this does not qualify two-hop
process workers or ordinary-window refusal, independently advancing peers during
diagnosis, aggregate-only failure, Vivido integration or physical platforms.
[Two-hop refusal checkpoint](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-two-hop-refusal-macos.json).

Vivido's production shared-device worker now separately isolates typed work
refusal with bounded private probes. Exact current-source host fences protect
replacement scenes and incarnations; a matching checked survivor composition can
replace the mixed image in the same draw. A direct-admission 8192² Metal case
passes retained blue peer pixels/primary progress, original bad Superseded and
pending-until-sampled peer receipts, green survivor output, exact-revision
Unicode and a later magenta update on the same device/two-export pool. A pinned
Vivido-only encoding patch reclaims recent atlas residency absent from the
complete incoming frame, protecting current images and preserving the atlas
bound. This does not qualify other workspaces/packages, authenticated hostile
mixtures, advancing peers during diagnosis, aggregate-only/Arena failure,
ordinary-window refusal or physical/platform/device acceptance.
[Vivido refusal checkpoint](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-refusal-macos.json).

The subsequent authenticated Vivido case uses two real SDK sessions and accepts
newer peer media after native diagnosis begins. A derived original survivor
reservation preserves checked pixels and their original receipt before the
failed mixed reservation retires; the newer image is admitted separately.
Topology/placement/target/binding fences still apply. Original outcomes,
exact-scene Unicode, duplicate-free cached redisplay, fresh content under the
same refused-owner runtime and zero final sessions/exports pass. Cold vector
worker font construction no longer holds the shared host lock, preserving input
renewal deadlines. Continuous peer output during probes, broader topology and
recovery mixtures, two-hop/ordinary-window refusal and platform/device gates
remain open.
[Authenticated Vivido refusal](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-auth-refusal-macos.json).

The ordinary local macOS Vivido window now passes actual hostile-work refusal
at 2862×1608 with two authenticated same-ID producers. A compact 4095-fill scene
plus one hit respects existing limits; original bad Superseded and peer
presentation outcomes, exact key/Unicode, green then magenta output and stable
native window/sessions pass. OS captures show the bad source removed. A newer
peer previously stalled after its wake was coalesced during rendering; one
candidate check after ready-image consumption now preserves progress and returns
to idle. Relay/process-worker windows, broader hostile recovery/visibility,
continuous peer advancement during probes, aggregate/Arena and physical/platform
gates remain open.
[Ordinary Vivido refusal](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-window-refusal-macos.json).

## Qualification still required

- Shared-device cancellation passes with the adopted SDK checked backend and an
  upstream renderer peer. Vivido uses that backend now; its broader asynchronous
  lifecycle and ordinary-window qualification remain open.
- SDK raw texture sampling covers hostile accepted overdraw, alpha, odd sizes,
  aligned subdivision, cache reuse, work refusal and empty output after refusal.
  Repeat those assertions through the migrated Vivido consumer.
- With two same-ID Vivid owners, verify retained snapshots, pending original
  receipts, source-isolated rejection, close/resume/channel replacement and stale
  completion. SDK texture tests cover original receipts, same-ID owner close and
  stale admission. Vivido manager reservations now cover same-ID ownership,
  original receipts and stale binding/placement refusal. Finite asynchronous
  consumer cases now cover pending jobs, exact image admission, owner-close
  replacement, direct scene rebinding/retargeting and logical-device rebuild.
  Finite authenticated wire resume/channel opening with pending jobs now passes,
  including deliberately overlapping control retirement. A direct ordinary
  window now passes below; broader recovery/source/visibility mixtures remain open.
- Verify terminal rendering, input, another pane and control remain responsive
  while one overlay job is slow or saturated. Exercise cancellation and logical
  device loss without invalidating a healthy shared-device peer.
- Run SDK and Vivido required project gates and meaningful native GPU/window
  checks. Preserve unrelated Vivido lockfile edits. Other devices/platforms,
  physical GPU faults and the documented display/input gates remain unrun.

The same checkpoint rebuilds the current compositor after aligning its Apple font
and accessibility bridge dependencies with their existing native public types.
Real AppKit hide/minimize/cover/restore, stale key/text rejection, fresh Unicode
and peer-preserving minimized close pass. A separately built ordinary sample with
no testing features passes own-window capture, all three owner-focus switches and
local-control cleanup. These GUI cases exercise the compositor's SDK vector host,
not Vivido's migrated checked overlay consumer. The direct Vivido case below
qualifies that consumer's ordinary window separately.

The [ordinary Vivido follow-up](../../vivid_compositor/docs/performance/2026-11-14-vivido-vector-native-window-macos.json)
finds and fixes queued redraw admission after explicit native unmapping. The
central draw path rejects unmapped/minimized windows without consuming damage or
settling overlay receipts; unmapping revokes overlay focus, and macOS reveal
restores a miniaturized native window before ordering it. An unlocked prototype
passes six-second hidden/minimized holds, continuing PTY/control progress,
original-receipt restore, Retina resize, exact captured RGB and isolated key/text
input with a surviving same-ID peer. These are actual windowed consumer paths.
The final discoverable test now passes on an unlocked session with explicit
same-local-ID owner/input scene-revision assertions, OS captures of only its own
window and clean retirement of both fixture processes. See the
[checked-only/window checkpoint](../../vivid_compositor/docs/performance/2026-11-14-sdk-vector-checked-only-macos.json). Six-second hidden holds do not prove visible GPU
stalls, physical scanout/device loss or the broader hostile-work isolation gate.
The same follow-up repairs the two GUI test observation errors and passes its
current native-feature workspace checks; earlier failures remain historical.

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

An additional component matrix passes authenticated source resume while a private
work-refusal diagnosis job is pending. Resuming either the hostile owner or its
healthy same-ID peer passes directly and through one/two relays: stale proof and
input are rejected, only the unchanged offender can close, original unaffected
receipts require fresh checked display admission, and generation-two media plus
renewed-lane Unicode preserve logical identity and relay sessions. Retired
receipts remain unusable; any preserved old terminal outcome is checked by exact
submission. This uses native Metal and explicit host display callbacks. It does
not qualify ordinary-window recovery mixtures, standalone lane replacement,
continuous independent peer presentation or physical/platform/device behavior.
[Resume during diagnosis evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-diagnosis-resume-macos.json).

The corresponding standalone lane-replacement matrix now passes both owner
orders across all three local paths while control/resume/media identities stay
live. Input generation two rejects old authority; relays remap only that owner's
physical attachment, so unchanged hostile content must receive fresh checked
refusal before the new window can close. Original peer receipts survive private
diagnosis and require checked display, followed by current magenta output and
exact renewed Unicode. This adds native component evidence, not ordinary-window,
continuous independent peer, arbitrary outage or physical/platform acceptance.
[Lane replacement during diagnosis evidence](../../vivid_compositor/docs/performance/2026-11-15-sdk-vector-diagnosis-lane-macos.json).
