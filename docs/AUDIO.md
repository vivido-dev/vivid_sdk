# Native audio services

Enable `audio-decode` to use `vivid_sdk::audio::{Decoder, Limits, CaptureResampler}`.
It shares Vivido's native audio decoder without pulling in its terminal, GUI,
CPAL device output or playback clock. Default producer builds retain their
existing dependencies.

Create a decoder on a dedicated media worker after authenticating and admitting
the track. `Decoder::new` validates canonical codec initialization and converts
decoded output to the requested interleaved float format. `with_limits` selects
finite conversion, returned-batch, frame and thread budgets. `push` consumes one
packet; `finish` drains decoder and resampler once. Returned samples own their
storage. Retire the decoder after an error.

`push_timed` and `finish_timed` return owned `DecodedBatch` values whose bounded
`DecodedSpan` ranges cover all samples. Each span carries an actual decoded-frame
PTS origin and an exact output-frame offset. Resampler tails retain their old
timeline across gaps/overlaps. Trim advances the visible range without changing
its origin; final padding follows filter drain. Packet trim boundaries restart
conversion after preserving the old tail. Unknown native timestamps remain
explicit, and timestamp/serial exhaustion retires the decoder. Reserve at most
`max_output_frames * size_of::<DecodedSpan>()` beyond sample scratch and output.

Input follows Vivid's canonical audio configuration. Output supports 8–384 kHz
and one through 32 channels. The decoder checks packet size, pending trim,
native format changes, cumulative output and nonfinite samples. It sets native
`max_samples` before opening; FFmpeg checks that bound across all channels when
allocating audio frames. See the [upstream allocation guard](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/decode.c).
Codec-internal allocations and native call duration remain additional. Hosts
must reserve aggregate resources and keep control independent of native work.
Batch and scratch storage can coexist, so reserve up to twice the configured
sample limit for conversion, plus native state and host-retained samples.

Rate and channel layout come from the actual returned frame through FFmpeg's
frame API, using a reusable metadata-only context. Matching codec-context
metadata cannot hide a different frame. No version-dependent frame-tail offset
is guessed in Rust.

`CaptureResampler` provides the existing microphone conversion from mono float
to 48 kHz signed-16 samples, with caller-owned output storage and explicit
capacity checks. It creates no microphone and grants no capture authority.

See the [support crate contract](../audio/README.md) for limits, native libraries,
codec fixtures and provenance. Native hosting is an independent opt-in
integration described below. Matching Live or Timed A/V uses the active audio
clock under the contracts below. Synchronized playback remains unfinished.

## Device output and clocks

Independent `audio-output` exposes `vivid_sdk::audio_output::{Output, Format,
Limits}` without decoding, GUI or terminal dependencies. Hosts can use a different
decoder, or combine it with `audio-decode`. Vivido uses both shared services.

Open and retire device streams on a dedicated worker. Reserve aggregate resources
first, then use `open_with_limits` to enforce the admitted ring ceiling. Acquire
`Output::channel` for an exact instance/generation capability. Blocking pushes
remain source-local; advancing the generation or stopping the output cancels old
writers. STOP is permanent. All submitted samples must be finite whole frames.

`validate_play` supports atomic host admission. `configure_play` sets exact timed
startup with a reachable prebuffer, `start` enables consumption, and `pause`
freezes it. Timed underrun silence advances the audio master clock; late samples
are discarded. Live capture keeps its separate underrun/reserve policy. The
callback consumes only whole interleaved frames and rounds silence/trim durations
to preserve channel alignment. EOS, decoder completion and actual device-ring
consumption are separate observations; `poll_drained` is nonblocking.

`OutputControl` carries only shared controls and observations. It cannot keep a
native stream alive or destroy one on its caller's thread. `Channel::try_push`
supports bounded, whole-frame prefix writes without waiting for ring capacity.
See the [output contract](../audio-output/README.md) for limits, worker/lifetime
requirements and optional `audio-output-testing` fixtures.

