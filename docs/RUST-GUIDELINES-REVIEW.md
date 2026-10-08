# Rust guidelines review

This review checks the Rust crate against Microsoft's Pragmatic Rust Guidelines for applications,
libraries, and performance (reviewed 2026-10-07). Commit `e542154` fixed the findings that did not
change the public API: `Debug` on every public type, with redacted output for types that hold
secrets; presenter items re-exported by name instead of by glob; the polling in `wait_for_rate`
and in the microphone write watchdog; and lint `allow`s converted to `expect`s with reasons. A
follow-up raised `rust-version` to 1.95, because the crate already used `Atomic*::try_update`,
which became stable in that release. The open findings follow, most important first. Guideline IDs such as `M-STRONG-TYPES` name the rule
each one comes from.

The application guidelines do not apply to this crate. It is a library, it uses no
application-level error crate, and the allocator and `target-cpu` rules only apply to binaries.

## Verification gate

**Presenter-only dead code.** With only the `presenter` feature, `--all-targets` reports
`presenter/overlay_host.rs` methods as never used: `environment`, `windows_with_semantics`,
`editor_caret`, `queue_accessibility`, `cursor`, and `clipboard`. Their callers are probably behind
`testing`. Gate the methods on the same feature, or add an `expect(dead_code)` with a reason.

## API design (breaking changes)

**Identifiers are bare `u64`s (M-STRONG-TYPES, M-INIT-CASCADED).** Several public functions take
two or more raw IDs in a row, and the compiler cannot catch them being swapped:

- `TerminalPlacement::node(node_id, surface_context_id, surface_id)`
- `TrackBuilder::detached(context_id, surface_id, slot, mode, lane)`
- `InputBindingGuard::enable_for_context(context_id, surface_id, ...)` and `FileDropBindingGuard::enable`
- `TrackChannel::send_raster_delta(epoch, frame_id, base_frame_id, ...)`

The repository treats the complete owner/context/surface/track tuple as identity. That makes a
transposed ID an ownership bug, not just a wrong value. The fix is to pass newtypes, or the
existing `TrackAddress`-style tuples, instead of loose integers. Several of these functions also
carry `expect(clippy::too_many_arguments)` for the same reason.

**Errors are `io::Error` with string messages (M-ERRORS-CANONICAL-STRUCTS).** About 410 functions
return `io::Result`. The only typed errors are `PresenterError` and `TrackLostError`, and
`presenter_error` wraps them in `io::Error::other`, so callers have to downcast to reach them.
`PresenterError` has public fields and no backtrace. Moving to per-area error structs with `is_*`
helpers would change every signature, so it is only worth doing if callers need to branch on
causes they can't reach today.

**Builder conventions (M-INIT-BUILDER).** `SessionLeaseBuilder::new` and `TrackBuilder::new` are
public constructors on the builder itself. The guideline wants `SessionLease::builder(...)` and
`Track::builder(...)`, with the required parameters passed when the builder is created.

## Performance

**Per-frame clones on the send path (M-HOTPATH, M-MEM-REUSE).** `send_raster` and
`send_raster_adaptive` call `Track::configuration()`, which clones the whole `TrackConfiguration`
for every frame. `raster_delta_body` clones the entire `TrackLocal` under the track lock,
including the `effective_claims` map, only to read the raster configuration and the delta limit.
Every frame also allocates a new body `Vec`, and `send_raster_adaptive` can encode a frame twice.
Possible fixes:

- Read only the fields needed under the lock.
- Keep the immutable configuration in an `Arc`.
- Add a send API that takes a reusable body buffer.

**No benchmarks (M-HOTPATH).** There is no `benches/`. Add a small benchmark of raster and video
sends against an offline session before changing the send path, so the change can be measured.

**Remaining polling loops (M-THROUGHPUT).**

- `accept_loop` in `presenter/service.rs` sleeps 10 ms whenever `accept` returns `WouldBlock`. The
  `PresenterListener::accept` contract is non-blocking, so this loop is how shutdown gets noticed.
  Removing the polling needs a way to wake the listener, which changes a trait that products
  implement.
- `spawn_wait` polls every 2 ms while a track wait is pending. Moving it onto the presenter's
  `changed` condvar first requires checking that every change to state `evaluate_wait` reads also
  notifies: milestones, `outer_presented`, `outer_position`, `last_media_id`, `lost`, holds, wait
  cancellation, channel generation, and track removal. Some of these already notify, but not all.
  One missed notify would turn into a wait that stalls until its timeout.

## Resilience

**Process-wide packet counter (M-AVOID-STATICS).** `NEXT_PACKET` in `audio_input.rs` numbers
microphone packets for every sender and presenter in the process. A counter per
`AudioInputSender` would scope packet IDs to their owner. That matches the repository's rule on
owner identity and makes tests deterministic.

## Documentation

With `clippy::missing_errors_doc` and `clippy::missing_panics_doc` enabled, 243 public functions
that return `Result` have no `# Errors` section, and 38 that can panic have no `# Panics` section
(M-CANONICAL-DOCS). To reproduce:

```sh
cargo clippy --features presenter,testing --lib -- -A clippy::all \
  -W clippy::missing_errors_doc -W clippy::missing_panics_doc
```

## Deliberate deviations

These differ from the guidelines on purpose and should stay unless the reason changes.

- **Flat crate root (M-BALANCED-MODULES).** About 150 items are re-exported at the root so that
  producers keep using paths like `vivid_sdk::Session`. The crate docs record this choice.
- **Re-exports from `vivid_protocol` (M-FOREIGN-REEXPORTS).** The two crates belong to the same
  Vivid stack, which is the guideline's umbrella-crate exception.
- **The `testing` feature name (M-TEST-UTIL).** The guideline suggests `test-util`. Renaming it
  would touch every consumer's manifest for a cosmetic gain.
