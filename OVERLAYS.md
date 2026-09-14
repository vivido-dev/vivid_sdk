# Overlay implementation status

The interactive overlay facility is in development and is **not yet usable end to end**.
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
- Typed window control, viewport/status, capture/renewal, and input-event codecs shared with the
  presenter. Input carries the published scene revision independently of window geometry; motion
  coalescing cannot cross scene revisions. Window actions preserve popup stacking and check owners,
  generations, and revisions. Wheel routing respects modal eligibility and input transparency.
- Rust `TrackChannel::send_vector` and `send_vector_asset`, with authenticated negotiated limits,
  retained-image budgeting, increasing scene revisions, and ordered EOS. Oversized records fail
  before spending channel credit. Asset release and publication outcomes are still outstanding.
- Rust `Session::open_overlay_input_lane`, returning typed `OverlayLaneEvent` values with bounded
  capture/renewal requests. Overflow or request timeout retires the lane; close shuts down the
  transport and reports connection loss once. The host does not yet accept this lane profile.
- Overlay profile, vector-kind, and vector-slot constants in all three language packages.

Still required before enabling the profiles:

- Live control and interactive-lane dispatch, authoritative viewport/DPI notifications,
  input-lane liveness, host input/IME routing, and live window lifecycle integration.
- Bounded worker scheduling, retained-asset accounting, atomic scene publication and explicit
  superseded/presented outcomes integrated with track credit and readiness.
- Rust high-level handles, matching Python blocking/async facades, and native asynchronous
  TypeScript APIs with full-width identities represented as bigint.
- Text measurement, resource/multi-owner stress tests, full language workflows, benchmarks, and
  live pane visual/input acceptance checks.
- The GPUI-inspired declarative UI engine, Taffy layout, rich text/editing/IME services, semantic
  accessibility bridge, image/SVG/GIF loading, and the separate three-language demo gallery.

The GPUI-inspired implementation is not complete. No declarative UI module, high-level overlay
window API, or gallery is exported by this increment. Existing examples and Zed remain unchanged.

The protocol extension is specified in
[the overlay specification](../vivid_protocol/vivid-protocol-1.5-overlays.md).
Existing examples are intentionally unchanged.

## Validation of the current foundations

- Protocol: all-target tests and Clippy pass; the added input-state tests also pass.
- Gateway: all 21 tests and Clippy pass.
- SDK: default all-target tests and all-feature workspace tests pass without exclusions, including
  109 default and 155 all-feature library tests. The incomplete-handshake shutdown regression now
  passes on Windows with cancellable establishment reads. Workspace Clippy passes.
- Vivido: workspace all-target tests and Clippy pass, including the GPU regressions. The main
  library reports 704 passed and 4 ignored; the IPC classification test passes in this run.
- Python: native build and 27 SDK/presenter tests pass with a workspace-local temporary directory.
  Mypy passes with the Unix automation platform selected. The full Windows suite cannot run
  Unix-only automation tests; the full-suite attempt stopped after three missing-`AF_UNIX` failures.
- TypeScript: native build, typecheck, and all 18 tests pass. The security test now converts its
  source-directory file URL with `fileURLToPath`, which works on Windows as well as Unix.
- No live overlay window/input acceptance run or overlay performance benchmark has been completed.

These checks validate the foundations, not the unfinished interactive overlay facility.