PAUSE disables future consumption without waiting for an already admitted device
callback. `OutputControl::paused_clock` returns a callback-quiescent `PausedClock`
only after final sample/clock accounting is committed. Its optional PTS preserves
an unknown timeline origin. Keep the output paused while observing it; starting
or reconfiguring invalidates that observation. This is a stream-free atomic
query, not a native-device wait or proof of acoustic playout.

## Native live hosting

With `presenter,vector-audio`, call `NativeVectorHost::enable_audio` before
producer authentication. Audio is off until that explicit opt-in. Per-source
workers own FFmpeg and CPAL, with a one-entry job queue and reservations covering
two conversion allocations, a bounded ring and packet storage. Authenticated
owner/context/surface/track/channel, writer, media epoch, resume generation,
surface generation and reservation incarnation follow admitted work. Retirement
stops the old output; a replacement timeline gets a fresh output instance.

The live path places PCM by decoded timing rather than packet duration. The first
audible range anchors its output clock, including codec priming and protocol trim.
Gaps insert bounded chunks of zero frames into the same ring after earlier PCM;
overlaps discard only the overlapping new prefix, preserving already queued
sound. Whole old ranges are discarded. Exact frame arithmetic avoids cumulative
rounding; independent origins are rounded to the nearest device frame. Unknown
initial decoded timing is refused; later untimestamped frames can continue an
already anchored decode timeline. Packet duration may be unknown.

`configure_timestamped_live` preserves this ordered queue through startup and
holds the live clock on underrun. A long gap streams through finite ring/chunk
storage, parking only its source and checking retirement between writes. Timing
metadata and the silence chunk are included in admission reservations. Linked
Live A/V follows the additional contract below. Synchronized playback remains
implementation work. The feature stays experimental and off by default.

The compositor's `examples/audio.toml` supplies a finite PCM vector application
sample, with native GPU acceptance through `--audio-render-check`. It covers
pointer/keyboard activation, EOS consumption, in-flight FLUSH, channel advance
after EOS, owner isolation, an active-audio process crash, restart and retirement.
Metal runs against the real default macOS device; an explicitly selected
`audio-testing` build uses the clocked production callback fixture, including
Linux software Vulkan. GUI input is simulated. These checks do not establish
physical keyboard routing or physical Linux audio output.

Activation gates device consumption. Output-ready follows actual admitted
samples; presentation follows device callback progress; EOS acceptance, native
drain and device-ring consumption remain distinct. An EOS transport can close
while its admitted audio drains. Ingress returns each record's credit once even
when retirement overtakes a queued job. A full inactive ring parks its own source
without blocking control or another owner. `active_worker_count` includes these
device workers so a runtime supervisor retains admission until native teardown.

The compositor's off-by-default `audio` feature enables this path for vector SDK
panes and supplies `timed-media-v1`, including when video is omitted. Audio gain
can therefore be offered by audio-only builds too. Its default build still
refuses native pane audio.
Embedded Vivido retains its existing shared audio integration independently.

## Standalone timed audio

The same opt-in host admits `TrackMode::Timed` audio. `enable_audio` replaces the
earlier experimental `enable_live_audio` method. Both modes use the same decoded
timing, source scope, ring reservation and retirement checks.

PCM may enter the bounded ring before PLAY. Initial PLAY maps its exact requested
PTS to clock start, discards preceding queued/future PCM and preserves the exact
decoded frame-offset origin. Known leading silence contributes to the startup
buffer horizon. PLAY may also precede media: a source worker primes its device
and decoder without waiting on native initialization in the control reader.
Periodic `NativeVectorHost::poll` retries priming if a prior native teardown still
owns the source permit. Zero-buffer PLAY can advance its clock before the first
packet, and PCM arriving after that position is discarded.

