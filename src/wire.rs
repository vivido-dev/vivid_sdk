//! Shared payload decoding, validation, and small error helpers.
//!
//! Every strict-schema check a module needs lives here so the same key, ownership, and type rules
//! apply wherever a payload is read.

use std::io;
use std::sync::Mutex;

use vivid_protocol::cbor::Value;
use vivid_protocol::messages;
use vivid_protocol::messages::PayloadMap;
use vivid_protocol::resource::ResourceContract;
use vivid_protocol::revision::{SceneRevision, TargetGeneration};
use vivid_protocol::track::TrackConfiguration;
use vivid_protocol::wire::Record;

use crate::*;

pub(crate) fn session_event(record_type: u16, object_id: u64, payload: PayloadMap) -> SessionEvent {
    match record_type {
        messages::TARGET_CHANGED => SessionEvent::TargetChanged(payload),
        messages::ANCHOR_READY => SessionEvent::AnchorReady {
            context_id: optional_u64(&payload, 0)
                .unwrap_or(None)
                .unwrap_or_default(),
            anchor_id: optional_u64(&payload, 1)
                .unwrap_or(None)
                .unwrap_or(object_id),
            payload,
        },
        messages::ANCHOR_GONE => SessionEvent::AnchorGone {
            context_id: optional_u64(&payload, 0)
                .unwrap_or(None)
                .unwrap_or_default(),
            anchor_id: optional_u64(&payload, 1)
                .unwrap_or(None)
                .unwrap_or(object_id),
            payload,
        },
        messages::TRACK_LOST => SessionEvent::TrackLost { object_id, payload },
        messages::CONTEXT_CHANGED => SessionEvent::ContextChanged { object_id, payload },
        _ => SessionEvent::Other {
            record_type,
            object_id,
            payload,
        },
    }
}

pub(crate) fn session_info(welcome: &messages::Welcome) -> SessionInfo {
    SessionInfo {
        session_id: welcome.session_id,
        session_tag: welcome.session_tag,
        root_context_id: welcome.root_context_id,
        target_generation: TargetGeneration::new(welcome.target_generation),
        target_profile: welcome.target_profile.clone(),
        target_descriptor: welcome.target_descriptor.clone(),
        accepted_profiles: welcome.accepted_profiles.clone(),
        session_revision: welcome.session_revision,
        scene_revision: SceneRevision::new(welcome.scene_revision),
        establishment_state: welcome.establishment_state,
        resume_generation: welcome.resume_generation,
        resource_contract: welcome.resource_contract.clone(),
    }
}

pub(crate) fn decoded_payload(record: &Record) -> io::Result<PayloadMap> {
    Ok(messages::decode_control(&record.body)?.payload)
}

pub(crate) fn presenter_error(body: &[u8]) -> io::Result<io::Error> {
    Ok(io::Error::other(PresenterError::from(
        messages::parse_error_reply(body)?,
    )))
}

pub(crate) fn expect_record(record: &Record, expected: u16, object_id: u64) -> io::Result<()> {
    if record.record_type != expected || record.object_id != object_id {
        return Err(invalid_data(format!(
            "expected record {expected:#06x} for object {object_id}, received {:#06x} for {}",
            record.record_type, record.object_id
        )));
    }
    Ok(())
}

pub(crate) fn validate_owner_pair(
    payload: &PayloadMap,
    context_id: u64,
    object_id: u64,
) -> io::Result<()> {
    if required_u64(payload, 0)? != context_id || required_u64(payload, 1)? != object_id {
        return Err(invalid_data(
            "reply contains the wrong owner-qualified identity",
        ));
    }
    Ok(())
}

pub(crate) fn validate_track_owner(
    payload: &PayloadMap,
    configuration: &TrackConfiguration,
) -> io::Result<()> {
    validate_track_tuple(payload, configuration)
}

pub(crate) fn validate_track_tuple(
    payload: &PayloadMap,
    configuration: &TrackConfiguration,
) -> io::Result<()> {
    if required_u64(payload, 0)? != configuration.context_id
        || required_u64(payload, 1)? != configuration.surface_id
        || required_u64(payload, 2)? != configuration.track_id
    {
        return Err(invalid_data(
            "reply contains the wrong complete track identity",
        ));
    }
    Ok(())
}

pub(crate) fn required_value(payload: &PayloadMap, key: u64) -> io::Result<&Value> {
    payload
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(value))
        .ok_or_else(|| invalid_data(format!("reply omits payload key {key}")))
}

