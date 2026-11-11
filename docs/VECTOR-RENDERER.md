# Shared vector rendering

`vector-render` exposes `vivid_sdk::vector_renderer`, migrated from Vivido's
portable display-list compiler and shaped-text painter. It is independent of
GPUI, the SDK presenter, a terminal runtime and a GPU device. Existing producers
inherit none of these dependencies unless a rendering feature is enabled.

`compile` and `compile_with_layouts` validate a complete Vivid vector `Canvas`
before returning one immutable `CompiledScene`. Shapes, brushes, transforms,
clipping, shadows, styled strokes, plain text, retained layouts and image assets
share the same compiler. The scene owns matching hit geometry; even-odd holes,
transparent regions and transformed clips follow the drawing rules. Transformed
styled-stroke extents include the miter reach and are checked before encoding.

Implement `TextShaper` using the host's real font selection. Its Parley layout
feeds Vello glyph drawing; plain text compilation never invents synthetic font
metrics. Vivido's adapter preserves its native font/fallback policy. Retained
styled layouts use `text_layout::paint`, which validates paragraph geometry and
style references and paints the exact premeasured layout without reshaping.

`text::FontSystem` supplies native font discovery and shaping without a GUI or
terminal. `text::prepare_styled` and `measure_styled` share Vivido's paragraph
preparation: styles, explicit line breaks, wrapping, alignment, spacing and
grapheme-safe ellipsis use one measured and painted layout. Public source offsets
refer to the original UTF-8 paragraph. The bounded internal direction mark and
ellipsis may add six bytes without increasing the producer's 4,096-byte limit.
The public `text_layout::paint` API still validates ordinary input limits.

With `presenter,vector-render`, `presenter::NativeTextHost::attach` binds this
service to an `Arc<VirtualVivid>` before any producers authenticate. Construct it
on the font worker, using an empty family for the platform sans-serif default,
then call `poll` regularly. Each call services at most one text request and
returns other platform work for the outer host to complete. This is not an
event loop: `None` can mean a text request completed, so hosts continue polling
on subsequent turns. Typography is advertised only after attachment succeeds.

`retained_layouts(BridgeSurfaceKey)` supplies live typed layouts to
`compile_with_layouts`. Layouts are isolated by producer, context, surface and
surface generation; suspending a lease hides them until its owner resumes.
Released IDs cannot enter new scenes. Existing compiled scenes retain their
resource tokens, so release or owner teardown does not free their accounting
early. Native storage is bounded to 128 layouts per owner and 64 KiB of charged
source envelopes across the presenter, including released resources still held
by compiled scenes. Missing fonts and host teardown return errors; dropping an
attached service never restores synthetic measurements.

Host-service waits run outside the authenticated control reader, under an
eight-job producer ceiling and a 64-job presenter ceiling, further limited by
the negotiated pending-request contract. Queries and shutdown remain responsive
while a font/platform response is pending. Owner/surface cleanup cancels its
pending control-host work; replies are checked against the current control connection before
installing resources. These same bounds apply to existing outer-host relays.

The caller resolves image and layout IDs in the complete owner/context/surface/
track/channel namespace before supplying their maps. The compiler does not grant
wire authority or infer ownership from local IDs. A compiled retained layout
keeps its original charge token alive. `retain_resource` attaches other external
charges and rejects more than `MAX_COMMANDS + MAX_RETAINED_ASSETS` tokens before
mutation. Namespace release does not release storage still referenced by a scene.

`vector-gpu` additionally exposes `vector_renderer::compositor`. A `Compositor`
borrows the host's existing WGPU 30 device, queue and Vello renderer, creates a
premultiplied RGBA target, and composes back-to-front `Drawing` values. It creates
no adapter, native window or event loop. The application selects its WGPU backend;
Vivido uses Metal, Vulkan or DirectX 12 through its existing renderer.

The adapter must support Vello's shader capabilities. Linux software GPU
acceptance uses Mesa 25; the older Mesa 22 test image fails shader validation for
packed half-float operations. Rendering tests require a working adapter and fail
explicitly rather than treating that failure as successful GPU coverage.

Each drawing carries a complete `SurfaceIdentity`, placement revision, exact
producer `Submission`, bounds, scale and immutable compiled content. Cache keys
include the owner and submission, so local ID reuse by another owner cannot
reuse its texture contents. Duplicate windows, inconsistent identities, invalid
scales and out-of-range geometry fail before cache mutation. Target dimensions
must fit the device and at most 256 MiB of RGBA pixels, matching Vivido's existing
capture ceiling. Window count follows the protocol's bounded overlay maximum.
Compiled encoding and hit storage are bounded to 64 MiB per scene, including
retained-layout expansion checks. Aggregate composition encoding is checked
before append. Caller-owned aggregate scene/image/font quotas remain required.

Unchanged keys, immutable compiled objects and target dimensions reuse the cached
image. Placement, submission or compiled-object replacement redraws; target resize
allocates a new texture. The cache retains compiled objects to prevent pointer
identity reuse. Metrics count
submitted render passes, cache hits and target allocations. These counters and
the changed flag do not acknowledge physical presentation. The host acknowledges
the exact submissions only after composition into its displayed frame, and
cancels unfinished outcomes on invalidation or failure.

Use the same renderer and device until `clear` unregisters the cached target.
Clear before renderer replacement or teardown, and discard parent scenes that
reference the invalidated image. Native health checks, GPU polling and recovery
remain the owning host's responsibilities.

Vivido's production overlay path uses the shared compiler and GPU compositor;
its adapter retains terminal font policy, authenticated window/input state and
presentation acknowledgements. Default SDK presenter configurations reject the
overlay bundle. Explicit overlay-host configuration exposes bookkeeping and relay
mechanisms. Native text measurement is now available through explicit service
attachment. [`NativeVectorHost`](NATIVE-VECTOR.md) additionally integrates
authenticated windows, compiled input and exact display receipts on an eventless
presenter. `vector-offscreen` optionally supplies a native device and asynchronous
straight-RGBA readback. The outer GUI owns display completion, ordered input and
renderer recovery. These features do not change wire profiles or turn stored
lists into presentation proof.

## Verification

From `vivid_sdk/`, run the CPU/compiler and GPU-cache validation suites:

```sh
cargo test --no-default-features --features vector-render --lib vector_renderer
cargo test --no-default-features --features vector-gpu --lib vector_renderer
```

From `vivido/`, explicitly run the GPU acceptance check:

```sh
cargo test --lib sdk_overlay_compositor -- --ignored --nocapture
```

The GPU check is ignored in ordinary suites so they can run without an adapter;
the explicit command requires one and checks rendered pixels. It covers a visible
clipping gap, cache reuse, two owners with reused local IDs, independent removal,
target resizing and rejection before mutation. Native Metal and Linux software
Vulkan have both been exercised. Full Linux Vivido checks additionally require
its supported FFmpeg ABI; the verified environment uses FFmpeg 7.1 and Mesa 25.