Activation and admitted PLAY both gate timed consumption. Clock-start follows
actual callback progress; presented follows actual PCM rendering. Leading and
underrun silence advance the timed clock without fabricating PCM presentation.
The last presented PCM PTS stays fixed through underruns while the master clock
continues advancing.
PAUSE retains the queue and clock. The native presenter withholds a frozen PTS
until a callback admitted before PAUSE commits its final counters. Linked video
also remains buffering while that master position is unsettled. Once confirmed,
the qualified paused position stays valid through later silent callbacks; PLAY,
PAUSE and binding replacement clear it before a new consuming operation. Resume
names the settled position and can consume retained PCM without another packet.
A reposition requires FLUSH
and a new media epoch/output timeline. Initial prebuffer cannot exceed the
two-second ring or the track's admitted latency policy. Synchronized starts,
hold-token resumes remain refused before mutation. With `vector-video`, linked
timed A/V follows the additional contract below.

Native audio DRAIN requires current EOS and completes only after decoder/resampler
drain and actual ring consumption. It is a bounded deferred request, sharing the
64-per-session registered-wait ceiling and a 30-second maximum wait. The control
reader remains live. Identical idempotent retries join one pending operation;
different bytes under its key are rejected, and the final non-secret success or
error is cached with correct request correlation. FLUSH, generation/surface
retirement, loss and resume cannot make an old DRAIN succeed against replacement
audio. Unprovable outcomes across a suspended/resumed control session are reported
as unknown outcomes. Logical closure removes pending and cached results.
`Session::drain` now takes a shared reference, allowing concurrent status queries
through an `Arc<Session>`. Deferred drain workers are included in runtime worker
accounting until their final response/cleanup.

The output support API uses `OutputControl::configure_timed_queue` for this
already-staged PCM policy and `Channel::observe_timed_queue` for its exact first
decoded position. These controls retain no native stream and perform no native
device operation. Vivido keeps its existing `configure_play` behavior separately.
The compositor's `examples/timed-audio.toml` supplies an independent timed PCM
application. `--timed-audio-render-check` exercises startup prebuffering, PAUSE
beyond the finite clip duration, continued staging through EOS while paused,
retained-queue resume, rapid toggles, both FLUSH replay paths and independent
owner/process retirement. It uses the same explicit fixture or default-device
selection described above. Synchronized playback remains future work.

## Local pane visibility

`NativeVectorHost::set_visible` applies local pane policy after metrics are
installed. Hiding a pane disables timed output consumption without changing
producer PLAY intent or discarding PCM, decoded video, or decoder state. The
last in-flight callback may finish; paused positions become authoritative only
after its final commit. Linked video follows that settled position. Video-only
software time freezes at the visibility transition. Showing resumes the same
PLAY from retained media, while explicit PAUSE stays paused. Live audio and
video retain their existing policies.

Presentation and playback-start/end waits and audio DRAIN fail with `NOT_VISIBLE` on hiding,
including already parked requests. This transient DRAIN error is not cached
under an idempotency nonce, so the producer can retry after showing the pane.
Control, bounded media staging and other panes keep progressing. Both visibility
transitions invalidate input and old display receipts; showing needs actual
fresh display admission. The compositor uses this policy for tabs, workspaces,
modals and native window visibility, including initially hidden applications.

When rendering runs on another thread, use the pane controller from
`NativeVectorHost::visibility_control` on the GUI thread. Audio and software-clock
holds then apply even while the render worker is blocked. The last admitted
callback still settles asynchronously; this is separate from render progress.
The host can refine video observations when it next polls. Hidden waits and
DRAIN are woken from the same state transition, and unrelated panes keep their
own clocks and queues.

The native host removes `timed-media-sync-v1` before authentication while
producer-visible hold/recovery remains unfinished. Its synchronized-start
scheduler now has private conformance coverage, described below; production
attachments continue to offer baseline PLAY. Keep calling `poll` while hidden.

## Synchronized-start scheduler

