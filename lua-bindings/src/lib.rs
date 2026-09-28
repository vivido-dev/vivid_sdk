//! Native Lua module for the Vivid Protocol 1.5 SDK, both roles in one artifact.
//!
//! ```lua
//! local vivid = require("vivid_sdk")
//! ```
//!
//! The mirror of `python-bindings/` and `node-bindings/`: handles with methods, configuration as
//! tables, and no protocol logic. Constants, resource claims, and image inspection all come from
//! `vivid_sdk`, so this module cannot disagree with the Rust SDK about a wire value. Every call
//! blocks the calling Lua state, as Lua expects; presenter and reader threads are pure Rust and
//! never enter Lua.

#![allow(clippy::type_complexity)]

mod automation;
mod convert;
mod desktop;
mod error;
mod file_drop;
mod input;
mod lease;
mod overlay;
mod pane;
mod pipeline;
mod presenter;
mod session;

use std::sync::OnceLock;
use std::time::Instant;

use mlua::prelude::*;
use vivid_sdk::ConstantValue;

use crate::convert::timeout;

/// The names under which `require` finds this module and its submodules.
const ROOT: &str = "vivid_sdk";
const SUBMODULES: &[&str] = &["presenter", "overlay", "automation"];

fn constant_table(lua: &Lua, _: ()) -> LuaResult<LuaTable> {
    let entries = lua.create_table()?;
    for (name, value) in vivid_sdk::constant_table() {
        let entry = lua.create_table()?;
        entry.set("name", *name)?;
        entry.set("text", value.as_text())?;
        entry.set("number", value.as_number())?;
        entries.push(entry)?;
    }
    Ok(entries)
}

/// `(encoding, width, height, encoded_length)` from PNG or JPEG header metadata, walked in Rust
/// beside the configuration it produces.
fn probe_encoded_image(lua: &Lua, data: LuaString) -> LuaResult<LuaTable> {
    let image = vivid_sdk::probe_encoded_image(&data.as_bytes()).map_err(error::io)?;
    let table = lua.create_table()?;
    table.set("encoding", image.encoding)?;
    table.set("width", image.width)?;
    table.set("height", image.height)?;
    table.set("encoded_length", image.encoded_length)?;
    Ok(table)
}

/// Block this Lua state for `seconds`. Lua has no portable sleep, and pacing frames needs one.
fn sleep(_: &Lua, seconds: LuaValue) -> LuaResult<()> {
    let duration = timeout(seconds, std::time::Duration::ZERO, "seconds")?;
    std::thread::sleep(duration);
    Ok(())
}

/// Seconds on a monotonic clock, for deadlines. Lua's `os.clock` is CPU time and `os.time` counts
/// whole wall-clock seconds, and neither is right for a bounded wait.
fn monotonic(_: &Lua, _: ()) -> LuaResult<f64> {
    static START: OnceLock<Instant> = OnceLock::new();
    Ok(START.get_or_init(Instant::now).elapsed().as_secs_f64())
}

fn presenter_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set("start", lua.create_function(presenter::start)?)?;
    // Why a capture produced nothing: `undecoded_video` never will here, while
    // `no_retained_pixels` is worth retrying.
    module.set("SKIP_UNDECODED_VIDEO", "undecoded_video")?;
    module.set("SKIP_NO_RETAINED_PIXELS", "no_retained_pixels")?;
    module.set("SKIP_NODE_HIDDEN", "node_hidden")?;
    Ok(module)
}

fn root(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set("VERSION", env!("CARGO_PKG_VERSION"))?;

    // Protocol constants, from the Rust table that owns them. Nothing here writes a number.
    for (name, value) in vivid_sdk::constant_table() {
        match value {
            ConstantValue::Text(text) => module.set(*name, *text)?,
            ConstantValue::Number(number) => module.set(*name, *number)?,
        }
    }

    module.set("constant_table", lua.create_function(constant_table)?)?;
    module.set(
        "probe_encoded_image",
        lua.create_function(probe_encoded_image)?,
    )?;
    module.set("error_info", lua.create_function(error::error_info)?)?;
    module.set("sleep", lua.create_function(sleep)?)?;
    module.set("monotonic", lua.create_function(monotonic)?)?;

    module.set("connect", lua.create_function(session::connect)?)?;
    module.set("display_image", lua.create_function(pane::display_image)?)?;
    module.set(
        "establish_desktop",
        lua.create_function(desktop::establish_desktop)?,
    )?;

    let pane = lua.create_table()?;
    pane.set("connect", lua.create_function(pane::pane_connect)?)?;
    pane.set(
        "from_session",
        lua.create_function(pane::pane_from_session)?,
    )?;
    module.set("PaneSession", pane)?;

    let rate = lua.create_table()?;
    rate.set("new", lua.create_function(pipeline::video_rate_control)?)?;
    module.set("VideoRateControl", rate)?;

    module.set("presenter", presenter_module(lua)?)?;
    module.set("overlay", overlay::module(lua)?)?;
    module.set("automation", automation::module(lua)?)?;

    // `require("vivid_sdk.presenter")` after the root answers from `package.loaded`, so both
    // spellings reach the same table.
    let loaded: LuaTable = lua.globals().get::<LuaTable>("package")?.get("loaded")?;
    for name in SUBMODULES {
        loaded.set(format!("{ROOT}.{name}"), module.get::<LuaValue>(*name)?)?;
    }
    Ok(module)
}

#[mlua::lua_module(name = "vivid_sdk")]
fn vivid_sdk(lua: &Lua) -> LuaResult<LuaTable> {
    root(lua)
}

/// A submodule required before the root: Lua's all-in-one loader finds `vivid_sdk.so` for
/// `require("vivid_sdk.presenter")` and calls `luaopen_vivid_sdk_presenter`, which loads the root
/// through `require` so there is still exactly one module table.
fn submodule(lua: &Lua, name: &str) -> LuaResult<LuaValue> {
    let require: LuaFunction = lua.globals().get("require")?;
    let root: LuaTable = require.call(ROOT)?;
    root.get(name)
}

#[mlua::lua_module(name = "vivid_sdk_presenter")]
fn vivid_sdk_presenter(lua: &Lua) -> LuaResult<LuaValue> {
    submodule(lua, "presenter")
}

#[mlua::lua_module(name = "vivid_sdk_overlay")]
fn vivid_sdk_overlay(lua: &Lua) -> LuaResult<LuaValue> {
    submodule(lua, "overlay")
}

#[mlua::lua_module(name = "vivid_sdk_automation")]
fn vivid_sdk_automation(lua: &Lua) -> LuaResult<LuaValue> {
    submodule(lua, "automation")
}
