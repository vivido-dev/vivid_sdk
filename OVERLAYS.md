# Overlay implementation status

The interactive overlay facility is in development and is **not yet available as an SDK API**.
`OverlaySession` and `OverlayWindow` are not exported. Direct Vivido and the terminating SDK
presenter do not currently negotiate the new overlay profiles. Configuring the terminating
presenter to advertise them fails explicitly.

Existing pane images can use `PaneImageOptions.text_layer = 2` in Rust, or the corresponding
`text_layer` / `textLayer` option in the Python / TypeScript pane-image helpers, to appear above
terminal text. This changes composition order; it does not provide popup focus, modal input,
viewport coordinates, or vector drawing.

Implemented foundations:

- Separate GPU composition for existing text layers 0, 1, and 2, with GPU readback regression tests.
- Bounded portable display-list, vector-frame, image-asset, and negotiated-limit codecs.
- Owner-scoped window state for modal eligibility, popup dismissal, focus restoration, capture,
  drag/resize, and bounded input queues.
- Vello scene compilation with matching transformed/clipped hit regions and host text shaping.
- Explicit rejection of unsupported overlay profiles by the terminating presenter/gateway.

Still required before enabling the profiles:

- Complete control and interactive-lane codecs/dispatch, authoritative viewport/DPI notifications,
  input-lane liveness, host input/IME routing, and live window lifecycle integration.
- Bounded worker scheduling, retained-asset accounting, atomic scene publication and explicit
  superseded/presented outcomes integrated with track credit and readiness.
- Rust high-level handles, matching Python blocking/async facades, and native asynchronous
  TypeScript APIs with full-width identities represented as bigint.
- Text measurement, resource/multi-owner stress tests, full language workflows, benchmarks, and
  live pane visual/input acceptance checks.

The protocol extension is specified in
[the overlay specification](../vivid_protocol/vivid-protocol-1.5-overlays.md).
Existing examples are intentionally unchanged.

## Validation of the current foundations

- Protocol: all-target tests and Clippy pass; the added input-state tests also pass.
- Gateway: all 21 tests and Clippy pass.
- SDK: default workspace tests pass. All-feature tests pass when excluding the existing Windows
  `drop_wakes_an_incomplete_control_handshake` timeout; that test still fails. Clippy passes.
- Vivido: both new GPU regressions pass. The workspace suite reports 703 passed, 4 ignored, and
  one IPC capability-classification failure that passes when rerun alone. Clippy passes.
- Python: native build and 27 SDK/presenter tests pass with a workspace-local temporary directory.
  Mypy passes with the Unix automation platform selected. The full Windows suite cannot run
  Unix-only automation tests (`AF_UNIX`, `geteuid`, and `uname`).
- TypeScript: native build and typecheck pass; 17 tests pass. The existing security test fails
  because it turns a Windows file URL into an invalid `F:\F:\...` path.
- No live overlay window/input acceptance run or overlay performance benchmark has been completed.

These checks validate the foundations, not the unfinished interactive overlay facility.
