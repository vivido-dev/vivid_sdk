# Smithay 0.7.0 with compositor fixes

This directory contains the published [Smithay 0.7.0 crate](https://crates.io/crates/smithay/0.7.0),
from upstream commit `a166cf4c94b5aedc332a65aa1dd753e8148829c3` as recorded in
`.cargo_vcs_info.json`. The downloaded crate archive has SHA-256
`740cea6927892bc182d5bf70c8f79806c8bc9f68f2fb96e55a30be171b63af98`.
The upstream MIT license, authorship and `Cargo.toml.orig` are retained unchanged.
The normalized manifest directs its eight Wayland dependencies to the SDK's
private `../wayland` bundle; its lockfile records that intentional source change.

The SDK enables only `wayland_frontend` and `renderer_pixman`. It uses this path
dependency directly so a consumer workspace cannot silently resolve an unpatched
registry release. There is deliberately no registry version fallback. Publishing
the SDK's new GUI features requires distributing the patched dependency (or moving
to a verified upstream release containing the fixes), just as distributing the
extracted GUI crates requires a release arrangement.

## Local changes

- Backport parent-owned subsurface positions and complete-transaction state
  application from [upstream PR 2141](https://github.com/Smithay/smithay/pull/2141).
  [Issue 1894](https://github.com/Smithay/smithay/issues/1894) describes the
  desynchronized-position failure. Positions update all child cache snapshots;
  later child commits cannot restore stale coordinates. Position queues coalesce
  updates, discard dead resources, and admit at most 256 entries.
- Apply stacking requests on the parent's commit. Queued operations retain weak
  surface and role identities, recheck parent membership, and ignore dead or
  replaced roles. Pending and applied operation batches are bounded to 256;
  overflow terminates that client with the core display resource-limit error.
- Merge only explicitly committed synchronized-child transactions into a parent.
  A parent commit must not consume a child's uncommitted pending buffer/state.
  Transactions capture direct children at their own commit boundary, so a root
  cannot bypass an intermediate parent's missing commit. Switching to effective
  desynchronization releases existing committed caches, including inherited
  descendant transitions, without consuming uncommitted pending state. This
  follows the core [content-update rules](https://wayland.freedesktop.org/docs/book/Content_Updates.html).
- Stage initial visual membership until the parent's commit, even for a new
  desynchronized child. `SubsurfaceCachedState::parent_committed` exposes that
  state to the host; destruction resets all snapshots before a role is reused.
- Apply every cached state and parent stacking operation before invoking commit
  observers, so observers see a coherent transaction.
- Add an optional `CompositorHandler::subsurface_destroyed` callback so hosts can
  repaint and revoke input even when a role is replaced with identical geometry.
- Add `CompositorLimits` and opt-in `CompositorState::with_limits` admission for
  region operations, retained damage/callback payloads across pending/cached/current
  states, and queued snapshots per surface/cache type. Counts are checked before
  vector growth; every cache checks snapshot capacity before any commit snapshot
  is taken. Existing constructors retain unbounded upstream behavior. Commit/merge
  transfers payloads without resetting their aggregate count. The SDK always uses
  limits. A common exact-client core no-memory helper serves compositor and SHM
  rejection, preserving the Rust/system backend behavior already covered by the
  SHM regression. No Vivid protocol records change.
- Check complete, representable initial popup positioner geometry with i64
  arithmetic before role construction calls a host handler. Untrusted offsets
  and anchor rectangles previously reached unchecked i32 additions first.
- Add `ensure_viewport_valid_f64` for exact post-buffer-scale source bounds.
  The existing integer helper delegates to it without changing its contract.
  A 65-pixel buffer at scale two has a 32.5-unit extent; rounding this to an
  integer would reject legal edge crops or admit out-of-buffer ones. SDK tests
  exercise both cases through real viewport clients and preserve peer owners.
- Reject unknown `wl_surface.set_buffer_transform` values with `invalid_transform`
  before mutating pending state, as required by the core protocol. Previously
  those requests were silently ignored. A real helper-client regression verifies
  rejection while a root with reused local surface IDs remains live.
- Reset the popup's complete configure negotiation on unmap, including pending
  responses, last ACK, serials, buffer and initial-commit markers. Its next mapping
  must complete a fresh initial configure/ACK sequence.
- Notify `SeatHandler::focus_changed` when keyboard focus clears, as well as when
  it arrives. Without the clearing notification, host selection authority could
  survive a fast focus-out/focus-in cycle.
- Reject invalid cursor-shape values and version-two shapes sent through a
  version-one device with `invalid_shape`. Pointer shape requests require the
  exact latest enter serial even during an implicit grab; the core cursor
  helper's grab exception otherwise admitted mismatched shape serials. The SDK's
  real-client regressions cover valid versions, stale/future serials with a held
  button, bitmap/shape replacement and independently rendered owners.
- Add default-compatible policy hooks for selection serials, MIME metadata and
  starting a drag. The SDK validates actual delivered serials, bounds source MIME
  lists before retention, and refuses DnD before the generic handler installs a
  grab. No privileged data-control protocols are advertised by the SDK.
- Expose `SelectionSource::request_data` for the exact live provider and offered
  MIME type. The SDK can read a native source asynchronously and re-originate
  bounded text through its platform service without replacing it prematurely.
- Send the current ordinary or primary selection to a newly created device for
  the already-focused client. Device registration previously retained the
  resource without sending an offer, so clipboard readiness depended on whether
  registration preceded keyboard focus. Propagation is restricted to the new
  device and retains the existing focus/client checks. The SDK regression binds
  both devices after presentation without refocusing, verifies file/primary
  transfer, and preserves a peer display with reused local frame IDs.
- Add validated `ShmLimits` and `ShmState::with_limits` for a shared SHM-global
  budget. Pool bytes/count and retained buffer metadata are reserved before
  mapping or descriptor initialization; growth reserves its delta before remapping and
  rolls back on failure. Pool permits live in the backing storage, so destroying
  a pool resource cannot free capacity while buffers retain it. Bounded pools
  unmap and close on their owning worker before releasing their charge, rather
  than entering the global asynchronous destruction queue. The original `new`
  constructor retains its upstream behavior. Quota rejection explicitly posts
  `wl_display.error.no_memory` on the backend-owned display object; the Rust
  backend's client-kill API alone does not send the supplied protocol reason.
  A backend that hides the core object instead receives a client-kill request
  carrying the same scoped protocol reason. A two-client regression runs on both
  the Rust and system server backends; the surviving client still completes sync.
- Return immediately after rejecting a nonpositive pool resize. A zero resize
  previously reached an unconditional `NonZeroUsize` conversion and panicked;
  negative values could reach an enormous remap attempt after client rejection.
- Handle backend object-allocation failure when constructing selection and DnD
  offers. Exhausted server-created ID slots disconnect that client through the
  backend's core no-memory error; offer propagation skips the rejected allocation
  instead of panicking on an unwrap. The SDK refuses DnD before installing a grab.
- Remove two unused tracing imports and spell the existing SIGBUS handler's
  function-pointer cast explicitly for Rust 1.95. No new unsafe blocks are added.
- Resolve upstream Clippy findings for Rust 1.95: provide `Id::default`, derive
  equivalent buffer defaults, use integer `abs_diff`, and simplify a barrier queue
  loop. The two frontend-only examples use tracing instead of macros forbidden by
  upstream's own Clippy configuration; the geometry benchmark uses the supported
  Rand 0.9 method names. The backport keeps
  Smithay's declared Rust 1.80.1 API floor.
- Apply current rustfmt import ordering in the EGL FFI module; its behavior is
  unchanged. This keeps the vendored crate's formatting check clean.

`vivid-backport.patch` records source, normalized-manifest and lockfile differences
from the published crate.
It applies with `patch -p1` in a pristine extraction. These changes do not modify
Vivid records, registry assignments, or authority rules.

## Regression coverage

The SDK's `tests/gui_wayland_tree.rs` launches actual Wayland client processes.
It exercises synchronized and desynchronized commits, uncommitted child buffers,
parent-owned positions/stacking, nested integer scales, alpha, input regions,
role destruction/reuse, stale input and clipped-child presentation callbacks.
`tests/gui_wayland_shm.rs` checks pre-map pool/growth limits, aggregate storage
across helpers, retained backing pools after resource destruction, unattached
buffer budgets, reclamation and invalid-mapping rollback. The tree regressions
also retain two rendered owners with reused local IDs, proving that a helper
error leaves its root interactive and a rejected root's failure survives teardown.
Private quota tests cover checked overflow, growth rollback and independent budgets.
The tree suite also tests pending region/damage/callback limits, mixed cached and
pending payloads, bounded snapshots with retained region copies, capacity reuse
after parent application, and rejected roots/helpers beside independent rendered
owners. Unrelated helper cleanup preserves the original root input target.
It also checks direct-parent commit boundaries, effective desynchronization and
initial/replacement membership with no intervening parent commit.
Malformed-client tests retain a second display using the same client-local IDs
and prove it remains interactive. The original `tests/gui_wayland.rs` checks
configure/presentation, resize callbacks, unchanged buffers and root lifecycle.

The upstream crate can also be checked independently on Linux:

```sh
cargo fmt --all --check
cargo test --no-default-features --features wayland_frontend,renderer_pixman --all-targets
cargo clippy --no-default-features --features wayland_frontend,renderer_pixman --all-targets -- -D warnings
```

Keep this patch small when upgrading. Verify all native regressions against any
replacement dependency before removing it; successful compilation alone does not
establish correct Wayland transaction semantics.
