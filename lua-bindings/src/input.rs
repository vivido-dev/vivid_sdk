//! Desktop input lanes: ordinary input over a separately authenticated interactive connection.
//!
//! Nothing here injects input. A lane reports what arrived, still generation-qualified; injection
//! belongs to the host that owns the OS seat, behind its own gate.

use std::time::Duration;

use mlua::prelude::*;
use vivid_sdk::{InputBinding, InputBindingStatus, InputLane, InputLaneEvent, InputTuple};

use crate::convert::{arg, check_keys, get, need, opt, payload_to_lua, timeout};
use crate::error::{IoResultExt, closed};
use crate::session::LuaSession;

/// How long a lane wait blocks by default. Pointer motion outruns any sensible poll interval, so
/// a lane is driven from a wait rather than from `take_event`.
const DEFAULT_LANE_WAIT: Duration = Duration::from_secs(1);

pub fn add_session_methods<M: LuaUserDataMethods<LuaSession>>(methods: &mut M) {
    methods.add_method("open_input_lane", |_, this, generation: LuaValue| {
        let generation = opt::<u64>(generation, "lane_generation")?.unwrap_or(1);
        Ok(LuaInputLane {
            inner: Some(this.get()?.open_input_lane(generation).lua()?),
        })
    });
}

pub struct LuaInputLane {
    inner: Option<InputLane>,
}

impl LuaInputLane {
    fn get(&self) -> LuaResult<&InputLane> {
        self.inner.as_ref().ok_or_else(|| closed("input lane"))
    }
}

impl LuaUserData for LuaInputLane {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("generation", |_, this| {
            Ok(this.inner.as_ref().map(InputLane::generation).unwrap_or(0))
        });
        fields.add_field_method_get("closed", |_, this| Ok(this.inner.is_none()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(match &this.inner {
                Some(lane) => format!("vivid_sdk.InputLane(generation={})", lane.generation()),
                None => "vivid_sdk.InputLane(closed)".into(),
            })
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| match this
            .inner
            .take()
        {
            Some(lane) => lane.close().lua(),
            None => Ok(()),
        });
        methods.add_method_mut("close", |_, this, ()| {
            this.inner
                .take()
                .ok_or_else(|| closed("input lane"))?
                .close()
                .lua()
        });
        methods.add_method("set_binding", |lua, this, binding: LuaValue| {
            let binding = input_binding(&arg::<LuaTable>(binding, "input binding")?)?;
            let status = this.get()?.set_binding(&binding).lua()?;
            binding_status(lua, status)
        });
        methods.add_method("take_event", |lua, this, ()| {
            match this.get()?.take_event().lua()? {
                Some(event) => lane_event(lua, event).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("wait_event", |lua, this, value: LuaValue| {
            let wait = timeout(value, DEFAULT_LANE_WAIT, "timeout")?;
            match this.get()?.wait_event(wait).lua()? {
                Some(event) => lane_event(lua, event).map(LuaValue::Table),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_function("events", |lua, (ud, value): (LuaAnyUserData, LuaValue)| {
            let wait = timeout(value, DEFAULT_LANE_WAIT, "timeout")?;
            ud.borrow::<LuaInputLane>()?;
            lua.create_function(move |lua, _: LuaMultiValue| {
                let lane = ud.borrow::<LuaInputLane>()?;
                let Some(lane) = lane.inner.as_ref() else {
                    return Ok(LuaValue::Nil);
                };
                match lane.wait_event(wait).lua()? {
                    // `lane_closed` is the last event a lane produces.
                    Some(InputLaneEvent::LaneClosed { .. }) | None => Ok(LuaValue::Nil),
                    Some(event) => lane_event(lua, event).map(LuaValue::Table),
                }
            })
        });
    }
}

fn input_binding(config: &LuaTable) -> LuaResult<InputBinding> {
    check_keys(
        config,
        &[
            "producer_epoch",
            "context_id",
            "surface_id",
            "surface_generation",
            "requested_classes",
            "reason",
            "requested_watchdog_us",
        ],
        "input binding",
    )?;
    Ok(InputBinding {
        producer_epoch: vivid_protocol::revision::InputEpoch::new(need(config, "producer_epoch")?),
        context_id: need(config, "context_id")?,
        surface_id: need(config, "surface_id")?,
        surface_generation: vivid_protocol::revision::SurfaceGeneration::new(need(
            config,
            "surface_generation",
        )?),
        requested_classes: need(config, "requested_classes")?,
        reason: get(config, "reason")?.unwrap_or(1),
        requested_watchdog_us: get(config, "requested_watchdog_us")?
            .unwrap_or(DEFAULT_LANE_WAIT.as_micros() as u64),
    })
}

fn binding_status(lua: &Lua, status: InputBindingStatus) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("producer_epoch", status.producer_epoch)?;
    table.set("grant_generation", status.grant_generation)?;
    table.set("context_id", status.context_id)?;
    table.set("surface_id", status.surface_id)?;
    table.set("surface_generation", status.surface_generation)?;
    table.set("effective_classes", status.effective_classes)?;
    table.set("state", status.state)?;
    table.set("reason", status.reason)?;
    table.set("watchdog_timeout_us", status.watchdog_timeout_us)?;
    Ok(table)
}

/// The complete grant identity, flattened into the event: an injection gate compares all of it.
fn set_tuple(table: &LuaTable, binding: &InputTuple) -> LuaResult<()> {
    table.set("producer_epoch", binding.producer_epoch.get())?;
    table.set("grant_generation", binding.grant_generation.get())?;
    table.set("context_id", binding.context_id)?;
    table.set("surface_id", binding.surface_id)?;
    table.set("surface_generation", binding.surface_generation.get())?;
    Ok(())
}

fn lane_event(lua: &Lua, event: InputLaneEvent) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    match event {
        InputLaneEvent::Input {
            record_type,
            surface_id,
            payload,
        } => {
            // The presenter's exact payload, undecoded: it still has to pass the host's gate.
            table.set("kind", "input")?;
            table.set("record_type", record_type)?;
            table.set("surface_id", surface_id)?;
            table.set("payload", payload_to_lua(lua, &payload)?)?;
        }
        InputLaneEvent::Renew(renewal) => {
            table.set("kind", "renew")?;
            set_tuple(&table, &renewal.binding)?;
            table.set("renewal_sequence", renewal.renewal_sequence)?;
            table.set("watchdog_timeout_us", renewal.watchdog_timeout_us)?;
        }
        InputLaneEvent::Revoked(termination) => {
            table.set("kind", "revoked")?;
            set_tuple(&table, &termination.binding)?;
            table.set("reason", termination.reason)?;
        }
        InputLaneEvent::Reset(termination) => {
            table.set("kind", "reset")?;
            set_tuple(&table, &termination.binding)?;
            table.set("reason", termination.reason)?;
        }
        InputLaneEvent::LaneClosed { diagnostic } => {
            table.set("kind", "lane_closed")?;
            table.set("diagnostic", diagnostic)?;
        }
        InputLaneEvent::Error(error) => {
            table.set("kind", "error")?;
            table.set("code", error.code)?;
            table.set("message", error.to_string())?;
        }
    }
    Ok(table)
}
