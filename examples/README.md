# Progressive Vivid SDK examples

Seven runnable examples in each language use the current `dev` SDK and Vivid Protocol 1.5.
They use the public Rust, Python, and native TypeScript APIs. Start with the pane helpers,
then learn the explicit surface/track lifecycle and the terminating presenter.

| Level | Topic | Rust | Python | TypeScript |
|---|---|---|---|---|
| Basic | Show a PNG/JPEG | [01](rust/01_show_image.rs) | [01](python/01_show_image.py) | [01](typescript/01_show_image.ts) |
| Basic | Generate four RGBA pixels | [02](rust/02_generated_raster.rs) | [02](python/02_generated_raster.py) | [02](typescript/02_generated_raster.ts) |
| Intermediate | Session, surface, track, placement, activation | [03](rust/03_surface_and_track.rs) | [03](python/03_surface_and_track.py) | [03](typescript/03_surface_and_track.ts) |
| Intermediate | Finite raster animation | [04](rust/04_raster_animation.rs) | [04](python/04_raster_animation.py) | [04](typescript/04_raster_animation.ts) |
| Advanced | Replace a track on the same surface | [05](rust/05_replace_track.rs) | [05](python/05_replace_track.py) | [05](typescript/05_replace_track.ts) |
| Advanced | Producer/presenter round trip | [06](rust/06_presenter_roundtrip.rs) | [06](python/06_presenter_roundtrip.py) | [06](typescript/06_presenter_roundtrip.ts) |
| Advanced | Interactive vector overlay window | [07](rust/07_overlay_window.rs) | [07](python/07_overlay_window.py) | [07](typescript/07_overlay_window.ts) |

## Setup

Run every command below from the SDK directory, `vivid_sdk/`. This repository uses a sibling
`../vivid_protocol` checkout; use the protocol revision required by the SDK's current `dev`.
Rust requires 1.88 or newer. Python requires 3.9 or newer. Use Node 22.20+ for the native
TypeScript build tools; these examples do not require Bun or browser support.

Examples 01â€“05 and 07 run **inside a Vivid-enabled Vivido pane**, inheriting `VIVID_ENDPOINT_CONTROL`,
the optional lane endpoints, and `VIVID_ROOT_SECRET`. Do not copy credentials into command
arguments or print them. Media uses authenticated SDK connections, never the terminal PTY.
Example 06 starts its own loopback presenter and needs no terminal or external credentials.

Python source installation:

```sh
uv venv --python 3.12
uv pip install maturin "mypy>=1.8,<2" "pytest>=7.4"
# Activate .venv: source .venv/bin/activate (POSIX)
# On PowerShell: .venv/Scripts/Activate.ps1
maturin develop
```

TypeScript source installation (native addon and public package declarations):

```sh
npm ci
npm run build:debug
npm run build:examples
```

The TypeScript examples import the local package by its public name, `@vivido/vivid-sdk`.
Compilation writes JavaScript into `target/examples-typescript/`; no global TS runner is needed.
The native package is a source-build preview; these instructions do not assume published prebuilds.

## Run

Replace `image.png` with a PNG or JPEG path, quoted if it contains spaces.

```sh
cargo run --example sdk_01_show_image -- image.png
cargo run --example sdk_02_generated_raster
cargo run --example sdk_03_surface_and_track
cargo run --example sdk_04_raster_animation
cargo run --example sdk_05_replace_track
cargo run --example sdk_06_presenter_roundtrip --features presenter
cargo run --example sdk_07_overlay_window
```

```sh
python examples/python/01_show_image.py image.png
python examples/python/02_generated_raster.py
python examples/python/03_surface_and_track.py
python examples/python/04_raster_animation.py
python examples/python/05_replace_track.py
python examples/python/06_presenter_roundtrip.py
python examples/python/07_overlay_window.py
```