The native scheduler admits policy 2 only after coherent negotiation and keeps
the requested target frozen until startup is ready. With an active audio master,
its PCM queue supplies the minimum-buffer horizon. Every active timed video
member additionally needs a decoded picture at or after the target, even when
the requested minimum is zero. Video's separate buffer horizon does not delay
that audio clock. Video-only software playback requires both its own horizon
and an eligible picture. Decoder-drained EOS resolves an empty remaining stream;
accepted EOS alone does not. No timer bypasses this barrier.

Eligibility checks the reservation incarnation, channel generation, epoch,
decoder reset, authenticated resume generation, surface generation and admitted
surface/channel pair. A healthy peer with the same numeric IDs cannot supply
readiness. A new synchronized PLAY preflights active failed video members before
changing audio configuration or group intent. A full video queue can wait for
audio's buffer without being misclassified as an impossible video pre-roll.

PAUSE cancels pending startup while allowing one target picture to prime without
advancing time. Later pictures remain queued until another PLAY. Retained poster
eligibility is checked against the new target and timeline too.

A short audio master can start and finish between video scheduling ticks. Its
final callback observation still proves that the group clock started. Software
handoff preserves that fact and the final physical PTS, including while hidden;
showing the pane resumes without imposing a fresh video buffer horizon. The
clock-started milestone follows that observation too. The authenticated regression
for this case timed out before the correction and passes with it.

`NativeVectorHost` still removes the synchronization profile while frontend
recovery integration and platform acceptance remain unfinished. Test-only
negotiation exercises real authenticated sessions and output callbacks
without exposing a production option for an incomplete profile. These tests
share the established native A/V peer fixture with the public baseline tests.

## Native hold publisher

The native presenter internally publishes retained-decoder visibility holds for
sessions that negotiated `timed-media-sync-v1`. Production native attachments
still remove that profile; tests negotiate it through real authenticated
sessions. This extends the scheduler conformance work without advertising the
unfinished replacement/recovery contract.

With the explicit `testing` feature and native audio or video enabled,
`NativeVectorHost::use_test_synchronized_media()` restores the coherent profile
set before producer authentication. This supports first-party producer integration
tests against the real native presenter. It rejects use after authentication and
preserves ordinary admission, ownership and readiness checks. It is unavailable
in ordinary builds; production attachments still withhold the profile.

`NativePaneVisibility::stop_test_audio(owner, context, surface, track, generation)`
stops only a current clocked test binding in that controller's pane. It rejects
foreign panes, mismatched generations and retired controllers, and never stops a
hardware device. The real source worker then reports its usual scoped failure.
Vivi's native conformance tests use it to exercise device recovery with a healthy
video member and an independent owner reusing the same local IDs. Its linked
and video-only tests also cover host replacement, paused priming, early resume,
independent member epochs, repeated replacement and quit while unavailable.

Each owning surface has a nonzero checked transition serial. Visibility,
producer play/pause intent, and qualified position refinements emit actionable
`PLAYBACK_HOLD` records through the bounded control writer. `TRACK_STATUS` key 24
returns the same hold state. Unchanged held positions and ordinary running clock
progress do not emit notifications. Exhaustion or notification admission failure
closes only that authenticated owner's control connection.

Ordinary hiding retains its native decoders, so these notifications carry
`recovery_required = false`. Before callback settlement, the last physical clock
observation is estimated; a confirmed paused callback refines it to exact. An
active timed audio reservation owns clock selection, otherwise the timed video
software clock supplies it. Hidden startup has no fabricated position from
accepted packet timestamps. Every checkpoint is scoped to its owning track,
reservation incarnation, channel generation, media epoch, decoder reset,
authenticated resume, surface generation and PLAY operation. A new operation
retires feedback from the preceding pause. Arbitrary native audio seeks retain
the existing FLUSH requirement.

Releasing a hold preserves its frozen checkpoint while the live clock resumes;
status does not replace that checkpoint with advancing running observations.
Explicit PAUSE remains effective across hide/show. Tests cover unknown startup,
callback refinement, video-only playback, pause, a new PLAY and FLUSH/seek,
serial exhaustion, and a peer reusing the same local IDs. Baseline sessions
receive neither hold notifications nor hold status. Durable lease reconciliation
and lease-driven decoder replacement are covered below.

