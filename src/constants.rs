//! The single naming table for the protocol constants every language binding exposes.
//!
//! Bindings do not hardcode values. Python and TypeScript build their constant namespaces by
//! reading [`constant_table`] at import time. Explicit declarations expose the names in each
//! language; values come from this table. Before this table existed the Python
//! package carried a hand-copied list that a TypeScript package would have copied a third time.
//!
//! Names are the binding-facing spelling, which is not always the Rust spelling: the crate calls a
//! negotiated profile [`CORE_CONTROL`](vivid_protocol::registry::CORE_CONTROL) while every binding
//! calls it `PROFILE_CORE`. The value is what matters and it is taken from `vivid_protocol`.

use vivid_protocol::context::{
    OP_DELEGATE, OP_DESKTOP_INPUT, OP_KNOWN_MASK, OP_OBSERVE, OP_RECEIVE_FILE_DROP, OP_SCENE,
    OP_SURFACE_TRACK_MEDIA, OP_TERMINAL_ANCHOR,
};
use vivid_protocol::file_drop::{FileDropDestination, FileDropState};
use vivid_protocol::input::{
    INPUT_CLASS_KEYBOARD, INPUT_CLASS_KNOWN_MASK, INPUT_CLASS_POINTER_AXIS,
    INPUT_CLASS_POINTER_BUTTON, INPUT_CLASS_POINTER_MOTION, MAX_WATCHDOG_US, MIN_WATCHDOG_US,
};
use vivid_protocol::lease::CleanupPolicy;
use vivid_protocol::registry::{
    AUDIO_GAIN, AUDIO_INPUT, CANVAS_CONTENT, CANVAS_SURFACE, CORE_CONTROL, DESKTOP_CONTENT,
    DESKTOP_INPUT, DESKTOP_SURFACE, FILE_DROP, FILE_DROP_PATH, GENERIC_CONTENT, LIVE_MEDIA,
    OBSERVABILITY, OVERLAY_INPUT, TERMINAL_CONTENT, TERMINAL_OVERLAY, TERMINAL_SURFACE,
    TIMED_MEDIA, VECTOR_SCENE, WEB_CARRIER,
};
use vivid_protocol::scene::Fit;
use vivid_protocol::surface::{
    CoordinateModel, POLICY_DENY_CAPTURE, POLICY_DENY_DESCRIPTOR_EXPORT, POLICY_DENY_IMAGE_CACHE,
    POLICY_DENY_POSTER_RETENTION, POLICY_KNOWN_MASK, POLICY_REDUCED_DIAGNOSTICS, SurfaceRole,
};
use vivid_protocol::track::{
    MILESTONE_BUFFERED_ENDED, MILESTONE_CHANNEL_ACCEPTED, MILESTONE_CHANNEL_DETACHED,
    MILESTONE_CLOCK_STARTED, MILESTONE_DECODER_INITIALIZED, MILESTONE_EOS_ACCEPTED,
    MILESTONE_FIRST_MEDIA, MILESTONE_KNOWN_MASK, MILESTONE_OUTPUT_READY, MILESTONE_PRESENTED,
    MILESTONE_RANDOM_ACCESS, MILESTONE_TRACK_LOST, TrackDirection, TrackMode,
};
use vivid_protocol::{MAX_TRACK_WAIT_TIMEOUT_US, messages::LaneClass};

use crate::controller::{DEFAULT_ACTIVATION_TIMEOUT_US, MAX_ACTIVATION_TIMEOUT_US};
use crate::pipeline::MINIMUM_TARGET_BITS_PER_SECOND;
use crate::scene::{COORDINATE_SPACE_GRID_CELL, TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH};
use crate::track::TrackWaitCondition;

/// Surface slot assignments from §1 of the 1.5 media specification.
///
/// Slot zero carries no playback and is the only slot an uplink audio track may declare. Slots 6
/// through 31 are reserved; 32 and above are application-defined.
pub const SLOT_NONE: u64 = 0;
/// The `primary-video` slot. Video tracks only.
pub const SLOT_PRIMARY_VIDEO: u64 = 1;
/// The `audio` slot, which is the surface playback group's master clock when present and healthy.
pub const SLOT_AUDIO: u64 = 2;
/// The `raster` slot. Raster tracks only.
pub const SLOT_RASTER: u64 = 3;
/// The `poster` slot. Encoded image or raster.
pub const SLOT_POSTER: u64 = 4;
/// Portable vector-scene visual slot.
pub const SLOT_VECTOR: u64 = 5;

