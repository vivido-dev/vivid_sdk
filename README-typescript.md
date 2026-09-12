# vivid-sdk for TypeScript

`@vivido/vivid-sdk` is the TypeScript SDK for Vivid Protocol 1.5, for both roles. The current local build is tested with Node 22 and Bun 1.3.14;
Deno and the full advertised runtime/platform matrix still need verification.

The package produces media; `vivid-sdk/presenter` (via `import { presenter }`) accepts it. One
artifact carries both, so a program can be either end of a session — or, as the tests do, both at
once.

It is a native addon over the Rust SDK rather than a reimplementation of it. Constants, resource
claims, and image container parsing all come from the SDK, so this package cannot disagree with the
Rust one about a wire value — and the [conformance check](conformance/README.md) proves the
agreement rather than assuming it.

```ts
import { ROLE_FIGURE, connect } from "@vivido/vivid-sdk";

const session = await connect();
const surface = await session.createSurface({
  logicalWidth: 2,
  logicalHeight: 2,
  role: ROLE_FIGURE,
  title: "pixel",
});
await session.placeTerminalSurface(surface, {
  nodeId: 1,
  width: 2,
  height: 1,
});
const track = await session.createTrack(surface, { kind: "raster", width: 2, height: 2 });
const channel = await session.openTrackChannel(track);
// One tightly packed sRGB RGBA8 frame: four red pixels.
await channel.sendRaster(Buffer.from([200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255]));
```

The record-body, in-flight, and retained-pixel claims follow from the geometry and are computed
in Rust with checked arithmetic. The same is true of `maximumEncodedBitsPerSecond` and the per-kind rate defaults, so a
track configuration here is the intent rather than a transcription of the SDK's own numbers.

Everything blocking is asynchronous. Native calls run on the addon's worker pool, so a flow wait or
a control round trip never holds the event loop.

The channel blocks only its own sender when cumulative flow maxima are exhausted. Control,
unrelated tracks, and realtime audio remain independently serviced. `channel.eos()` writes EOS after
the last media record on that same connection.

Use `waitTrack()` with a bounded timeout before activation or when waiting for presentation,
playback, channel, or loss milestones.

## Events

Session and channel events are async iterators. Each step parks a worker on a bounded wait, so the
loop costs nothing while the session is quiet:

```ts
for await (const event of session.events()) {
  if (event.kind === "anchorReady") {
    // ...
  }
}
```

The iteration ends at `connectionClosed` — the last event a session produces — or when the session
is closed underneath it. Passing `{ signal }` stops it sooner.

```ts
const controller = new AbortController();
for await (const event of channel.events({ signal: controller.signal })) { /* ... */ }
```

## Errors

Every rejection is a class. `VividError` carries a `code`, and `ClosedHandleError` extends it for a
handle used after it was closed, so a caller can tell "this session is finished" from "this request
was refused" without matching on a message:

```ts
import { ClosedHandleError, VividError } from "@vivido/vivid-sdk";

try {
  await channel.sendRaster(frame);
} catch (error) {
  if (error instanceof ClosedHandleError) return;
  if (error instanceof VividError) console.error(error.code, error.message);
  throw error;
}
```

## Presenter

A presenter binds an endpoint, issues a capability per pane, and holds the retained scene a
producer sends. Reading a pane is pull-based: there are no callbacks and no queue to drain, so a
slow reader cannot stall the presenter, and `waitForMedia` is a bounded wait for something to read.

```ts
import { presenter } from "@vivido/vivid-sdk";

const running = await presenter.Presenter.start({ endpoint: "tcp:127.0.0.1:0" });
try {
  running.updateMetrics(1, 80, 24, 8, 16);
  const capability = running.issuePaneCapability(1); // hand to exactly one producer

  // ... a producer connects to running.endpoint() with that capability and sends a frame ...

  if (await running.waitForMedia(1, 5000)) {
    for (const layer of (await running.capturePane(1)).layers) {
      layer.contentKind; // "raster" | "encodedImage"
      layer.pixels;      // the decoded frame, for a raster layer
    }
  }
} finally {
  await running.close();
}
```

`tcp:` endpoints are restricted to loopback, and port 0 binds an ephemeral port that
`running.endpoint()` then reports. A Unix path is created owner-only and removed on close.