## Native holds across lease suspension

For a negotiated native synchronized session, lease suspension saves a detached
hold before clearing its media bindings. It preserves the producer's play/pause
intent independently of the paused or retired tracks and sets decoder recovery
required. Native audio pause is requested and the video software clock is frozen
before retirement, without waiting for a callback under the state lock. A
confirmed paused observation is exact; otherwise the last validated
physical observation remains estimated. Startup without clock progress stays
unknown. Buffering or reset software positions cannot supply a checkpoint.

Suspension persists metadata without writing to the lost control transport.
Authenticated resume returns the same surface hold through `TRACK_STATUS`,
including its original track/channel/epoch qualification and monotonic serial.
Channel replacement does not retag that historical position. A resumed session
with native media members releases the detached reason; a hidden pane keeps its
not-visible hold until shown. Decoder recovery remains required. A newer
PAUSE updates saved intent; an explicit PLAY clears the old checkpoint. Repeated
disconnects preserve those changes without synthesizing a clock from a reset
decoder. Removing the owning surface removes the hold.

Serial exhaustion during suspension retires only that owner's lease and surfaces.
Tests exercise real leased activation, transport cancellation and authenticated
resume, including independent owners with reused local IDs, running and paused
intent, audio and video-only clocks, unknown startup, replacement channels and
repeated reconnects. Baseline leases retain their existing behavior and receive
no synchronization authority. Replacement transactions below complete this
lease-driven path. Production native attachment still withholds the synchronization
profile while broader lifecycle integration remains unfinished.

## Native replacement transactions

After authenticated lease resume, the presenter can send `NEED_KEYFRAME` reason 6
on a current video connection. It carries the current surface hold serial and
the saved resume PTS when known. Unknown startup position stays absent. Opening
a connection with its existing generation can receive the request; synchronized
playback requires newer generations for every retained timed member, including
audio. A newly created track incarnation can replace a retired member too.
Hidden panes do not issue replacement requests until shown. Unchanged
serial/generation/resume/writer state does not generate another request.

The producer reconciles the request with hold status and ignores a completed or
superseded transition. It advances old generations before cancelling their writes,
uses each track's own media epoch and keeps media IDs increasing across generations.
It primes video from an earlier random-access picture and uses synchronized PLAY
at one group target. Reference pictures before the target preserve decoder state
but do not satisfy the eligible-picture barrier. Recovery FLUSH pauses the retired
timeline without replacing the saved producer intent; explicit PLAY and PAUSE
still update it.

Playing recovery completes only after the accepted replacement operation supplies
a current physical or software clock observation. Admission keeps the owning track,
channel and writer, media epoch, decoder reset, PLAY serial, authenticated resume,
surface generation and native reservation incarnation. An old member generation
or a peer's readiness cannot substitute. If audio ends, continuing video supplies
its admitted software clock. No stale audio clock is relabeled as video progress.

For a paused group, install synchronized PLAY and PAUSE before priming media.
The presenter can then admit the target picture and confirm audio startup readiness
or decoder-drained EOS without consuming samples. Once every current member is
ready, recovery completes with playing intent false. A later explicit PLAY starts
the retained queues. A picture decoded before PLAY can acquire the current PLAY
receipt when selected for paused priming; its acknowledgement still requires a
fresh host display receipt. Empty EOS admits its exact native surface/channel pair, but
EOS acceptance alone does not resolve decoder readiness. The stream-free
`OutputControl::ready_to_start` observation adds no clock or source authority.