/// Track kind assignments from §1 of the 1.5 media specification.
pub const TRACK_KIND_VIDEO: u64 = 1;
/// Audio track kind.
pub const TRACK_KIND_AUDIO: u64 = 2;
/// Raster track kind.
pub const TRACK_KIND_RASTER: u64 = 3;
/// Encoded-image track kind.
pub const TRACK_KIND_IMAGE: u64 = 4;
/// Portable vector-scene track kind.
pub const TRACK_KIND_VECTOR: u64 = 5;

/// Encoded-image encodings accepted by
/// [`ImageConfiguration`](vivid_protocol::track::ImageConfiguration).
pub const IMAGE_ENCODING_PNG: u64 = 1;
/// JPEG encoded-image encoding.
pub const IMAGE_ENCODING_JPEG: u64 = 2;

/// A constant's value: either a negotiated profile name or a numeric assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantValue {
    /// A profile or semantic-profile name, compared as an exact string on the wire.
    Text(&'static str),
    /// A numeric assignment, bit, or bound.
    Number(u64),
}

impl ConstantValue {
    /// The profile name, or `None` when this constant is numeric.
    pub fn as_text(&self) -> Option<&'static str> {
        match self {
            Self::Text(value) => Some(value),
            Self::Number(_) => None,
        }
    }

    /// The numeric value, or `None` when this constant is a profile name.
    pub fn as_number(&self) -> Option<u64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Text(_) => None,
        }
    }
}

use ConstantValue::{Number, Text};

/// Every constant a binding exposes, in a stable order suitable for generated output.
///
/// The order is grouped, not sorted: generated Python stubs and TypeScript declaration files read
/// better when profiles, policies, and milestones stay together.
pub fn constant_table() -> &'static [(&'static str, ConstantValue)] {
    TABLE
}