A pane capability is capability material: hand it to one producer over something that is not a
command line, and do not log it. It does not appear in `inspect`, `JSON.stringify`, or a spread.

`capturePane` composes the producer's own retained surfaces. It is not a screenshot — terminal text
belongs to a renderer, and this presenter has none. A capture that produced nothing says why in
`skipped`: `undecoded_video` will never produce pixels here, while `no_retained_pixels` is worth
retrying.

## Pane sessions

`PaneSession` is the one-image-per-pane convenience layer, and the shortest path from a frame to a
screen:

```ts
import { PaneSession } from "@vivido/vivid-sdk";

await using pane = await PaneSession.connect();
await pane.showEncodedImage(png, { title: "figure" });
await pane.showRgba(width, height, rgba);
await pane.clear();
```

`await using` closes the presentation and the session when the block exits. The node, surface, and
track lifecycle is the SDK's; this class adds discovery options and nothing else.

## Automation client

`automation` drives the terminal runtimes over their local automation endpoints — the same socket
the CLIs use, without spawning one: `vivido` (windows, keys, grid reads, waits), `vivida` (which
embeds vivido's host and adds workspace layout on the same endpoint), and `vvmux` (panes, tabs,
agents, over the VVMX framing).

```ts
import { automation } from "@vivido/vivid-sdk";

const v = await automation.vividoConnect({ target: "scratch" });
const before = (await v.request("inspect", { window_id: 1 })).window.sequences.screen;
await v.request("typing", { text: "npm test", window_id: 1 });
await v.request("wait_text", {
  text: "passing",
  regex: false,
  after_screen: before,
  common: { timeout: 30000, target: { window_id: 1 } },
});
v.close();

const m = await automation.vvmuxConnect("default");
const panes = await m.request({ method: "list_panes" });
m.close();
```

Params mirror the *serde* shape of each runtime's request struct, not its CLI flags: clap's
`flatten` and defaults do not apply to serde, so structs flattened on the command line arrive as
nested objects (`target`, `common`), and fields the CLI would default are still required on the
wire when the struct lacks `#[serde(default)]`. A `hello` handshake on connect answers with the
capability document naming what the instance claims.

Errors are typed: a failed reply throws `AutomationError` carrying the runtime's `code`, `message`,
and optional `data`. Discovery follows the CLI's own order, and a named target that has gone away is
an error, never a silent fall-through to another instance.

Unix only, and one operating-system account of trust: every socket is owner-checked before a byte is
written, and registries are only read from a plain, owner-only runtime directory with identity
derived from the session name rather than taken from the registry.

**One difference from the Python client, stated rather than glossed.** Python also checks the
connected peer's credentials after connecting — `SO_PEERCRED` on Linux, `getpeereid` on macOS.
Node has no binding for either, so that check does not run here. The pre-connect owner check does,
and it is the one that decides whether the socket at that path is yours; but a socket whose owner
changed between the `lstat` and the `connect` would be missed, and Python would catch it.

## Discovery and secrets

By default, discovery reads `VIVID_ENDPOINT_CONTROL`, optional interactive/realtime/bulk endpoint
variables, and `VIVID_ROOT_SECRET`. Explicit values are options. Secrets and channel tags are absent
from handle representations and errors, and `Session` exposes no accessor for its channel key.

Passing `rootSecret` as an option puts the value through JavaScript, where the environment form
does not; prefer the environment.

## From 1.5

Version 2.0 replaces the old subprocess wrapper. See
[MIGRATING-node-1.5-to-2.0.md](MIGRATING-node-1.5-to-2.0.md).

## Current limits

This is a source-build preview: npm platform prebuild packages and optional dependencies are not
wired for distribution yet. Run `npm ci` and `npm run build:debug` in this checkout. The sibling
`vivid_protocol` branch is a build prerequisite.

Integer inputs must be finite safe JavaScript integers and fit their destination field. Invalid
values reject; they are never narrowed or clamped to another ID. Full-width wire `u64`/`i64`
values still require a lossless public representation: existing `number` output DTOs are not full
integer parity with Rust/Python. Lease/resume identity queries also do not yet provide an
activation/resume connection workflow. Neither area should be treated as complete parity.
