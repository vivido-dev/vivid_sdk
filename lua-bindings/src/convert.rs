//! Values across the Lua boundary, checked on the way in.
//!
//! Integers must be integral, in range, and exactly representable by the Lua that produced them:
//! an invalid value is refused, never narrowed, rounded, or clamped into another ID. Configuration
//! tables are checked for unknown keys, so a misspelled field is an error instead of a silently
//! ignored default.

use std::time::Duration;

use mlua::prelude::*;
use vivid_protocol::cbor::Value;
use vivid_protocol::messages::PayloadMap;

use crate::error::invalid;

/// The largest integer this Lua carries exactly. Lua 5.3 and later have 64-bit integers; LuaJIT,
/// 5.1 and 5.2 carry every number as a double, which is exact only below 2^53.
#[cfg(any(feature = "luajit", feature = "lua51", feature = "lua52"))]
pub const EXACT: u64 = (1 << 53) - 1;
#[cfg(not(any(feature = "luajit", feature = "lua51", feature = "lua52")))]
pub const EXACT: u64 = i64::MAX as u64;

/// The largest magnitude a float names exactly, on every Lua. Beyond it a double may already be a
/// neighbour of what the caller wrote — `2^53 + 1` evaluates to `2^53` — so only a true integer
/// (Lua 5.3 and later) goes further.
const FLOAT_EXACT: f64 = ((1_u64 << 53) - 1) as f64;

/// An integral Lua number as an `i128`, which holds every value either representation carries;
/// `None` when it is not an integer at all.
fn integral(value: &LuaValue) -> Option<Result<i128, ()>> {
    match value {
        LuaValue::Integer(integer) => Some(Ok(i128::from(*integer))),
        LuaValue::Number(number) if number.is_finite() && number.fract() == 0.0 => {
            Some(if number.abs() <= FLOAT_EXACT {
                Ok(*number as i128)
            } else {
                Err(())
            })
        }
        _ => None,
    }
}

fn inexact(name: &str) -> LuaError {
    invalid(format!(
        "{name} exceeds the integers this Lua carries exactly"
    ))
}

fn unsigned(value: &LuaValue, name: &str) -> LuaResult<u64> {
    match integral(value) {
        Some(Ok(number)) if number < 0 => {
            Err(invalid(format!("{name} must be a non-negative integer")))
        }
        Some(Ok(number)) if number <= i128::from(EXACT) => Ok(number as u64),
        Some(_) => Err(inexact(name)),
        None => Err(invalid(format!("{name} must be a non-negative integer"))),
    }
}

fn signed(value: &LuaValue, name: &str) -> LuaResult<i64> {
    match integral(value) {
        Some(Ok(number)) if number.unsigned_abs() <= u128::from(EXACT) => Ok(number as i64),
        Some(_) => Err(inexact(name)),
        None => Err(invalid(format!("{name} must be an integer"))),
    }
}

/// One value of a configuration table or argument, converted with its field name for messages.
pub trait Field: Sized {
    fn from_value(value: LuaValue, name: &str) -> LuaResult<Self>;
}

macro_rules! unsigned_field {
    ($($ty:ty),*) => {$(
        impl Field for $ty {
            fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
                <$ty>::try_from(unsigned(&value, name)?)
                    .map_err(|_| invalid(format!("{name} is out of range")))
            }
        }
    )*};
}
unsigned_field!(u8, u16, u32, u64, usize);

macro_rules! signed_field {
    ($($ty:ty),*) => {$(
        impl Field for $ty {
            fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
                <$ty>::try_from(signed(&value, name)?)
                    .map_err(|_| invalid(format!("{name} is out of range")))
            }
        }
    )*};
}
signed_field!(i16, i32, i64);

impl Field for f64 {
    fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
        match value {
            LuaValue::Integer(integer) => Ok(integer as f64),
            LuaValue::Number(number) => Ok(number),
            _ => Err(invalid(format!("{name} must be a number"))),
        }
    }
}