Completion advances the hold serial and clears decoder recovery. Delayed duplicate
reason-6 events leave the completed generation's sender usable. Requests and
operation snapshots retain bounded metadata and weak identities, not native
devices or decoder allocations. The conformance tests use real authenticated
connections, FFmpeg and explicit clocked audio fixtures; they do not claim physical
sound, GPU presentation or latency measurements. Host/device retirement extends
these transactions as described below. Vivi now supplies audio-only, linked A/V
and video-only recovery policy with real native conformance coverage. The
compositor frontend exposes explicit pane-local retry controls. Platform
lifecycle and device-availability acceptance remain necessary before
`NativeVectorHost` advertises the full profile.

## Native host and device retirement

For a negotiated synchronized session, failure of a current native audio output
or media connection freezes its owning surface before retiring the failed track.
The hold adds the downstream reason, preserves producer intent, and requires
decoder recovery. Other owners, including owners reusing the same local IDs,
keep their own clocks and queues. A late failure callback must still match the
original channel, writer, decoder and reservation incarnation; it cannot lose a
replacement track.

Native staging also checks output health while its ring is full. A stopped or
failed output cannot drain that ring, so waiting for space without checking
health would strand both the owning decoder and its recovery notification. A
deterministic saturated-ring regression covers failure without requiring another
callback or media packet.

Use `NativePaneVisibility::media_recovery_pending()` to observe whether the pane
needs an explicit device retry. It does not acquire output resources or wait for
render work; visibility-only holds and replacement priming do not request retry.
After explicit user retry or restored frontend/device availability, call the
pane controller's
`NativePaneVisibility::recover_media()`. This visits every affected owner in that
pane, releases its downstream availability gate, and allows correlated
replacement requests. It opens no device and starts no retired PLAY operation.
The producer must replace the lost tracks, query the surface's current revision,
reactivate its slots, prime replacement channels and submit synchronized PLAY.
Actual current-member readiness still governs recovery completion. Repeating the
signal without a new device failure is a no-op; other panes remain held.

Dropping `NativeVectorHost` first freezes eligible groups with the detached
reason, then retires its media, windows, input and display receipts. The
presenter can remain alive with its authenticated sessions. Install
`NativeVectorHost::attach_replacement(presenter, family)` after the old host has
fully retired. It restores the original audio opt-in policy and creates a fresh,
checked attachment generation. A live host, absent retirement history or exhausted
generation is rejected without claiming the unavailable frontend.

Every replacement pane starts hidden, including panes whose producers survived
the retirement. Install metrics and explicitly show each pane when its frontend
is ready. Fresh tracks and display receipts remain required; controllers and
frames from the old attachment cannot acquire authority in the new one. A new
attachment releases the old host gate and supersedes its failed-device gate,
while authenticated lease and visibility gates retain their independent scope.
Lease resume alone does not clear a blocked device. Each device failure retains
its attachment generation: a new failure during host replacement requires its
own recovery signal and cannot borrow the earlier replacement's release.

Retirement pauses clocks without waiting for a device or callback under presenter
state. The last qualified observation may remain estimated; unknown startup
position stays absent. Failure to enqueue a hold closes that owner's control
writer but still commits the local freeze for lease reconciliation. Exhausted
serials keep the owning surface blocked instead of wrapping or restarting media.
The conformance tests cover full replacement playback, stale callbacks/receipts,
combined lease/device loss, startup without a clock, attachment exhaustion,
notification failure and recovery of multiple owners in one pane. They use the
explicit clocked output fixture and do not establish physical device recovery.

## Linked timed A/V

Enable both `vector-audio` and `vector-video`, explicitly opt into audio, and
activate timed audio and video tracks on the same owner/context/surface. Every
member must have matching playback intent before activation. Replacing or
removing a timed audio master requires pausing its video first. Mixed Live/Timed
A/V is refused before activation rather than running independent clocks.

PLAY addressed to either member preflights all active native members before
mutating intent or configuring the single audio device. Video policy, audio
latency/ring limits, clock reposition and serial exhaustion can therefore reject
the whole request. An inactive target cannot introduce a second native audio
master into that PLAY. Native initialization remains asynchronous.

