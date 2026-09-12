# Cross-language conformance

Build the Python extension and Node addon, then run from the SDK directory:

```sh
npm run test:conformance
```

The runner executes Rust, Python, and TypeScript scenarios against real loopback presenters.
`VIVID_CONFORMANCE_PYTHON=/absolute/path/to/python` selects an existing Python environment;
otherwise the runner uses `uv run python`.

Reports cover public constant exports, an exact four-color raster capture, and rejection of zero
and oversized raster widths. Submodule-export tests additionally check lease/file-drop/microphone
constants. Rust/protocol tests remain the authority for cases not included here.

Differential comparison catches drift between languages. It cannot detect a bug all three share.
Independent assertions therefore check the exact fixture pixels, capture shape, invalid-geometry
results, and the protocol microphone packet shape: 20 ms of 48 kHz mono s16LE is 1920 bytes.

Add scenarios in `examples/conformance.rs`, `conformance/scenario.py`, and
`conformance/scenario.mjs`, plus independent expectations in `validateReport`. Keep waits bounded
and close handles in teardown. This suite does not establish full API parity, live terminal-pane
integration, or distribution compatibility.

`compare.test.mjs` verifies that comparison rejects changed nested constants, empty reports,
missing or additional keys, wrong pixels, missing validation, and a shared wrong microphone
constant. Serialize with ordinary `JSON.stringify`; an array replacer is a recursive property
allowlist and can silently erase nested data. Structural comparison and negative tests guard that historical error.
