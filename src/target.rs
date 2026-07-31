//! Target descriptors, validated per negotiated target profile.
//!
//! A session's target profile decides the shape of its descriptor: a terminal target carries a
//! grid and anchor capabilities, a desktop target carries a virtual rectangle and an output list.
//! Every path that accepts a descriptor — `WELCOME`, `TARGET_CHANGED`, and the dry-run
//! substitute — goes through [`validate_target_descriptor`], so no caller has to remember which
//! profile it negotiated.

use std::io;

use vivid_protocol::cbor::Value;
use vivid_protocol::messages::PayloadMap;
use vivid_protocol::target::DesktopTarget;

use crate::*;

/// The dry-run terminal target: an 80×24 grid on a 1920×1080 viewport.
pub(crate) fn offline_terminal_descriptor() -> PayloadMap {
    vec![
        (0, Value::Unsigned(1920)),
        (1, Value::Unsigned(1080)),
        (2, Value::Unsigned(80)),
        (3, Value::Unsigned(24)),
        (4, Value::Unsigned(24)),
        (5, Value::Unsigned(45)),
        (6, Value::Bool(true)),
        (7, Value::Unsigned(3)),
        (8, Value::Unsigned(256)),
    ]
}

/// The dry-run desktop target: one settled 1920×1080 output at the virtual origin.
///
/// A desktop producer's coordinate math is only exercised by a descriptor it can actually project
/// into, so the dry-run target is a real single-output topology rather than a fabricated grid.
pub(crate) fn offline_desktop_descriptor() -> PayloadMap {
    DesktopTarget {
        origin_x: 0,
        origin_y: 0,
        width: 1920,
        height: 1080,
        outputs: vec![vivid_protocol::target::OutputDescriptor {
            output_id: 1,
            origin_x: 0,
            origin_y: 0,
            width: 1920,
            height: 1080,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: vivid_protocol::geometry::Rotation::None,
            primary: true,
        }],
        settled: true,
        topology_revision: 1,
    }
    .encode()
}

/// The dry-run descriptor for a target profile.
pub(crate) fn offline_target_descriptor(target_profile: &str) -> PayloadMap {
    match target_profile {
        DESKTOP_SURFACE => offline_desktop_descriptor(),
        _ => offline_terminal_descriptor(),
    }
}

/// Validate a target descriptor against the session's negotiated target profile.
///
/// An unrecognized profile is rejected rather than waved through: a producer that negotiated a
/// profile this SDK cannot validate would be projecting coordinates it does not understand.
pub(crate) fn validate_target_descriptor(
    target_profile: &str,
    descriptor: &PayloadMap,
) -> io::Result<()> {
    match target_profile {
        TERMINAL_SURFACE => validate_terminal_target_descriptor(descriptor),
        DESKTOP_SURFACE => DesktopTarget::decode(descriptor)
            .map(|_| ())
            .map_err(|error| {
                invalid_data(format!("desktop target descriptor is invalid: {error}"))
            }),
        other => Err(invalid_data(format!(
            "target profile {other:?} has no descriptor validator in this SDK"
        ))),
    }
}

/// Terminal §2: viewport, grid, and cell metrics, all nonzero, plus anchor capabilities.
pub(crate) fn validate_terminal_target_descriptor(descriptor: &PayloadMap) -> io::Result<()> {
    validate_exact_payload_keys("terminal target descriptor", descriptor, 0..=8)?;
    for key in 0..=5 {
        if required_u64(descriptor, key)? == 0 {
            return Err(invalid_data(
                "terminal target descriptor contains a zero dimension",
            ));
        }
    }
    let _settled = required_bool(descriptor, 6)?;
    if required_u64(descriptor, 7)? != 3 || required_u64(descriptor, 8)? == 0 {
        return Err(invalid_data(
            "terminal target descriptor has unsupported anchor capabilities",
        ));
    }
    Ok(())
}

/// Whether a descriptor for this profile reports a settled target.
///
/// Both profiles carry the settle flag, at different keys; `TARGET_CHANGED` needs it to enforce
/// the reused-generation rule without knowing which profile it is looking at.
pub(crate) fn descriptor_settled(
    target_profile: &str,
    descriptor: &PayloadMap,
) -> io::Result<bool> {
    match target_profile {
        DESKTOP_SURFACE => required_bool(descriptor, 5),
        _ => required_bool(descriptor, 6),
    }
}

/// The key carrying the settle flag, so geometry comparison can exclude it.
pub(crate) fn settled_key(target_profile: &str) -> u64 {
    match target_profile {
        DESKTOP_SURFACE => 5,
        _ => 6,
    }
}

/// The highest descriptor key this profile's target uses.
///
/// `TARGET_CHANGED` appends the generation and reason above the descriptor, so this is where the
/// descriptor ends and the envelope's own fields begin.
pub(crate) fn last_descriptor_key(target_profile: &str) -> u64 {
    match target_profile {
        DESKTOP_SURFACE => 6,
        _ => 8,
    }
}
