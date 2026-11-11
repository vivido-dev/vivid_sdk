# Experimental native GPU frames

The off-by-default `gpu-share` feature exposes `vivid_sdk::gpu_share` on macOS.
It does not enable the GUI. Enabling both `gpu-share` and `gui` adds
`Frame::paint` for the migrated GPUI runtime. Native FFI lives in the separate
`vivid_gpu_share` constituent crate; the main SDK retains `forbid(unsafe_code)`.
This is an in-process Metal/IOSurface prototype, with no cross-process protocol
or Linux/Windows sharing implementation.

Create one `Pool` per producer and call `Pool::begin_copy(device, queue, source)`
on its native renderer worker after drawing. Poll the returned `Pending` from
the event pump. Sources must be complete,
premultiplied-alpha, single-sample, single-mip, two-dimensional RGBA8Unorm or
BGRA8Unorm images. Both formats require COPY_SRC; RGBA also requires
TEXTURE_BINDING. Use resources from the same logical WGPU device. Physical
Metal device checks reject mismatches; ordinary WGPU validation still applies.

Each pool lazily caches one RGBA conversion pipeline, sampler and bind-group
layout for its most recent logical WGPU device. Pool clones share that cache;
resizing does not rebuild it. Logical device equality is required even when two
devices have the same Metal registry ID. An export on another logical device
evicts the old cache, including a direct BGRA export that needs no conversion.
On the native worker, call `Pool::clear_cache` when hiding or retiring the
renderer so caches do not prolong device ownership. The next export rebuilds
lazily. Clearing or dropping caches does not revoke frames or encoded work;
WGPU command ownership preserves pending pipelines through completion.
Allocation permits retain no cache/device, avoiding completion-callback cycles.

Each live frame owns an immutable BGRA IOSurface lease. BGRA sources use a GPU
texture copy; RGBA sources use WGPU's texture blitter for GPU conversion. Neither
transfer maps pixels into CPU memory. Later producer redraws cannot mutate a
retained frame. Only the last producer/HAL/scene/consumer-GPU reference can return
a successful lease's allocation and CoreVideo mapping to the idle cache. Each new
lease starts ineligible for reuse; cancellation, expiry or failed completion
discards it after submitted work retires, including a previously recycled image.

Idle images belong to one pool generation for the current logical device and
physical size. Resize, device change or `clear_cache` retires that generation.
Late references can release their charged allocation but cannot refill a new
generation. Each pool permits one through eight resident allocations, including
idle images and older retiring generations; each image is limited to 2048 pixels
per axis and 16 MiB. Exhaustion returns WouldBlock before native allocation.
IDs increase for every successful export, including reused allocations, and are
not complete app identities. The host must retain its ordinary owner identity
and reject closed-owner publication.

Use `Pool::with_budget` with a cloned `Budget` to bound active and retiring
producers together. Aggregate budgets allow one through 64 resident allocations,
bounding pixel payload to at most 1 GiB before driver overhead. Both local and
aggregate slots follow the native allocation through consumer retirement and
idle caching. `allocated()` reports charged resident slots; `retained()` reports
active leases, excluding idle entries. Neither is a physical memory measurement.
An idle cache can consume budget while `retained()` is zero. Clear unused pools'
caches to return idle capacity; a producer cannot evict another producer's cache.
Dropping a pool releases its idle entries while outstanding work stays charged.
Failed admission does not mint a frame serial or increase allocation counts;
geometry changes may first evict idle images from the old generation.

Publication requires completion of the exact producer submission, under an
absolute five-second deadline. `Pending::poll` observes progress without waiting
and returns the frame once. Completion or error consumes the operation; late
polls fail. Producer redraws may proceed after submission because queue ordering
precedes those later writes. `Pool::copy` and `Pending::wait` supply synchronous
worker-only convenience and teardown paths. Native objects and both quotas remain
retained through submitted work, including cancellation and error retirement.
Keep the producer's device-loss/error monitor: WGPU completion callbacks alone
do not prove successful rendering. During the GUI paint phase, call
`frame.paint(bounds, window)` on an `Arc<Frame>`. The scene and renderer retain
that lease, its pixel buffer and its Metal mapping until the consumer GPU
command finishes. The renderer checks the producer's physical device identity.
Packed surfaces use a separate cache from NV12 video surfaces. The shader samples
premultiplied pixels and converts the filtered sample to the renderer's existing
blend convention, including zero alpha.

Native ownership follows Apple's
[Core Video texture-cache contract](https://developer.apple.com/documentation/corevideo/cvmetaltexturecachecreatetexturefromimage(_:_:_:_:_:_:_:_:_:)),
which requires keeping the CoreVideo Metal texture alive while GPU commands use
it. Holding only the underlying Metal texture does not replace that ownership.

This proves a transfer/lifetime mechanism, not physical display completion or a
performance advantage. The prototype now reuses released native images and
CoreVideo mappings, while importing them into WGPU for each lease. It uses
worker-side CPU observation of a producer fence rather than a GPU-to-GPU shared
event. Physical device-loss acceptance across two renderers, cross-device
behavior and native Linux/Windows paths still need production acceptance.
The compositor's optional Terminal adapter now supplies runtime/revision guards
and bounded asynchronous export across resize, hide, close and replacement.

Run the native constituent regression explicitly from `gpu-share/`:

```sh
cargo test --all-targets --all-features -- --include-ignored
```

It checks both formats, immutable content after source redraw, capacity charged
through cloned leases, returned capacity, monotonic IDs and rejection of invalid
geometry/format/usage. Further regressions cover queue ordering through later
redraws, one-time polling, absolute expiry, cancellation and aggregate admission
across dropped producers/replacements reusing local frame IDs. Its CPU readback
is a test oracle only. Cache regressions cover reuse through resize, distinct
logical devices on one Metal device, BGRA eviction, clearing/drop during pending
work, cloned pools, retained frames and peer isolation.
Image reuse regressions additionally cover idle quota charging, final-reference
retirement, native buffer reuse with increasing serials, old-generation rejection
across resize/clear/owner drop, and discard on expiry/cancellation. The compositor's
`--terminal-gpu-sharing-check` is the independent native-renderer/GUI acceptance
fixture. Its `--gpu-panes-render-check` additionally exercises normal registered
Terminal views in a mixed tiling shell, full-quota input, resize, hide/show,
replacement and retirement, with independent full-frame source comparisons.
See the compositor performance document for scope and invocation.

The compositor's `--gpu-panes-loss-check` additionally destroys one producer's
logical WGPU device with a submitted export still unpublished. It requires a
real loss notification and the normal export-health failure path. It checks a
consumer-held old image through explicit Restart, fresh owner identity, stale
request rejection, peer input/rendering, recovered pixels and final allocation
retirement. The developer build enables Vivido's existing `test-util` feature;
ordinary builds contain no destruction hook. This is controlled producer-device
failure on one Metal device, not physical GPU removal, a driver reset or failure
of the GUI consumer device. Those platform acceptance gates remain open.

`cargo run --release --locked --example export_cost` from `gpu-share/` measures
isolated native submission and completion cost for both formats at three sizes.
It completes a fixed source upload before timing, warms up four exports, then
records 24 sequential samples per case. It excludes source rendering, GUI
adoption and consumer/display presentation; it is not an application latency or
throughput benchmark. The compositor performance document preserves the measured
before/after pipeline-cache and image-reuse comparisons, exact source patches
and build records.