Audio consumption waits until every active timed video member has its requested
buffer horizon or drained EOS, as well as the audio ring's own startup horizon.
Video waits for actual callback progress and selects due pictures directly from
that admitted audio clock. A healthy buffering audio source never starts a
second software timeline. Timed underrun silence continues the shared clock.
Video's bounded queue parks only its own media worker when audio startup waits.

PAUSE freezes the group and retains its latest video and queued PCM. FLUSH
discards its target's decoder/epoch state and pauses the group; repositioning
audio still requires its own FLUSH. Neither operation affects another owner
reusing the same local IDs. Final video EOS still needs actual display proof.

Audio loss or an audio stream ending before video relinquishes clock authority.
Video continues on a monotonic software clock from the last admitted audio
position, preserving the original PLAY start. It can subsequently pause/resume
without reopening the lost/ended audio timeline. Source selection revalidates
the complete admitted audio scope, including channel/writer, epoch, reset,
resume, surface generation and reservation incarnation.

Authenticated SDK regressions cover joint startup, video-targeted PLAY, retained
EOS resume, short-audio EOS, audio transport loss, group FLUSH and independent
owners with reused IDs. An explicit ignored regression exercises the real
default audio device.

The compositor's `examples/av.toml` supplies a combined application with both
tracks on one authenticated surface and an independent vector view. Its
`--av-render-check` covers joint startup, displayed video EOS alongside device
consumption, queued-EOS group PAUSE/resume, paused resize, both replay paths,
independent apps and active A/V crash/restart. It uses the same explicit fixture
or default-device selection as the other audio checks. The clip is finite PCM
and H.264, and GUI input is simulated. Synchronized/hold-token starts and
physical Linux device/input acceptance remain work.


## Linked Live A/V

Enable `vector-audio` and `vector-video`, explicitly enable native audio, and
activate matching Live audio and video on the same authenticated
owner/context/surface. Live has no PLAY, PAUSE or startup prebuffer contract.
Before audio has a usable callback clock, video keeps its immediate latest
preview policy. Once audio has actually rendered frames, newly decoded future
pictures wait for that admitted clock to reach their PTS. An already displayed
poster can remain visible when audio starts at an earlier position.

The existing two-image Live reservation holds the current picture and at most
one latest pending picture; it creates no additional queue. Decode advances
reference state before dropping older output. An older decoded PTS cannot evict
a newer pending picture. Pending replacement frees the previous pending slot,
and GUI-retained images continue to hold their original budget leases.

Every retained picture keeps its original admission: owner/context/surface/
track, channel/writer, epoch, decoder reset, resume and surface generations, and
reservation incarnation. A clean EOS transport close can retain that exact
picture; it cannot rebind it to a replacement writer or track with reused IDs.
FLUSH and retirement invalidate pending output. Video EOS waits for pending
output to become eligible and for the final actual display receipt.

Audio loss, device failure or early audio EOS relinquishes master authority.
Video then resumes its ordinary latest-picture policy; Live creates no software
playback clock. A peer's clock can never substitute for the admitted master.
Authenticated tests cover callback gating, immediate preview before audio starts,
latest-PTS replacement, clean-EOS closure, audio loss, FLUSH and track recreation,
with independent owners reusing local IDs.

The compositor's `examples/live-av.toml` selects `--sdk-vector-live-av` for two
independent combined applications beside a linked counter. Click/Space starts
or replays a finite three second PCM/H.264 clip. Both tracks share their media
session and surface; replay FLUSHes both epochs and advances only channels that
submitted EOS. When video credit lags submitted PCM, the sample skips complete
unsent GOPs to a fresh keyframe and retains the final dependent pictures.
`--live-av-render-check` requires actual callback consumption and
video display, resizes during playback, stalls the GUI for two seconds while
media workers continue, checks both replay generations and peer
isolation, and crashes/restarts an active A/V child. It uses the same explicit
callback fixture or default-device selection as the other audio checks. GUI
input is simulated; physical Linux audio/input and synchronized starts remain
outside this acceptance scope.
