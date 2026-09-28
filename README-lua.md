# vivid_sdk for Lua

See [pane overlays](docs/OVERLAYS.md#lua) for interactive windows, submission receipts, and viewport events.

See the [progressive examples](examples/README.md) for runnable tutorials in each language, from displaying an image to a producer/presenter round trip.

`vivid_sdk` is the Lua SDK for Vivid Protocol 1.5, for both roles. It is a native Lua module
built from the Rust SDK with [mlua](https://github.com/mlua-rs/mlua), so it loads with a plain
`require` — no `ffi.load`, no `ffi.cdef`, and no Lua source to install beside it:

```lua
local vivid = require("vivid_sdk")
```

The module produces media; `vivid.presenter` accepts it. One library carries both, so a program
can be either end of a session — or, as the tests do, both at once. It is a binding over the Rust
SDK rather than a reimplementation: constants, resource claims, and image container parsing all
come from Rust, so it cannot disagree with the Rust, Python, or TypeScript packages about a wire
value, and the [conformance check](conformance/README.md) compares it with them.

LuaJIT, Lua 5.1, 5.2, 5.3, 5.4, and 5.5 are supported, one per build. Rust 1.88+ is required.

## Building

A module is built for exactly one Lua. From the SDK directory:

```sh
lua/build.sh                  # LuaJIT, the default; also what Neovim embeds
lua/build.sh --lua lua54      # or lua51, lua52, lua53, lua55
lua/build.sh --release
```

The script builds `lua-bindings/` and stages the library as `lua/vivid_sdk.so` (`vivid_sdk.dll`
on Windows). Put that directory on `package.cpath`:

```sh
LUA_CPATH="lua/?.so;;" luajit -e 'print(require("vivid_sdk").VERSION)'
```

The file may be copied anywhere on `package.cpath`; its name is what `require` looks for. The
submodules resolve from the same file, so `require("vivid_sdk.presenter")`,
`require("vivid_sdk.overlay")`, and `require("vivid_sdk.automation")` work with or without the
root loaded first, and reach the same tables as `vivid.presenter` and friends.

LuaRocks builds through [`luarocks-build-rust-mlua`](https://github.com/mlua-rs/luarocks-build-rust-mlua),
which selects the feature for the Lua it targets. From a checkout:

```sh
luarocks make vivid-sdk-scm-1.rockspec
```

Like the Python and Node packages, this is a source-build preview: the sibling `vivid_protocol`
checkout is a build prerequisite, and no prebuilt rocks are published yet. `lua-bindings/` is a
Cargo workspace of its own, because a build targets one Lua and mlua's Lua features are mutually
exclusive.

Editor support: `lua/types/vivid_sdk.lua` is a lua-language-server definition file for the whole
module. Add `lua/types` to `workspace.library`. It is never loaded at runtime.

## Producing

```lua
local vivid = require("vivid_sdk")

local session = vivid.connect()
local surface = session:create_surface({
  logical_width = 2,
  logical_height = 2,
  role = vivid.ROLE_FIGURE,
  title = "pixel",
})
session:place_terminal_surface(surface, { width = 2, height = 1 })
local track = session:create_track(surface, { kind = "raster", width = 2, height = 2 })
local channel = session:open_track_channel(track)
-- One tightly packed sRGB RGBA8 frame: four red pixels. Lua strings are byte strings.
channel:send_raster(string.rep("\200\0\0\255", 4))
session:close()
```

Handles are userdata with methods: `session:create_track(surface, config)`,
`channel:send_raster(rgba)`. Configuration is a table. The record-body, in-flight, and
retained-pixel claims follow from the geometry and are computed in Rust with checked arithmetic,
as are the per-kind rate defaults, so a track configuration states intent rather than a
transcription of the SDK's numbers. `session:build_track_config(surface, config)` and
`session:build_surface_config(config)` show what would be sent.

A channel keeps its own raster frame IDs, which the SDK requires to be nonzero and strictly
increasing: the first frame and the thousandth are both just `send_raster(rgba)`. Pass
`{ frame_id = n }` to drive the sequence yourself; the channel continues after it.

Every call blocks the calling Lua state, as Lua expects. The channel blocks only its own sender
when cumulative flow maxima are exhausted; control, unrelated tracks, and realtime audio remain
independently serviced in Rust. `channel:eos()` writes EOS after the last media record on that
same connection. Use `session:wait_track(track, condition, value, timeout)` with a bounded timeout
before activation, or when waiting for presentation, playback, channel, or loss milestones.

For a PNG or JPEG:

```lua
local presentation = vivid.display_image("image.png")
io.read("*l") -- the image stays while the presentation is open
presentation:close()
```

The image is anchored at the cursor when the terminal has a text plane, and placed against the
grid otherwise. `presentation.session:close()` instead ends the session cleanly, which lets the
presenter keep the anchored image as a poster after the process exits.

`vivid.PaneSession` is the one-image-per-pane convenience layer:

```lua
local pane = vivid.PaneSession.connect()
pane:show_encoded_image(png, { title = "figure" })
pane:show_rgba(width, height, rgba)
pane:clear()
pane:close()
```

Beyond the core, the same handles cover what the Rust SDK does: desktop input lanes
(`session:open_input_lane()`), file drops and incoming transfers
(`session:set_file_drop_binding{...}`, `session:open_incoming_file_transfer{...}`), channel
recovery with continuous packet IDs (`session:recover_channel(track, key_unit)`), encoder pacing
(`vivid.VideoRateControl.new(bps)`), desktop orchestration (`vivid.establish_desktop(...)`), and
contexts, leases, and resume identity (`session:create_context{...}`,
`session:create_session_lease{...}`, `session:prepare_resume()`).

## Conventions

- **Names** are snake_case, as in the Rust and Python SDKs. Constants are fields of the module —
  `vivid.ROLE_FIGURE`, `vivid.WAIT_MILESTONE_SET` — read at load time from the Rust table that
  owns them; nothing in this module writes a protocol number.
- **Timeouts** are seconds, as a local number: `session:wait_event(0.5)`. Wire durations stay in
  microseconds and keep their `_us` names: `pts_us`, `requested_watchdog_us`.
- **Placement** takes ordinary cell numbers — `{ width = 16, height = 8 }`, fractions allowed —
  and scales them to the wire's 32.32 fixed point, because Lua 5.1 and LuaJIT have no 64-bit
  shift to write `16 << 32` with.
- **Integers** must be integral, in range, and exact. LuaJIT, 5.1 and 5.2 carry numbers as doubles,
  exact below 2^53; 5.3 and later have 64-bit integers, exact to 2^63 - 1. A value outside that is
  refused, never rounded into another ID. Values the SDK returns above those limits cannot be
  carried exactly; the SDK never allocates such IDs itself, and handles such as a retained overlay
  image are passed as handles rather than numbers so their identities stay whole.
- **Configuration tables** are checked for unknown keys, so `{ logical_widht = 2 }` is an error
  rather than a silently ignored field. Booleans must be booleans; `compress = 1` is refused.
- **Bytes** are Lua strings: pixels, encoded images, PCM, a digest.
- **Payloads** — event and status maps the wire keys by integer — are tables keyed by those
  integers, `payload[0]`, with arrays as sequences and `null` as absent.
- **Iteration**: `for event in session:events() do ... end` ends at `connection_closed`, at a quiet
  timeout (30 seconds by default), or when the session is closed underneath it.
- **Closing**: dropping a handle is an unclean loss, like dropping a Rust `Session`; `close()` is
  the clean one. On Lua 5.4 and later every closable handle supports `local s <close> = ...`.
- **Lua has no sleep or monotonic clock**, so the module provides `vivid.sleep(seconds)` and
  `vivid.monotonic()` for pacing frames and bounding loops.

## Errors

Every failure is raised as an ordinary Lua error, so an unhandled one stops a script with its
message and a traceback. To act on the kind, pass the caught value to `vivid.error_info`:

```lua
local ok, err = pcall(channel.send_raster, channel, frame)
if not ok then
  local info = vivid.error_info(err)
  if info and info.kind == "closed" then return end
  error(err, 0)
end
```

`info.kind` is `closed` (a handle used after it was closed — Python's `ClosedHandleError`),
`invalid` (refused before anything was sent — Python's `ValueError`), `vivid` (a presenter
refused, or the transport or protocol failed — `VividError`), or `automation`. `info.message` is
the message without the traceback. A presenter rejection carries its registered `code` and
whether it was `fatal`; an automation refusal carries the runtime's `code` and optional `data`.
`error_info` returns `nil` for anything that is not an SDK error.

## Presenter

A presenter binds an endpoint, issues a capability per pane, and holds the retained scene a
producer sends. Reading a pane is pull-based: there are no callbacks and no queue to drain, so a
slow reader cannot stall the presenter, and `wait_for_media` is a bounded wait for something to
read. The presenter's threads are pure Rust and never enter Lua.

```lua
local running = vivid.presenter.start("tcp:127.0.0.1:0") -- or "unix:/run/user/1000/vivid.sock"
running:update_metrics(1, { columns = 80, rows = 24 })
local capability = running:issue_pane_capability(1) -- hand to exactly one producer

-- ... a producer connects to running.endpoint with that capability and sends a frame ...

if running:wait_for_media(1, 5) then
  for _, layer in ipairs(running:capture_pane(1).layers) do
    local frame = layer.content -- kind "raster" (width, height, rgba) or "encoded_image" (data)
  end
end
running:close()
```

`tcp:` endpoints are restricted to loopback, and port 0 binds an ephemeral port that
`running.endpoint` reports. A Unix path is created owner-only.

A pane capability is capability material: hand it to one producer over something that is not a
command line, and do not log it. It never appears in a handle's `tostring`. A minted lease's
activation secret is returned as a second value from `create_session_lease` and is never part of
the lease table, for the same reason.

`capture_pane` composes the producer's own retained surfaces. It is not a screenshot — terminal
text belongs to a renderer, and this presenter has none. A capture that produced nothing says why
in `skipped`: `undecoded_video` will never produce pixels here, while `no_retained_pixels` is
worth retrying.

## Automation client

`vivid.automation` drives the terminal runtimes over their local automation endpoints — the same
socket the CLIs use, without spawning one: `vivido`, `vivida` (which embeds vivido's host and adds
workspace layout on the same endpoint), and `vvmux` (over the VVMX framing).

```lua
local auto = vivid.automation

local v = auto.vivido_connect({ target = "scratch" }) -- or socket = ..., or nothing to discover
local before = v:request("inspect", { window_id = 1 }).window.sequences.screen
v:request("typing", { text = "make test", window_id = 1 })
v:request("key", { key = "Enter", mods = auto.array(), ["repeat"] = 1, route = "application",
  target = { window_id = 1 } })
v:close()

local m = auto.vvmux_connect("default")
local panes = m:request({ method = "list_panes" })
m:close()
```

Params mirror the serde shape of each runtime's request struct, not its CLI flags. JSON has arrays
and objects where Lua has tables, so an empty list is `auto.array()` and an explicit null is
`auto.null`; a JSON `null` in a reply reads back as `nil`. vvmux envelope fields — `pane_id`,
`agent`, `expect`, and the rest — are the second argument to `request`, beside the method record.

Errors are typed: a refusal raises an `automation` error whose `code` is the runtime's
(`window_not_found`, `method_not_supported`, ...). Resolution failures use the client-side codes
`endpoint_not_found`, `endpoint_unsafe`, and `invalid_session_name`; malformed requests
`invalid_request`, and oversized frames `limit_exceeded`. Discovery follows the CLI's own order,
and a named target that has gone away is an error, never a silent fall-through to another instance.

Unix only, and one operating-system account of trust, with the same checks as the Python client:
every socket is owner-checked before a byte is written and peer-credential-checked after connect;
registries are read only from a plain, owner-only runtime directory; and identity is derived from
the session name rather than taken from the registry.

## Testing

```sh
lua/build.sh && LUA_CPATH="lua/?.so;;" luajit lua-tests/run.lua
```

The suite needs nothing but the module and a Lua. Automation tests talk to fake servers written in
Python's standard library, because Lua has no sockets; they are skipped without `python3` (or
`VIVID_TEST_PYTHON`). The live overlay check is Vivido's native binding test, which runs
`bindings/tests/overlay_lua.lua` against a real host with GPU readback; see
[binding validation](docs/OVERLAYS.md#binding-validation).

## Current limits

- Lease and resume identity queries do not yet provide an activation or resume connection
  workflow, as in the other bindings.
- Overlay hit IDs, revisions, and asset IDs above the integer limits above cannot be written or
  read exactly from Lua; Python and TypeScript carry them at full width.
- No prebuilt rocks are published, and Windows builds are untested; the automation client is Unix
  only.
