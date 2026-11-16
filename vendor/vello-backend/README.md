# Private Vivid Vello backend

This is the SDK's pinned GPU implementation dependency, based on Vello 0.11.0.
It retains the Vello Authors' Apache-2.0/MIT license, source headers and
[upstream README](README.upstream.md). [UPSTREAM.json](UPSTREAM.json) records the
exact original source hashes and commit. The package is named
`vivid_vello_backend` so a published SDK cannot silently resolve an unpatched
registry Vello as this implementation.

The local changes include the
[documented cancellation/map-range patch](../../docs/upstream/vello-0.11-cancellation.patch)
and the [checked-work patch](../../docs/upstream/vello-0.11-checked-work.patch).
The [wide-winding patch](../../docs/upstream/vello-0.11-wide-winding.md) supplies a
pinned derived MSAA8 shader for valid nonzero paths beyond packed eight-bit
winding capacity. It retains the original coverage LUT, fast/parity algorithms
and bindings, using full-width counters for crowded tiles. Its 9,552-byte shared
storage is below the existing coarse shader requirement. Source generation is
offline, hash-checked and reproducible; no registry or startup-source rewrite is
used.
Cancelled map callbacks no longer unwrap sends into dropped receivers, and
mapped-range errors propagate. Compute modules use WGPU's default shader runtime
checks: trusted shader source does not make scene-controlled arena overflow safe.
The async path releases failed coarse resources without dispatching fine work,
whose contract assumes successful coarse allocation. Encoding,
allocator sizes and scene limits remain unchanged. The SDK retains its original
per-job dispatch limits and independently rejects missing allocation evidence.

Scene/DrawGlyphs are re-exported from exactly upstream Vello 0.11.0; the original
scene implementation is not duplicated here. SDK/compiler and application Vello
scene identity therefore stays shared. GPU options/error types remain private
implementation details; applications should use SDK renderer APIs. Upstream
development-only hot reload and embedded profiling are not exposed by this fork.
Their inactive source remains attributable to upstream. The standard no-op waker
replaces the equivalent upstream utility implementation for current Clippy.

This backend creates no window or Vivid authority. Hosts still own device-health
callbacks, resource limits and display receipts. Cancellation safety does not
permit continuing GPU work after device loss. The SDK's raw-texture consumer
migration remains separate work.

Run from this directory:

```sh
cargo fmt --all --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

The SDK's explicit native `native_vector_shared_cancellation` regressions exercise
both map stages and host device-health retirement using this actual backend.
Platform/device and package-publication qualification must be recorded explicitly;
copying a patched source is not a qualification result.