impl Field for bool {
    fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
        // Deliberately not truthiness: `alternate = "no"` is a mistake, not `true`.
        match value {
            LuaValue::Boolean(flag) => Ok(flag),
            _ => Err(invalid(format!("{name} must be a boolean"))),
        }
    }
}

impl Field for String {
    fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
        match value {
            LuaValue::String(text) => text
                .to_str()
                .map(|text| text.to_owned())
                .map_err(|_| invalid(format!("{name} must be valid UTF-8"))),
            _ => Err(invalid(format!("{name} must be a string"))),
        }
    }
}

/// A byte string: pixels, encoded media, a digest. Lua strings are byte strings.
pub struct Bytes(pub Vec<u8>);

impl Field for Bytes {
    fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
        match value {
            LuaValue::String(bytes) => Ok(Self(bytes.as_bytes().to_vec())),
            _ => Err(invalid(format!("{name} must be a byte string"))),
        }
    }
}

impl Field for LuaTable {
    fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
        match value {
            LuaValue::Table(table) => Ok(table),
            _ => Err(invalid(format!("{name} must be a table"))),
        }
    }
}

impl<T: Field> Field for Vec<T> {
    fn from_value(value: LuaValue, name: &str) -> LuaResult<Self> {
        let table = LuaTable::from_value(value, name)?;
        sequence(&table, name)?
            .into_iter()
            .enumerate()
            .map(|(index, item)| T::from_value(item, &format!("{name}[{}]", index + 1)))
            .collect()
    }
}

/// A table's array part, refusing a table that also carries other keys or has holes.
pub fn sequence(table: &LuaTable, name: &str) -> LuaResult<Vec<LuaValue>> {
    let length = table.raw_len();
    let mut count = 0usize;
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, _) = pair?;
        match key {
            LuaValue::Integer(index) if index >= 1 && (index as usize) <= length => count += 1,
            _ => return Err(invalid(format!("{name} must be a list"))),
        }
    }
    if count != length {
        return Err(invalid(format!("{name} must be a list without holes")));
    }
    (1..=length).map(|index| table.raw_get(index)).collect()
}

/// An optional field: `nil` is absent, anything else must convert.
pub fn get<T: Field>(table: &LuaTable, name: &str) -> LuaResult<Option<T>> {
    match table.get::<LuaValue>(name)? {
        LuaValue::Nil => Ok(None),
        value => T::from_value(value, name).map(Some),
    }
}

/// A required field.
pub fn need<T: Field>(table: &LuaTable, name: &str) -> LuaResult<T> {
    get(table, name)?.ok_or_else(|| invalid(format!("missing configuration field {name}")))
}

/// Refuse keys a configuration table does not define, so a typo cannot fall back to a default.
pub fn check_keys(table: &LuaTable, allowed: &[&str], what: &str) -> LuaResult<()> {
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, _) = pair?;
        let known = match &key {
            LuaValue::String(name) => name.to_str().is_ok_and(|name| allowed.contains(&&*name)),
            _ => false,
        };
        if !known {
            let shown = match &key {
                LuaValue::String(name) => format!("{:?}", name.to_string_lossy()),
                other => other.type_name().to_owned(),
            };
            return Err(invalid(format!("unknown {what} field {shown}")));
        }
    }
    Ok(())
}

/// A positional argument, converted with the same rules as a configuration field.
pub fn arg<T: Field>(value: LuaValue, name: &str) -> LuaResult<T> {
    if value.is_nil() {
        return Err(invalid(format!("{name} is required")));
    }
    T::from_value(value, name)
}

/// An optional positional argument.
pub fn opt<T: Field>(value: LuaValue, name: &str) -> LuaResult<Option<T>> {
    match value {
        LuaValue::Nil => Ok(None),
        value => T::from_value(value, name).map(Some),
    }
}

/// An options table argument; `nil` is an empty table.
pub fn options(lua: &Lua, value: LuaValue, name: &str) -> LuaResult<LuaTable> {
    match value {
        LuaValue::Nil => lua.create_table(),
        value => LuaTable::from_value(value, name),
    }
}

