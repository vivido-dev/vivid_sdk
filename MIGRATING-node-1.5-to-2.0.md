# Migrating `@vivido/vivid-sdk` from 1.5 to 2.0

Version 2.0 replaces the package's implementation. The 1.5 package did not speak Vivid: it spawned
the `vivi` binary and let that process do the work, which meant a Vivid producer had to be
installed beside it and the package's API could only be as rich as the CLI's flags. 2.0 is a
napi-rs addon over the Rust SDK, so the protocol is in-process and the API is the SDK's.

## What stays

`PaneSession` keeps its method names and signatures. The common case — show an image in a pane —
is a one-line change, or no change at all beyond the version:

```js
import { PaneSession } from "@vivido/vivid-sdk";

const pane = await PaneSession.connect();
await pane.showRgba(width, height, rgba);
await pane.showEncodedImage(png);
await pane.clear();
await pane.close();
```

`from_env` is now `connect`, and every method is asynchronous.

## What is gone

The package no longer runs a subprocess, so the things that described one are gone:

| 1.5 | 2.0 |
|---|---|
| `viviPathFromEnv(env)` | removed — there is no binary to locate |
| `VVMUX_VIVI_BIN`, `VVMUX_VIVI_PROTOCOL_VERSION` | not read |
| `pane.active` (the child process) | removed — there is no child |
| `ShowOptions.zoom` | removed — zoom was a `vivi` flag |
| `ShowOptions.inline`, `ShowOptions.noWait` | removed for the same reason |

A capability passed through `VVMUX_VIVI_BIN`'s environment now reaches the SDK through the
standard discovery variables (`VIVID_ENDPOINT_*`, `VIVID_ROOT_SECRET`), read on the Rust side
rather than in JavaScript.

`ShowOptions` is replaced by `PaneImageOptions { title, columns, rows, textLayer }`, which is what
the pane actually takes.

## What is new

The rest of the SDK: session events, queries, timed playback, scene nodes, input lanes, file
drop, session leases, media resources, and the presenter role. See `README-typescript.md`.

## Platform support

2.0 ships per-platform prebuilt addons. Node 20 or newer; Bun and Deno work through the same
addon. A browser presenter is a different artifact — this package is Node-side.