pub(crate) fn validate_exact_payload_keys(
    schema: &str,
    payload: &PayloadMap,
    expected: std::ops::RangeInclusive<u64>,
) -> io::Result<()> {
    let expected = expected.collect::<Vec<_>>();
    if payload.len() != expected.len()
        || payload
            .iter()
            .zip(expected)
            .any(|((actual, _), expected)| *actual != expected)
    {
        return Err(invalid_data(format!(
            "{schema} payload keys are not the exact canonical schema"
        )));
    }
    Ok(())
}

pub(crate) fn validate_payload_keys(
    schema: &str,
    payload: &PayloadMap,
    required: std::ops::RangeInclusive<u64>,
    optional: &[u64],
) -> io::Result<()> {
    let required = required.collect::<Vec<_>>();
    if payload.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        || required
            .iter()
            .any(|key| !payload.iter().any(|(actual, _)| actual == key))
        || payload
            .iter()
            .any(|(key, _)| !required.contains(key) && !optional.contains(key))
    {
        return Err(invalid_data(format!(
            "{schema} payload keys do not match its canonical schema"
        )));
    }
    Ok(())
}

pub(crate) fn required_u64(payload: &PayloadMap, key: u64) -> io::Result<u64> {
    required_value(payload, key)?
        .as_u64()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not an unsigned integer")))
}

pub(crate) fn required_i64(payload: &PayloadMap, key: u64) -> io::Result<i64> {
    required_value(payload, key)?
        .as_i64()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not a signed integer")))
}

pub(crate) fn optional_u64(payload: &PayloadMap, key: u64) -> io::Result<Option<u64>> {
    payload
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(value))
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| invalid_data(format!("payload key {key} is not unsigned")))
        })
        .transpose()
}

pub(crate) fn optional_map(payload: &PayloadMap, key: u64) -> io::Result<Option<&PayloadMap>> {
    payload
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(value))
        .map(|value| match value {
            Value::Map(value) => Ok(value),
            _ => Err(invalid_data(format!("payload key {key} is not a map"))),
        })
        .transpose()
}

pub(crate) fn required_u32(payload: &PayloadMap, key: u64) -> io::Result<u32> {
    u32::try_from(required_u64(payload, key)?)
        .map_err(|_| invalid_data(format!("payload key {key} exceeds u32")))
}

pub(crate) fn required_bool(payload: &PayloadMap, key: u64) -> io::Result<bool> {
    required_value(payload, key)?
        .as_bool()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not a boolean")))
}

pub(crate) fn required_text(payload: &PayloadMap, key: u64) -> io::Result<&str> {
    required_value(payload, key)?
        .as_text()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not text")))
}

pub(crate) fn required_map(payload: &PayloadMap, key: u64) -> io::Result<&PayloadMap> {
    match required_value(payload, key)? {
        Value::Map(value) => Ok(value),
        _ => Err(invalid_data(format!("payload key {key} is not a map"))),
    }
}

pub(crate) fn required_text_array(payload: &PayloadMap, key: u64) -> io::Result<Vec<String>> {
    required_value(payload, key)?
        .as_array()
        .ok_or_else(|| invalid_data(format!("payload key {key} is not an array")))?
        .iter()
        .map(|value| {
            value
                .as_text()
                .map(ToOwned::to_owned)
                .ok_or_else(|| invalid_data(format!("payload key {key} contains non-text")))
        })
        .collect()
}

pub(crate) fn required_contract(payload: &PayloadMap, key: u64) -> io::Result<ResourceContract> {
    ResourceContract::from_value(required_value(payload, key)?).map_err(io::Error::other)
}

pub(crate) fn ensure_live_surface(state: &SurfaceLocal) -> io::Result<()> {
    if state.destroyed {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "surface is destroyed",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn ensure_live_track(state: &TrackLocal) -> io::Result<()> {
    if state.destroyed {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "track is destroyed",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_profiles(profiles: &[String]) -> io::Result<()> {
    if profiles.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_input("profile lists must be sorted and unique"));
    }
    Ok(())
}

pub(crate) fn random_bytes(bytes: &mut [u8]) -> io::Result<()> {
    getrandom::fill(bytes)
        .map_err(|error| io::Error::other(format!("secure randomness failed: {error}")))
}

pub(crate) fn lock<'a, T>(
    mutex: &'a Mutex<T>,
    name: &str,
) -> io::Result<std::sync::MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other(format!("{name} lock is poisoned")))
}

pub(crate) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(crate) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn signed(value: i64) -> Value {
    if value >= 0 {
        Value::Unsigned(value as u64)
    } else {
        Value::Negative(value)
    }
}
