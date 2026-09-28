-- The Lua package: one native module, `require("vivid_sdk")`, built from lua-bindings/ by
-- luarocks-build-rust-mlua for the Lua LuaRocks targets. A source-build preview: build from a
-- checkout with its sibling ../vivid_protocol, `luarocks make vivid-sdk-scm-1.rockspec`.
rockspec_format = "3.0"
package = "vivid-sdk"
version = "scm-1"

source = {
  url = "git+https://github.com/vivido-dev/vivid_sdk.git",
  branch = "dev",
}

description = {
  summary = "Vivid Protocol 1.5 producer and presenter SDK",
  detailed = [[
A native Lua module over the Rust vivid_sdk crate: surfaces, tracks, and authenticated track
channels for producers; a terminating presenter; pane overlays; and the vivido/vvmux automation
client. Loaded with a plain require; no FFI.
]],
  homepage = "https://github.com/vivido-dev/vivid_sdk",
  license = "Apache-2.0",
}

dependencies = {
  "lua >= 5.1, < 5.6",
}

build_dependencies = {
  "luarocks-build-rust-mlua",
}

build = {
  type = "rust-mlua",
  -- The crate is its own Cargo workspace inside the SDK, and the backend chooses the Lua feature
  -- (luajit, lua51 ... lua55), so the crate's LuaJIT default is switched off.
  cargo_extra_args = { "--manifest-path=lua-bindings/Cargo.toml" },
  target_path = "lua-bindings/target",
  default_features = false,
  modules = {
    vivid_sdk = "vivid_sdk_lua",
  },
}