```sh
node target/examples-typescript/01_show_image.js image.png
node target/examples-typescript/02_generated_raster.js
node target/examples-typescript/03_surface_and_track.js
node target/examples-typescript/04_raster_animation.js
node target/examples-typescript/05_replace_track.js
node target/examples-typescript/06_presenter_roundtrip.js
node target/examples-typescript/07_overlay_window.js
```

Examples 01, 02, 03, and 05 wait for Enter and then remove the presentation. Example 07 instead
runs its own event loop, so its `--duration` bounds that loop rather than a blocking wait. Add
`--duration 2` to remove it automatically after two seconds (0â€“3600 accepted).
For Cargo, application arguments follow `--`:

```sh
cargo run --example sdk_03_surface_and_track -- --duration 2
python examples/python/03_surface_and_track.py --duration 2
node target/examples-typescript/03_surface_and_track.js --duration 2
```

Example 04 sends 90 frames over roughly three seconds and exits. Example 05 first shows the
four-color frame for one second, then switches to an amber 4 Ã— 4 raster; its duration applies
after replacement. Example 06 exits after verifying a single 2 Ã— 2 raster and the exact red,
green, blue, white pixel bytes. A timeout or mismatch fails the process.

## What to observe

- **01â€“02:** the pane convenience API owns the presentation. Keeping the handle alive retains
  the image; `PaneSession.close` clears it before closing.
- **03:** a stable surface owns an immutable track. The first frame is submitted, output readiness
  is awaited for at most five seconds, and the track is activated. The image appears in the
  top-left 16 Ã— 8 terminal cells. This explicit example uses grid placement rather than an anchor.
- **04:** one sequential sender rotates the four pixels, with monotonically increasing frame IDs
  and pacing below the track's default rate. Every send is awaited in Python/TypeScript; blocked
  flow cannot accumulate an unbounded queue. EOS follows the final frame on the same channel.
- **05:** a replacement is created and primed while the old track remains active. Readiness precedes
  slot activation; only then is the old track destroyed. The surface, node, and logical 2 Ã— 2
  coordinate system remain unchanged when the encoded raster resolution becomes 4 Ã— 4.
- **06:** a terminating presenter captures retained media, not a desktop screenshot. Exact byte
  assertions independently check pixel order; agreement between languages alone is insufficient.
- **07:** the overlay stack, which needs the complete profile bundle rather than a raster track.
  A vector display list carries paths, a stroke, host-shaped text, and one application hit region;
  the host rasterizes it above the terminal glyphs and clips it to the pane. The event loop shows
  why `targets` exists: one session may own many windows, and outcomes and viewport snapshots
  share the same lane as input. Exiting requires a click inside the declared region, Escape, or
  `--duration`.

Rust and Python placement take signed **32.32 fixed-point cells** (`16 << 32`); TypeScript takes
ordinary cell numbers (`16`). Producer track waits take **microseconds** in Rust/Python and
**milliseconds** in TypeScript. Presenter waits take Rust `Duration`, Python **seconds**, and
TypeScript **milliseconds**.

The explicit examples delete their node and surface before closing. Current presenters may retain
an anchored poster on clean GOODBYE, so simply closing an arbitrary session is not equivalent to
removing its presentation. The pre-existing [Python image command](python/vivid_image.py) deliberately
uses that poster behavior; its old `python examples/vivid_image.py IMAGE` path remains available.

No lease/resume, desktop injection, microphone, or encoded-video parity is claimed here.
TypeScript fixture IDs stay within its safe integer range.

## Verification

```sh
cargo fmt --all --check
cargo test --workspace --all-targets
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
python -m mypy examples/python
python -m pytest
npm run typecheck
npm run typecheck:examples
npm test
```

Run all three 06 commands above for bounded live-socket verification. To verify visible placement,
animation, replacement, and removal, run 01â€“05 inside a real pane; a successful headless presenter
round trip does not establish that the terminal compositor rendered the examples.
