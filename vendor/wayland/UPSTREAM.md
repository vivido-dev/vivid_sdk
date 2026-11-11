# Private native Wayland dependency bundle

These eight unmodified-license crate archives originate from
[Smithay/wayland-rs](https://github.com/Smithay/wayland-rs) and
[Smithay/wayland-protocols-rs](https://github.com/Smithay/wayland-protocols-rs).
Each directory retains its upstream MIT license, authorship, `Cargo.toml.orig`,
package lockfile and `.cargo_vcs_info.json`. `sources.json` records exact archive
hashes and VCS revisions. The normalized manifests use explicit sibling paths for
these packages so an embedding application's workspace does not need Cargo patches
and cannot silently resolve an unmodified native backend. Missing bundled files
fail the build. The optional native host is separate from the GUI platform's
registry-based Wayland client stack; its boundary uses owned SDK frames and input.

| Crate | Version | Archive SHA-256 |
| --- | --- | --- |
| wayland-backend | 0.3.17 | `38a91b4eaddff87b1cd1074985e3713da4af2c49742d1b356b2c01670a67a078` |
| wayland-client | 0.31.15 | `e3c36a0f861ad76d0901f2800b46321410d9f73f2ea88aac0650d86c32688073` |
| wayland-server | 0.31.14 | `0dde9c29be0f723a573977de51ee455bf3dfa03652730a74f9dd3b337e374d75` |
| wayland-protocols | 0.32.13 | `23d0c813de3daa2ed6520af85a3bd49b0e722a3078506899aa9686fea58dc4b6` |
| wayland-protocols-wlr | 0.3.12 | `eb04e52f7836d7c7976c78ca0250d61e33873c34156a2a1fc9474828ec268234` |
| wayland-protocols-misc | 0.3.12 | `6e9567599ef23e09b8dad6e429e5738d4509dfc46b3b21f32841a304d16b29c8` |
| wayland-cursor | 0.31.14 | `4a52d18780be9b1314328a3de5f930b73d2200112e3849ca6cb11822793fb34d` |
| wayland-egl | 0.32.7 | `7dc56cc3ef057a5efb434e58a0712ea30d2823b736ba9faa4669854ac6454395` |

## Local changes

- The Rust server's default-compatible `ClientData::max_object_slots` hook limits
  the sum of retained client/server namespace lengths, including destroyed holes.
  Slot reuse remains permitted. Core sync/get-registry, registry binds, typed
  new-ID requests and server-created objects check admission before table growth.
  Invalid client IDs cannot claim the server namespace. The SDK always supplies
  2,048 slots per connection. Standard vector capacity and object payloads are
  separate overhead; this is not an exact process-memory limit.
- Exhaustion posts the core no-memory error and retires only the offending
  connection. Server allocation returns its existing `InvalidId` error rather
  than succeeding after rejection. The handle retains the supplied user-data Arc
  until the backend mutex is released, including on allocation failure.
- Object and connection identity generations use checked increments. Exhaustion
  fails instead of wrapping into an earlier identity. Server namespace allocation
  also checks the representable protocol-ID range.
- `RequestLimits`, `RequestBudget`, and default-compatible client-data hooks add
  pre-parse request-count and declared-wire-byte token buckets. Header admission
  survives partial payload reads and descriptor waits without double charging.
  Shared budgets can outlive connections; the policy getter is evaluated for each
  new request so roles can select existing root/helper debt without resetting it.
  Both buckets refill with monotonic fractional credit, capped at the configured
  burst; denial spends neither shared bucket. Exhaustion clears buffered data/FDs
  and records core no-memory for the exact client. Defaults remain unbounded, and
  the system backend ignores the hooks. The SDK requires the Rust backend and
  always supplies connection plus separate shared root/helper budgets.
  Tests cover fragmented reads, pre-body byte rejection, core count admission,
  shared debt across reconnects and role changes, independent owner groups,
  fractional refill, idle caps, and backwards sample handling.
- The Rust server's default-compatible `ClientData::max_buffered_fds` bounds
  incoming and outgoing userspace descriptor queues separately. The SDK sets 64
  for each. Incoming batches use fixed inline temporary storage; checked admission
  precedes queue growth. A logical 28-descriptor batch limit also rejects excess
  descriptors that fit ancillary alignment padding. Truncated control data is
  rejected even without an explicit queue limit, avoiding descriptor-order
  corruption. Outgoing admission counts FDs before duplication and flushes/rechecks
  before rejecting. Parsing and flushing return queue capacity.
  Rejection closes queued/temporary descriptors and discards buffered bytes before
  attempting the core no-memory error. A blocked peer may not receive the error;
  its exact `ClientData` still receives protocol failure. Kernel-queued rights and
  resources already transferred to application services are outside these counts.
  No new unsafe code is added. Socket tests cover capacity reuse and pre-duplication
  admission; a public API regression saturates output beside another owner reusing
  the same numeric ID and proves only the rejected queue is released.
- The system server backend ignores the hook. The SDK explicitly rejects that
  backend before private-display setup; there is no quota bypass through feature
  unification. The bundle's integration regressions explicitly select Rust even
  when other workspace members enable system client support.
- Backend client insertion call sites accept the internal structured map error.
  A socket unit-test iterator drops an unnecessary conversion for Rust 1.95 Clippy.
  No new unsafe code is added; the backend retains its declared Rust 1.71 floor.
- Published archives omit path-only upstream development dependencies. The
  normalized client manifest restores its local protocol example dependency;
  the backend restores its scanner and client/server sys development features.
  Its generated test protocol uses an unchanged fixture from the scanner archive,
  recorded in `test-fixtures.json`, instead of an absent sibling source tree.
- The independent workspace manifest and lockfile support bundled checks.
  `rustfmt.toml` matches the backend revision's upstream workspace configuration.

`vivid-backport.patch` records all differences inside the eight package directories,
including normalized manifest wiring, restored test fixture and new tests. It
applies with `patch -p1` to a directory containing pristine extractions renamed to
these package directory names. Original manifests, individual package lockfiles
and licenses remain byte-for-byte unchanged.

## Verification and upgrades

From this directory on Linux with Wayland client/server/EGL development libraries:

```sh
cargo fmt --all --check
cargo test --offline --workspace --all-targets
cargo clippy --offline --workspace --all-targets -- -D warnings
python3 verify_sources.py
```

`verify_sources.py` uses cached crate archives (or `--cache DIR`), checks their
hashes, applies the patch in a temporary directory and compares every package file.
It also verifies the scanner fixture's archive and bytes. SDK native tests exercise
rendered roots/helpers and retained callback slots; the composed GPU check exercises
failure classification, independent peers and fresh-owner restart. Re-run those
checks before replacing this bundle. No Vivid wire records or numeric assignments
change here.