/// A local timeout in seconds, which is how every Lua wait is spelled. Wire durations stay in
/// microseconds and keep their `_us` names.
pub fn timeout(value: LuaValue, default: Duration, name: &str) -> LuaResult<Duration> {
    let seconds = match value {
        LuaValue::Nil => return Ok(default),
        value => f64::from_value(value, name)?,
    };
    let duration = (seconds.is_finite() && seconds >= 0.0)
        .then(|| Duration::try_from_secs_f64(seconds).ok())
        .flatten()
        .ok_or_else(|| {
            invalid(format!(
                "{name} must be a finite, non-negative number of seconds"
            ))
        })?;
    if u64::try_from(duration.as_micros()).is_err()
        || std::time::Instant::now().checked_add(duration).is_none()
    {
        return Err(invalid(format!("{name} is out of range")));
    }
    Ok(duration)
}

/// Microseconds for a wait the SDK bounds in microseconds.
pub fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// One decoded CBOR value. Arrays become sequences and maps become integer-keyed tables; `null`
/// is absent, as `nil` is.
pub fn cbor_to_lua(lua: &Lua, value: &Value) -> LuaResult<LuaValue> {
    Ok(match value {
        Value::Unsigned(unsigned) => unsigned.into_lua(lua)?,
        Value::Negative(negative) => negative.into_lua(lua)?,
        Value::Bytes(bytes) => LuaValue::String(lua.create_string(bytes)?),
        Value::Text(text) => LuaValue::String(lua.create_string(text)?),
        Value::Bool(flag) => LuaValue::Boolean(*flag),
        Value::Null => LuaValue::Nil,
        Value::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (index, item) in items.iter().enumerate() {
                table.raw_set(index + 1, cbor_to_lua(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
        Value::Map(entries) => LuaValue::Table(payload_to_lua(lua, entries)?),
    })
}

/// A control payload as an integer-keyed table: `payload[0]`, `payload[1]`, and so on.
pub fn payload_to_lua(lua: &Lua, payload: &PayloadMap) -> LuaResult<LuaTable> {
    let table = lua.create_table_with_capacity(0, payload.len())?;
    for (key, value) in payload {
        table.raw_set(*key, cbor_to_lua(lua, value)?)?;
    }
    Ok(table)
}

/// A scene geometry map: non-negative integer keys to integers, strings, or booleans.
pub fn geometry(table: &LuaTable) -> LuaResult<PayloadMap> {
    let mut geometry = Vec::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        let key = u64::from_value(key, "geometry key")?;
        let value = match value {
            LuaValue::String(_) => Value::Text(String::from_value(value, "geometry value")?),
            LuaValue::Boolean(flag) => Value::Bool(flag),
            value => match i64::from_value(value, "geometry value")? {
                negative if negative < 0 => Value::Negative(negative),
                unsigned => Value::Unsigned(unsigned as u64),
            },
        };
        geometry.push((key, value));
    }
    geometry.sort_by_key(|(key, _)| *key);
    Ok(geometry)
}

fn json_options() -> mlua::serde::SerializeOptions {
    // JSON `null` becomes `nil`, so it stays falsy the way Python's `None` and JavaScript's
    // `null` are; arrays keep the array metatable so they round-trip as arrays.
    mlua::serde::SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false)
        .set_array_metatable(true)
}

pub fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> LuaResult<LuaValue> {
    lua.to_value_with(value, json_options())
}

// Requests are JSON only for the automation client, which is Unix only.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn lua_to_json(lua: &Lua, value: LuaValue) -> LuaResult<serde_json::Value> {
    lua.from_value_with(
        value,
        mlua::serde::DeserializeOptions::new()
            .deny_unsupported_types(true)
            .deny_recursive_tables(true)
            .sort_keys(true),
    )
    .map_err(|error| invalid(format!("request is not representable as JSON: {error}")))
}

/// A byte string as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