static TABLE: &[(&str, ConstantValue)] = &[
    // Negotiated profiles. `multiplexed-session-carrier-v1` is deliberately absent: the
    // specification marks it experimental and forbids shipping it.
    ("PROFILE_CORE", Text(CORE_CONTROL)),
    ("PROFILE_TERMINAL_SURFACE", Text(TERMINAL_SURFACE)),
    ("PROFILE_DESKTOP_SURFACE", Text(DESKTOP_SURFACE)),
    ("PROFILE_CANVAS_SURFACE", Text(CANVAS_SURFACE)),
    ("PROFILE_LIVE_MEDIA", Text(LIVE_MEDIA)),
    ("PROFILE_TIMED_MEDIA", Text(TIMED_MEDIA)),
    ("PROFILE_AUDIO_GAIN", Text(AUDIO_GAIN)),
    ("PROFILE_AUDIO_INPUT", Text(AUDIO_INPUT)),
    ("PROFILE_DESKTOP_INPUT", Text(DESKTOP_INPUT)),
    ("PROFILE_FILE_DROP", Text(FILE_DROP)),
    ("PROFILE_FILE_DROP_PATH", Text(FILE_DROP_PATH)),
    ("PROFILE_OBSERVABILITY", Text(OBSERVABILITY)),
    ("PROFILE_WEB_CARRIER", Text(WEB_CARRIER)),
    ("PROFILE_TERMINAL_OVERLAY", Text(TERMINAL_OVERLAY)),
    ("PROFILE_VECTOR_SCENE", Text(VECTOR_SCENE)),
    ("PROFILE_OVERLAY_INPUT", Text(OVERLAY_INPUT)),
    // Surface semantic profiles.
    ("SURFACE_GENERIC", Text(GENERIC_CONTENT)),
    ("SURFACE_TERMINAL", Text(TERMINAL_CONTENT)),
    ("SURFACE_DESKTOP", Text(DESKTOP_CONTENT)),
    ("SURFACE_CANVAS", Text(CANVAS_CONTENT)),
    // Coordinate models.
    (
        "COORDINATE_DESKTOP_LOGICAL_PIXELS",
        Number(CoordinateModel::DesktopLogicalPixels as u64),
    ),
    (
        "COORDINATE_NORMALIZED",
        Number(CoordinateModel::Normalized as u64),
    ),
    (
        "COORDINATE_CANVAS_LOGICAL_UNITS",
        Number(CoordinateModel::CanvasLogicalUnits as u64),
    ),
    (
        "COORDINATE_TERMINAL_CONTENT_CELLS",
        Number(CoordinateModel::TerminalContentCells as u64),
    ),
    // Descriptor roles.
    ("ROLE_UNSPECIFIED", Number(SurfaceRole::Unspecified as u64)),
    ("ROLE_DOCUMENT", Number(SurfaceRole::Document as u64)),
    ("ROLE_DESKTOP", Number(SurfaceRole::Desktop as u64)),
    ("ROLE_TIMED_MEDIA", Number(SurfaceRole::TimedMedia as u64)),
    ("ROLE_FIGURE", Number(SurfaceRole::Figure as u64)),
    ("ROLE_TERMINAL", Number(SurfaceRole::TerminalText as u64)),
    ("ROLE_CANVAS", Number(SurfaceRole::ApplicationCanvas as u64)),
    // Capture and export policies.
    ("POLICY_DENY_CAPTURE", Number(POLICY_DENY_CAPTURE)),
    (
        "POLICY_DENY_DESCRIPTOR_EXPORT",
        Number(POLICY_DENY_DESCRIPTOR_EXPORT),
    ),
    (
        "POLICY_DENY_POSTER_RETENTION",
        Number(POLICY_DENY_POSTER_RETENTION),
    ),
    ("POLICY_DENY_IMAGE_CACHE", Number(POLICY_DENY_IMAGE_CACHE)),
    (
        "POLICY_REDUCED_DIAGNOSTICS",
        Number(POLICY_REDUCED_DIAGNOSTICS),
    ),
    ("POLICY_KNOWN_MASK", Number(POLICY_KNOWN_MASK)),
    // Track modes, directions, kinds, lanes, and slots.
    ("TRACK_MODE_LIVE", Number(TrackMode::Live as u64)),
    ("TRACK_MODE_TIMED", Number(TrackMode::Timed as u64)),
    (
        "TRACK_DIRECTION_DOWNLINK",
        Number(TrackDirection::Downlink as u64),
    ),
    (
        "TRACK_DIRECTION_UPLINK",
        Number(TrackDirection::Uplink as u64),
    ),
    ("TRACK_KIND_VIDEO", Number(TRACK_KIND_VIDEO)),
    ("TRACK_KIND_AUDIO", Number(TRACK_KIND_AUDIO)),
    ("TRACK_KIND_RASTER", Number(TRACK_KIND_RASTER)),
    ("TRACK_KIND_IMAGE", Number(TRACK_KIND_IMAGE)),
    ("TRACK_KIND_VECTOR", Number(TRACK_KIND_VECTOR)),
    ("LANE_CONTROL", Number(LaneClass::Control as u64)),
    ("LANE_INTERACTIVE", Number(LaneClass::Interactive as u64)),
    ("LANE_REALTIME", Number(LaneClass::Realtime as u64)),
    ("LANE_BULK", Number(LaneClass::Bulk as u64)),
    ("SLOT_NONE", Number(SLOT_NONE)),
    ("SLOT_PRIMARY_VIDEO", Number(SLOT_PRIMARY_VIDEO)),
    ("SLOT_AUDIO", Number(SLOT_AUDIO)),
    ("SLOT_RASTER", Number(SLOT_RASTER)),
    ("SLOT_POSTER", Number(SLOT_POSTER)),
    ("SLOT_VECTOR", Number(SLOT_VECTOR)),
    // Scene node fit modes.
    ("FIT_FILL", Number(Fit::Fill as u64)),
    ("FIT_CONTAIN", Number(Fit::Contain as u64)),
    ("FIT_COVER", Number(Fit::Cover as u64)),
    ("FIT_NONE", Number(Fit::None as u64)),
    // Encoded-image encodings.
    ("IMAGE_PNG", Number(IMAGE_ENCODING_PNG)),
    ("IMAGE_JPEG", Number(IMAGE_ENCODING_JPEG)),
    // Readiness milestones.
    (
        "MILESTONE_CHANNEL_ACCEPTED",
        Number(MILESTONE_CHANNEL_ACCEPTED),
    ),
    ("MILESTONE_FIRST_MEDIA", Number(MILESTONE_FIRST_MEDIA)),
    (
        "MILESTONE_DECODER_INITIALIZED",
        Number(MILESTONE_DECODER_INITIALIZED),
    ),
    ("MILESTONE_RANDOM_ACCESS", Number(MILESTONE_RANDOM_ACCESS)),
    ("MILESTONE_OUTPUT_READY", Number(MILESTONE_OUTPUT_READY)),
    ("MILESTONE_PRESENTED", Number(MILESTONE_PRESENTED)),
    ("MILESTONE_CLOCK_STARTED", Number(MILESTONE_CLOCK_STARTED)),
    ("MILESTONE_EOS_ACCEPTED", Number(MILESTONE_EOS_ACCEPTED)),
    ("MILESTONE_BUFFERED_ENDED", Number(MILESTONE_BUFFERED_ENDED)),
    (
        "MILESTONE_CHANNEL_DETACHED",
        Number(MILESTONE_CHANNEL_DETACHED),
    ),
    ("MILESTONE_TRACK_LOST", Number(MILESTONE_TRACK_LOST)),
    ("MILESTONE_KNOWN_MASK", Number(MILESTONE_KNOWN_MASK)),
    // Track wait conditions.
    (
        "WAIT_REVISION_GREATER",
        Number(TrackWaitCondition::RevisionGreater as u64),
    ),
    (
        "WAIT_MILESTONE_SET",
        Number(TrackWaitCondition::MilestoneSet as u64),
    ),
    (
        "WAIT_RASTER_FRAME_PRESENTED",
        Number(TrackWaitCondition::RasterFramePresented as u64),
    ),
    (
        "WAIT_VIDEO_PTS_PRESENTED",
        Number(TrackWaitCondition::VideoPtsPresented as u64),
    ),
    (
        "WAIT_PLAYBACK_STARTED",
        Number(TrackWaitCondition::PlaybackStarted as u64),
    ),
    (
        "WAIT_PLAYBACK_ENDED",
        Number(TrackWaitCondition::PlaybackEnded as u64),
    ),
    (
        "WAIT_CHANNEL_ACCEPTED",
        Number(TrackWaitCondition::ChannelAccepted as u64),
    ),
    (
        "WAIT_CHANNEL_CLOSED",
        Number(TrackWaitCondition::ChannelClosed as u64),
    ),
    (
        "WAIT_TRACK_LOST",
        Number(TrackWaitCondition::TrackLost as u64),
    ),
    (
        "MAX_TRACK_WAIT_TIMEOUT_US",
        Number(MAX_TRACK_WAIT_TIMEOUT_US),
    ),
    // Context operation classes.
    ("OP_OBSERVE", Number(OP_OBSERVE)),
    ("OP_SURFACE_TRACK_MEDIA", Number(OP_SURFACE_TRACK_MEDIA)),
    ("OP_SCENE", Number(OP_SCENE)),
    ("OP_TERMINAL_ANCHOR", Number(OP_TERMINAL_ANCHOR)),
    ("OP_DESKTOP_INPUT", Number(OP_DESKTOP_INPUT)),
    ("OP_DELEGATE", Number(OP_DELEGATE)),
    ("OP_RECEIVE_FILE_DROP", Number(OP_RECEIVE_FILE_DROP)),
    ("OP_KNOWN_MASK", Number(OP_KNOWN_MASK)),
    // Input classes and grant watchdog bounds.
    ("INPUT_CLASS_KEYBOARD", Number(INPUT_CLASS_KEYBOARD)),
    (
        "INPUT_CLASS_POINTER_MOTION",
        Number(INPUT_CLASS_POINTER_MOTION),
    ),
    (
        "INPUT_CLASS_POINTER_BUTTON",
        Number(INPUT_CLASS_POINTER_BUTTON),
    ),
    ("INPUT_CLASS_POINTER_AXIS", Number(INPUT_CLASS_POINTER_AXIS)),
    ("INPUT_CLASS_KNOWN_MASK", Number(INPUT_CLASS_KNOWN_MASK)),
    ("MIN_WATCHDOG_US", Number(MIN_WATCHDOG_US)),
    ("MAX_WATCHDOG_US", Number(MAX_WATCHDOG_US)),
    // Delegation, file drop, and microphone packet shape.
    ("CLEANUP_IMMEDIATE", Number(CleanupPolicy::Immediate as u64)),
    (
        "CLEANUP_SUSPEND_ON_UNCLEAN_LOSS",
        Number(CleanupPolicy::SuspendOnUncleanLoss as u64),
    ),
    (
        "DESTINATION_SHELL_CWD",
        Number(FileDropDestination::ShellCwd as u64),
    ),
    (
        "DESTINATION_DESKTOP_FOLDER",
        Number(FileDropDestination::DesktopFolder as u64),
    ),
    ("DROP_OFFERED", Number(FileDropState::Offered as u64)),
    ("DROP_ACCEPTED", Number(FileDropState::Accepted as u64)),
    (
        "DROP_TRANSFERRING",
        Number(FileDropState::Transferring as u64),
    ),
    ("DROP_COMMITTED", Number(FileDropState::Committed as u64)),
    ("DROP_CANCELLED", Number(FileDropState::Cancelled as u64)),
    ("DROP_FAILED", Number(FileDropState::Failed as u64)),
    (
        "MIC_PACKET_US",
        Number(vivid_protocol::audio_input::PACKET_US),
    ),
    (
        "MIC_PACKET_BYTES",
        Number(vivid_protocol::audio_input::PCM_BYTES as u64),
    ),
    // Terminal placement and rate control.
    (
        "COORDINATE_SPACE_GRID_CELL",
        Number(COORDINATE_SPACE_GRID_CELL),
    ),
    (
        "TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH",
        Number(TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH),
    ),
    (
        "MINIMUM_TARGET_BITS_PER_SECOND",
        Number(MINIMUM_TARGET_BITS_PER_SECOND),
    ),
    (
        "DEFAULT_ACTIVATION_TIMEOUT_US",
        Number(DEFAULT_ACTIVATION_TIMEOUT_US),
    ),
    (
        "MAX_ACTIVATION_TIMEOUT_US",
        Number(MAX_ACTIVATION_TIMEOUT_US),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn microphone_packet_is_twenty_milliseconds_of_mono_s16le() {
        assert_eq!(number("MIC_PACKET_US"), 20_000);
        assert_eq!(number("MIC_PACKET_BYTES"), 48_000 * 20 / 1000 * 2);
    }

    fn number(name: &str) -> u64 {
        constant_table()
            .iter()
            .find(|(key, _)| *key == name)
            .unwrap_or_else(|| panic!("{name} is missing from the constant table"))
            .1
            .as_number()
            .unwrap_or_else(|| panic!("{name} is not numeric"))
    }

    fn text(name: &str) -> &'static str {
        constant_table()
            .iter()
            .find(|(key, _)| *key == name)
            .unwrap_or_else(|| panic!("{name} is missing from the constant table"))
            .1
            .as_text()
            .unwrap_or_else(|| panic!("{name} is not a profile name"))
    }

    /// A duplicate name would silently shadow itself in a generated Python or TypeScript namespace.
    #[test]
    fn names_are_unique() {
        let mut seen = HashSet::new();
        for (name, _) in constant_table() {
            assert!(
                seen.insert(*name),
                "{name} appears twice in the constant table"
            );
        }
    }

    /// Every bit a peer may legitimately set has a name a binding can use. Adding a milestone,
    /// policy, operation class, or input class to the protocol widens its known mask, so this
    /// fails until the new bit is named here and therefore in every SDK.
    #[test]
    fn every_known_bit_is_named() {
        let groups: [(&str, u64, &str); 4] = [
            ("MILESTONE_", MILESTONE_KNOWN_MASK, "MILESTONE_KNOWN_MASK"),
            ("POLICY_", POLICY_KNOWN_MASK, "POLICY_KNOWN_MASK"),
            ("OP_", OP_KNOWN_MASK, "OP_KNOWN_MASK"),
            (
                "INPUT_CLASS_",
                INPUT_CLASS_KNOWN_MASK,
                "INPUT_CLASS_KNOWN_MASK",
            ),
        ];
        for (prefix, mask, mask_name) in groups {
            let named = constant_table()
                .iter()
                .filter(|(name, _)| name.starts_with(prefix) && *name != mask_name)
                .filter_map(|(_, value)| value.as_number())
                .fold(0_u64, |accumulated, bit| accumulated | bit);
            assert_eq!(
                named & mask,
                mask,
                "{prefix}* names cover {named:#x} but the protocol admits {mask:#x}"
            );
            assert_eq!(
                named & !mask,
                0,
                "{prefix}* names include a bit the protocol does not admit"
            );
        }
    }

    /// The table carries the protocol's values, not a transcription of them.
    #[test]
    fn values_come_from_the_protocol() {
        assert_eq!(text("PROFILE_CORE"), CORE_CONTROL);
        assert_eq!(text("PROFILE_DESKTOP_INPUT"), DESKTOP_INPUT);
        assert_eq!(text("SURFACE_TERMINAL"), TERMINAL_CONTENT);
        assert_eq!(number("MILESTONE_OUTPUT_READY"), MILESTONE_OUTPUT_READY);
        assert_eq!(number("LANE_BULK"), LaneClass::Bulk as u64);
        assert_eq!(number("ROLE_TERMINAL"), SurfaceRole::TerminalText as u64);
        assert_eq!(
            number("MAX_TRACK_WAIT_TIMEOUT_US"),
            MAX_TRACK_WAIT_TIMEOUT_US
        );
    }

    /// Every wait condition the SDK accepts is reachable by name, and each round-trips back to the
    /// condition it names.
    #[test]
    fn every_wait_condition_is_named() {
        for (name, value) in constant_table() {
            let Some(number) = value.as_number() else {
                continue;
            };
            if !name.starts_with("WAIT_") {
                continue;
            }
            TrackWaitCondition::try_from(number)
                .unwrap_or_else(|_| panic!("{name} does not name a wait condition"));
        }
        let named = constant_table()
            .iter()
            .filter(|(name, _)| name.starts_with("WAIT_"))
            .count();
        assert_eq!(
            named, 9,
            "the 1.5 media specification defines nine wait conditions"
        );
    }

    /// Slots and kinds follow §1 of the media specification, and a binding that reads them from
    /// here cannot drift from the table in the text.
    #[test]
    fn slots_and_kinds_match_the_media_specification() {
        assert_eq!(
            [
                number("SLOT_NONE"),
                number("SLOT_PRIMARY_VIDEO"),
                number("SLOT_AUDIO"),
                number("SLOT_RASTER"),
                number("SLOT_POSTER"),
            ],
            [0, 1, 2, 3, 4]
        );
        assert_eq!(
            [
                number("TRACK_KIND_VIDEO"),
                number("TRACK_KIND_AUDIO"),
                number("TRACK_KIND_RASTER"),
                number("TRACK_KIND_IMAGE"),
            ],
            [1, 2, 3, 4]
        );
    }

    /// A profile constant must be an exact wire name. A typo here would negotiate a profile that
    /// no presenter offers, and the failure would surface as a confusing rejection at connect.
    #[test]
    fn profile_names_are_wire_spellings() {
        for (name, value) in constant_table() {
            let Some(text) = value.as_text() else {
                continue;
            };
            assert!(
                name.starts_with("PROFILE_") || name.starts_with("SURFACE_"),
                "{name} carries a string but is not a profile constant"
            );
            assert!(
                text.ends_with("-v1"),
                "{name} = {text:?} is not a versioned profile name"
            );
            assert_eq!(
                text.trim(),
                text,
                "{name} = {text:?} has surrounding whitespace"
            );
        }
    }
}
