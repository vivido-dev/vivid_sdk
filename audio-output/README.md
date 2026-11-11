# Native audio output

Optional SDK `audio-output` exposes `vivid_sdk::audio_output`. This standalone
crate contains Vivido's bounded CPAL output ring, device callback and source
clock. It depends on neither FFmpeg, GUI, terminal nor presenter code. It grants
no protocol, device-capture or source authority. The host binds each opaque
`Output` instance to its complete admitted source identity.

Call `Output::open` or `open_with_limits` on a device worker. Native device
queries, stream startup and destruction belong off GUI/control threads. `Format`
reports interleaved sample rate and channels; a separate decoder or mixer must
produce that format. Default ring duration is two seconds. `Limits::capacity_for`
validates formats, checked frame rounding and allocation limits without opening
a device. Ring storage is at most 16,777,216 f32 samples (64 MiB). Hosts reserve
aggregate device, ring and decoder budgets before admitting sources.

`Output::channel` returns an opaque capability for that exact instance and channel
generation. `Channel::push` accepts and publishes finite whole frames and waits
only for its own ring, even when the device has freed only one channel slot.
Run it on a media worker. `advance_channel` accepts exactly the next nonzero
generation, invalidates old writes before waiting for them, and preserves the
device stream. Its replacement channel is unavailable until the retirement flush
finishes; overlapping advances are refused. `stop` permanently silences the output and releases blocked
writers. A retained numeric generation cannot address another output instance.

`Output::control` returns an `OutputControl` that can pause, start, stop, change
gain and observe the sample clock without retaining the native stream. Keep the
`Output` and channel on the device worker. Dropping the last output owner stops
later callbacks even if controls remain alive. `Channel::try_push` publishes a
complete-frame prefix without waiting for ring capacity, allowing a worker to
service retirement and bounded staging between writes.

`OutputControl::ready_to_start` observes the configured startup reserve or
decoder-drained EOS without starting consumption or calling a native device.
Permanent stop returns false. Readiness does not prove elapsed playback, display
or source identity; the host still validates its admitted timeline and can use
`paused_clock` to confirm callback quiescence.

`pause` disables future consumption; a callback admitted before it can still
commit its last block. Poll `OutputControl::paused_clock` before treating a
position as frozen. It returns `None` while enabled or while a consuming callback
can still advance the clock. `Some(PausedClock { pts_us: None, .. })` distinguishes
an already quiescent output whose timeline origin is unknown. A known PTS alone
does not prove prior PCM consumption. The observer never waits, locks, allocates
or calls a native device. Keep the output paused and serialize start and
reconfiguration while observing it. Starting or reconfiguring invalidates the
observation, and the host must retain its own source/playback identity. Already
submitted native-device samples cannot be recalled; this is not acoustic-latency
measurement.

Validate PLAY before mutating host scene state. `configure_play` refuses unknown
start PTS and prebuffers exceeding ring capacity, with no partial mutation.
Positive prebuffer durations require at least one complete frame. Startup maps
the requested PTS exactly, including leading silence before a future first
packet. PAUSE retains the current timeline; resuming does not relabel already
rendered or queued audio. Repositioning requires a timeline flush and coordinated
host-side decoder/packet retirement.

Timed underruns render elapsed silence while the device clock continues. Samples
that arrive after that timeline position are discarded instead of replayed late.
Live output retains its capture clock during underruns and keeps the existing
adaptive startup reserve. EOS acceptance, decoder completion and actual ring
consumption are distinct; `poll_drained` never blocks control. Empty EOS does not
turn future packet metadata into elapsed audio. No audio operation proves that a
visual frame was displayed.

For PCM already staged by decoded timestamps, call `configure_timestamped_live`
before submission. It keeps the bounded target startup reserve, preserves every
queued frame and holds the live clock on underrun. The host queues real zero
frames between the corresponding audio ranges and removes overlapping new PCM;
startup never trims away that ordered timeline. This policy is independent of
Vivido's existing adaptive live output.

For standalone timed PCM already staged in timestamp order, use
`Channel::observe_timed_queue(origin_us, frame_offset)` once before its first
samples and `OutputControl::configure_timed_queue(start_us, minimum_buffer_us)`.
PLAY and PCM may arrive in either order. The stream-free control validates before
mutation, skips queued/future pre-roll, includes leading silence in the buffer
horizon and preserves exact decoded frame offsets. Timed underruns advance the
clock and discard late PCM. PAUSE/resume retains the same timeline and needs no
new packet; repositioning needs a fresh output timeline. `presented_frames`
counts actual PCM, excluding leading/underrun silence and discarded samples.
`presented_pts` records the end of the last rendered PCM and stays fixed through
underruns while the master clock continues advancing.
The callback acquires no new locks or allocations for this policy.

The callback consumes and trims complete interleaved frames, even when a producer
has published only part of one. Delay, gap, leading-silence and trim counts preserve
channel boundaries. Fractional durations are rounded to complete device frames.
The realtime callback uses bounded ring operations and atomics, without locks,
allocation, waits or a GUI event loop. Media packet timestamps, epochs, PLAY
generations and authenticated ingress remain host responsibilities. Native
compositor integration must bind them to decoder work and callback lifetimes.

## Test support and provenance

`audio-output-testing` enables explicit device-free fixtures through the SDK.
`testing::manual` drives the production callback deterministically, with no audio
device. Its renderer holds a weak output reference and cannot keep a retired
output alive. The feature is off by default. Vivido enables it only for tests.

Migrated on 2026-11-08 from the CI worktree's `vivido/src/vivid/audio.rs`, based
on Vivido revision `8d0f3c8881e36cad035207d12aab17defda2f54a`. The Apache-2.0
license is included. Vivido now imports the shared SDK output and constructs its
shared decoder explicitly for the returned device format. Its microphone remains
in Vivido and retains the existing consent rules.
