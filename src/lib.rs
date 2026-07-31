//! Full-duplex producer SDK for Vivid Protocol 1.5.
//!
//! The public object model deliberately follows the 1.5 wire model:
//!
//! - [`Surface`] is stable semantic, scene, policy, and input identity.
//! - [`Track`] is one immutable media configuration owned by a surface.
//! - [`TrackChannel`] is one authenticated transport generation for a track.
//!
//! There is no source, media-ticket, incremental-credit, or feature-ID compatibility layer.
//!
//! The crate is organized by wire concern — one module per object family — and every public item
//! is re-exported here, so `vivid_sdk::Session` and its neighbours keep the paths producers
//! already use. Module boundaries are an implementation detail; the public surface is this file.

#![forbid(unsafe_code)]

mod channel;
mod config;
mod handshake;
mod input;
mod lease;
mod offline;
mod resume;
mod scene;
mod session;
mod surface;

mod target;
#[cfg(feature = "testing")]
pub mod testing;
mod track;
mod wire;

// Crate-internal items are re-exported so each module's `use crate::*` resolves the shared
// helpers exactly as they resolved when this crate was one file. None of these are public.
pub(crate) use channel::{ChannelMediaState, FlowSync, close_track_flow};
pub(crate) use handshake::{build_hello, hello_lease_identity, producer_lease_identity};
pub(crate) use input::close_input_lane;
pub(crate) use lease::TrackReadyValues;
pub(crate) use offline::{endpoint, offline_contract, offline_endpoint, optional_endpoint};
pub(crate) use session::{ControlPlane, PendingInput, SessionLifecycle};
pub(crate) use surface::SurfaceLocal;
pub(crate) use target::{
    descriptor_settled, last_descriptor_key, offline_target_descriptor, settled_key,
    validate_target_descriptor,
};
pub(crate) use track::{TrackLocal, TrackMediaSequence, TrackRegistry};
pub(crate) use wire::{
    decoded_payload, ensure_live_surface, ensure_live_track, expect_record, invalid_data,
    invalid_input, lock, optional_map, optional_u64, presenter_error, random_bytes, required_bool,
    required_contract, required_i64, required_map, required_text, required_text_array,
    required_u32, required_u64, required_value, session_event, session_info, signed,
    validate_exact_payload_keys, validate_owner_pair, validate_payload_keys, validate_profiles,
    validate_track_owner, validate_track_tuple,
};

pub use channel::TrackChannel;
pub use config::{
    ConnectionFactory, PresenterError, ProducerAuthentication, ProducerConfig, RequestMetadata,
};
pub use input::{
    InputBindingStatus, InputGrantTermination, InputLane, InputLaneEvent, InputLeaseRenewal,
};
pub use lease::{ContextReady, SessionLeaseReady};
pub use scene::{SceneCommit, SlotBinding};
pub use session::{AnchorStatus, ChannelEvent, Session, SessionEvent, SessionInfo};
pub use surface::{Surface, SurfaceStatus};
pub use track::{Track, TrackStatus, TrackSupport, TrackWaitCondition, TrackWaitSatisfied};

pub use vivid_protocol::messages::LaneClass;
pub use vivid_protocol::wire::ConnectionKind;

pub use vivid_protocol::context::{
    ContextDefinition, OP_DELEGATE, OP_DESKTOP_INPUT, OP_KNOWN_MASK, OP_OBSERVE, OP_SCENE,
    OP_SURFACE_TRACK_MEDIA, OP_TERMINAL_ANCHOR,
};
pub use vivid_protocol::geometry::Rotation;
pub use vivid_protocol::input::{
    INPUT_CLASS_KEYBOARD, INPUT_CLASS_KNOWN_MASK, INPUT_CLASS_POINTER_AXIS,
    INPUT_CLASS_POINTER_BUTTON, INPUT_CLASS_POINTER_MOTION, InputBinding, InputEvent, InputGate,
    InputTuple,
};
pub use vivid_protocol::lease::{CleanupPolicy, SessionLeaseDefinition};
pub use vivid_protocol::media::RasterDeltaOperation;
pub use vivid_protocol::messages::ErrorDetail;
pub use vivid_protocol::registry::{
    CANVAS_CONTENT, CANVAS_SURFACE, CORE_CONTROL, DESKTOP_CONTENT, DESKTOP_INPUT, DESKTOP_SURFACE,
    GENERIC_CONTENT, LIVE_MEDIA, OBSERVABILITY, TERMINAL_CONTENT, TERMINAL_SURFACE, TIMED_MEDIA,
};
pub use vivid_protocol::scene::{Fit, SceneNode};
pub use vivid_protocol::surface::{
    CoordinateModel, DesktopSurfaceParameters, POLICY_DENY_CAPTURE, POLICY_DENY_DESCRIPTOR_EXPORT,
    POLICY_DENY_IMAGE_CACHE, POLICY_DENY_POSTER_RETENTION, POLICY_REDUCED_DIAGNOSTICS,
    SurfaceDefinition, SurfaceDescriptor, SurfaceRole, input_capability,
};
pub use vivid_protocol::target::{DesktopTarget, OutputDescriptor};
pub use vivid_protocol::track::{
    AudioConfiguration, ImageConfiguration, MILESTONE_BUFFERED_ENDED, MILESTONE_CHANNEL_ACCEPTED,
    MILESTONE_CHANNEL_DETACHED, MILESTONE_CLOCK_STARTED, MILESTONE_DECODER_INITIALIZED,
    MILESTONE_EOS_ACCEPTED, MILESTONE_FIRST_MEDIA, MILESTONE_KNOWN_MASK, MILESTONE_OUTPUT_READY,
    MILESTONE_PRESENTED, MILESTONE_RANDOM_ACCESS, MILESTONE_TRACK_LOST, RasterConfiguration,
    VideoConfiguration,
};

const MAX_CONTROL_EVENTS: usize = 1024;
const MAX_INPUT_EVENTS: usize = 1024;
const MAX_CHANNEL_EVENTS: usize = 256;
const DEFAULT_CONTROL_BODY: u32 = vivid_protocol::CONTROL_MAX_RECORD_BODY;
const OFFLINE_SESSION_ID: u64 = 1;
const OFFLINE_CONTEXT_ID: u64 = 1;
const OFFLINE_FLOW_BYTES: u64 = 1 << 60;
const OFFLINE_FLOW_RECORDS: u64 = 1 << 40;
const CARRIER_BINDING_NONE: [u8; 32] = [0; 32];

#[cfg(test)]
mod tests {
    use super::*;

    // The tests reach into crate internals, so they take the same imports the modules take.
    use crate::channel::push_channel_event;
    use crate::session::apply_track_lost;
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use vivid_protocol::anchor;
    use vivid_protocol::auth::{self, Secret32};
    use vivid_protocol::cbor::Value;
    use vivid_protocol::messages::{self, PayloadMap, TrackKind};

    use vivid_protocol::revision::{
        ChannelGeneration, GrantGeneration, InputEpoch, SurfaceGeneration, TargetGeneration,
        TrackRevision,
    };
    use vivid_protocol::track::{
        KindConfiguration, RasterConfiguration, TrackConfiguration, TrackMode,
    };

    fn surface(context_id: u64, surface_id: u64) -> SurfaceDefinition {
        SurfaceDefinition {
            context_id,
            surface_id,
            semantic_profile: GENERIC_CONTENT.into(),
            coordinate_model: CoordinateModel::DesktopLogicalPixels,
            logical_width: 2,
            logical_height: 2,
            scale_numerator: 1,
            scale_denominator: 1,
            rotation: 0,
            descriptor: SurfaceDescriptor {
                role: SurfaceRole::Figure,
                title: "test".into(),
                semantic_content_revision: 1,
                semantic_availability: 0,
                locator_hint: String::new(),
            },
            policy: 0,
            profile_parameters: vec![],
        }
    }

    fn raster_track(context_id: u64, surface_id: u64, track_id: u64) -> TrackConfiguration {
        TrackConfiguration {
            context_id,
            surface_id,
            track_id,
            slot: 3,
            mode: TrackMode::Live,
            lane: LaneClass::Bulk,
            maximum_record_body: 88,
            maximum_rate_millihertz: 60_000,
            maximum_encoded_bits_per_second: 1_000_000,
            maximum_records_per_second: 60,
            maximum_inflight_body_bytes: 176,
            kind: KindConfiguration::Raster(RasterConfiguration {
                width: 2,
                height: 2,
                alpha_mode: 1,
                delta_enabled: false,
                maximum_delta_operations: 1,
                zstd_enabled: false,
            }),
            target_latency_us: 16_000,
            maximum_latency_us: 100_000,
            retained_pixel_charge: 4,
        }
    }

    #[test]
    fn offline_lifecycle_uses_surfaces_tracks_and_ordered_eos() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        let media_sequence = channel
            .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
        let eos_sequence = channel.eos().unwrap();
        assert!(eos_sequence > media_sequence);
        assert_eq!(surface.id(), 7);
        assert_eq!(track.surface_id(), 7);
        assert_eq!(track.channel_generation(), ChannelGeneration::ONE);
        session.close().unwrap();
    }

    #[test]
    fn track_status_may_lag_an_independently_ordered_media_connection() {
        let mut submitted = TrackMediaSequence {
            last_id: 12,
            last_epoch: 3,
            last_record_sequence: 14,
        };
        let mut status = TrackStatus {
            context_id: 1,
            surface_id: 2,
            track_id: 3,
            revision: TrackRevision::new(4),
            kind: TrackKind::Video,
            mode: TrackMode::Timed,
            lifecycle: 2,
            channel_generation: ChannelGeneration::ONE,
            attachment_state: 1,
            milestones: MILESTONE_OUTPUT_READY,
            media_epoch: 2,
            last_media_id: 9,
            last_media_record_sequence: 11,
            last_decoded_pts_us: 0,
            last_presented_pts_us: 0,
            last_presentation_id: 0,
            cumulative_body_bytes: 100,
            cumulative_media_records: 9,
            maximum_body_bytes: 1_000,
            maximum_media_records: 100,
            ingress_depth_bucket: 0,
            playback_state: None,
            terminal_loss_code: None,
        };

        submitted.reconcile_status(&status, false);
        assert_eq!(
            submitted,
            TrackMediaSequence {
                last_id: 12,
                last_epoch: 3,
                last_record_sequence: 14,
            }
        );

        status.last_media_id = 15;
        status.media_epoch = 4;
        status.last_media_record_sequence = 2;
        submitted.reconcile_status(&status, true);
        assert_eq!(
            submitted,
            TrackMediaSequence {
                last_id: 15,
                last_epoch: 4,
                last_record_sequence: 2,
            }
        );
    }

    #[test]
    fn node_mutations_advance_the_scene_revision_without_touching_the_surface() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let node = SceneNode {
            owning_context_id: 1,
            node_id: 4,
            surface_context_id: 1,
            surface_id: 7,
            geometry: vec![
                (0, Value::Unsigned(1)),
                (1, Value::Unsigned(0)),
                (2, Value::Unsigned(0)),
                (3, Value::Unsigned(4 << 32)),
                (4, Value::Unsigned(2 << 32)),
                (5, Value::Unsigned(1)),
            ],
            fit: Fit::Contain,
            linear_sampling: true,
            z_index: 0,
            visible: true,
            opacity: u16::MAX,
            clip: None,
        };
        let revision = surface.revision();
        let generation = surface.generation();

        let created = session
            .create_node(&node, &RequestMetadata::default())
            .unwrap();
        let mut hidden = node.clone();
        hidden.visible = false;
        let updated = session
            .update_node(&hidden, &RequestMetadata::default())
            .unwrap();
        let deleted = session
            .delete_node(1, 4, &RequestMetadata::default())
            .unwrap();

        assert!(updated.scene_revision > created.scene_revision);
        assert!(deleted.scene_revision > updated.scene_revision);
        // Scene placement is not surface state: the surface keeps its identity, revision, and
        // coordinate generation across every node mutation.
        assert_eq!(surface.id(), 7);
        assert_eq!(surface.revision(), revision);
        assert_eq!(surface.generation(), generation);
    }

    #[test]
    fn node_deletion_requires_complete_identity() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        assert!(
            session
                .delete_node(0, 4, &RequestMetadata::default())
                .is_err()
        );
        assert!(
            session
                .delete_node(1, 0, &RequestMetadata::default())
                .is_err()
        );
    }

    #[test]
    fn track_replacement_preserves_surface_generation() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = session
            .create_surface(surface(1, 5), &RequestMetadata::default())
            .unwrap();
        let first = session
            .create_track(raster_track(1, 5, 1), &RequestMetadata::default())
            .unwrap();
        let second = session
            .create_track(raster_track(1, 5, 2), &RequestMetadata::default())
            .unwrap();
        let generation = surface.generation();
        session
            .destroy_track(&first, &RequestMetadata::default())
            .unwrap();
        assert_eq!(surface.generation(), generation);
        assert_eq!(second.surface_id(), surface.id());
    }

    #[test]
    fn complete_owner_identity_prevents_same_id_aliasing() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let first = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let second = session
            .create_surface(surface(2, 7), &RequestMetadata::default())
            .unwrap();
        session
            .destroy_surface(&first, &RequestMetadata::default())
            .unwrap();
        assert_eq!(second.context_id(), 2);
        assert_eq!(second.id(), 7);
        assert!(!lock(&second.inner, "surface").unwrap().destroyed);
    }

    #[test]
    fn config_rejects_feature_id_style_or_unsorted_profiles() {
        let mut config = ProducerConfig::offline();
        config.required_profiles.reverse();
        assert!(config.validate().is_err());
    }

    #[test]
    fn the_desktop_preset_is_prerequisite_closed_and_selects_the_desktop_target() {
        let config = ProducerConfig::desktop();
        config
            .validate()
            .expect("the desktop preset is a legal offer");
        assert_eq!(config.target_profile, DESKTOP_SURFACE);
        // desktop-input-v1 declares desktop-surface-v1 and live-media-v1 as prerequisites, so an
        // offer that omitted either would be refused by validate() rather than by the presenter.
        assert!(config.optional_profiles.iter().any(|p| p == DESKTOP_INPUT));
        assert!(config.required_profiles.iter().any(|p| p == LIVE_MEDIA));
        assert!(config.required_profiles.iter().any(|p| p == CORE_CONTROL));
    }

    #[test]
    fn a_dry_run_desktop_session_reports_a_real_desktop_target() {
        // The dry run has to exercise the same coordinate math as a live desktop session, so its
        // target is a parseable single-output topology, not a fabricated terminal grid.
        let session = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let info = session.info();
        assert_eq!(info.target_profile, DESKTOP_SURFACE);
        let target = info
            .desktop_target()
            .expect("a desktop session has a desktop target");
        assert_eq!((target.width, target.height), (1920, 1080));
        assert_eq!(target.outputs.len(), 1);
        assert!(target.outputs[0].primary);
        assert!(info.target_settled().unwrap());

        // A terminal session has no desktop target at all rather than a coerced one.
        let terminal = Session::connect(ProducerConfig::offline()).unwrap();
        assert_eq!(terminal.info().target_profile, TERMINAL_SURFACE);
        assert!(terminal.info().desktop_target().is_none());
    }

    #[test]
    fn target_changes_are_validated_against_the_negotiated_profile() {
        // A desktop descriptor reaching a terminal session, or the reverse, must be refused: the
        // keys parse either way, and only the profile says which shape is meaningful.
        let mut desktop = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let mut terminal = Session::connect(ProducerConfig::offline()).unwrap();

        let mut desktop_change = crate::target::offline_desktop_descriptor();
        desktop_change.push((9, Value::Unsigned(2)));
        desktop_change.push((10, Value::Unsigned(1)));
        let mut terminal_change = crate::target::offline_terminal_descriptor();
        terminal_change.push((9, Value::Unsigned(2)));
        terminal_change.push((10, Value::Unsigned(1)));

        assert_eq!(
            desktop.apply_target_changed(&desktop_change).unwrap(),
            TargetGeneration::new(2)
        );
        assert_eq!(
            terminal.apply_target_changed(&terminal_change).unwrap(),
            TargetGeneration::new(2)
        );
        assert!(desktop.apply_target_changed(&terminal_change).is_err());
        assert!(terminal.apply_target_changed(&desktop_change).is_err());

        // Those two are refused by the key schema, which is the cheap check. Drive the profile
        // validator directly to prove it refuses a same-shaped descriptor that is wrong for the
        // profile: a terminal descriptor truncated to the desktop key range still parses as a
        // map, and only the desktop validator knows its outputs list is missing.
        let truncated: PayloadMap = crate::target::offline_terminal_descriptor()
            .into_iter()
            .filter(|(key, _)| *key <= 6)
            .collect();
        assert!(crate::target::validate_target_descriptor(DESKTOP_SURFACE, &truncated).is_err());
        assert!(
            crate::target::validate_target_descriptor(
                DESKTOP_SURFACE,
                &crate::target::offline_desktop_descriptor()
            )
            .is_ok()
        );
        // An unrecognized target profile is refused rather than waved through.
        assert!(
            crate::target::validate_target_descriptor(
                CANVAS_SURFACE,
                &crate::target::offline_desktop_descriptor()
            )
            .is_err()
        );
    }

    #[test]
    fn a_desktop_target_change_still_enforces_the_settle_rule() {
        // Terminal §2 and desktop §1 both forbid reusing a generation except for one final
        // settle, and the settle flag lives at a different key in each profile.
        let mut session = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let unsettled = DesktopTarget {
            origin_x: 0,
            origin_y: 0,
            width: 1920,
            height: 1080,
            outputs: vec![OutputDescriptor {
                output_id: 1,
                origin_x: 0,
                origin_y: 0,
                width: 1920,
                height: 1080,
                scale_numerator: 1,
                scale_denominator: 1,
                rotation: Rotation::None,
                primary: true,
            }],
            settled: false,
            topology_revision: 2,
        };
        let mut change = unsettled.encode();
        change.push((9, Value::Unsigned(2)));
        change.push((10, Value::Unsigned(1)));
        session.apply_target_changed(&change).unwrap();
        assert!(!session.info().target_settled().unwrap());

        // The same generation may repeat exactly once, to settle identical geometry.
        let mut settled = unsettled.clone();
        settled.settled = true;
        let mut settle_change = settled.encode();
        settle_change.push((9, Value::Unsigned(2)));
        settle_change.push((10, Value::Unsigned(1)));
        session.apply_target_changed(&settle_change).unwrap();
        assert!(session.info().target_settled().unwrap());

        // A second settle at the same generation is a protocol error, not an idempotent no-op.
        assert!(session.apply_target_changed(&settle_change).is_err());
    }

    #[test]
    fn typed_desktop_surface_parameters_round_trip_through_a_surface() {
        // Desktop §2 keys 0-4 have a typed builder so no producer hand-encodes the map, which is
        // where the topology sanitization rule would otherwise be dropped.
        let parameters = DesktopSurfaceParameters {
            captured_origin_x: -1920,
            captured_origin_y: 0,
            topology: vec![OutputDescriptor {
                output_id: 1,
                origin_x: -1920,
                origin_y: 0,
                width: 1920,
                height: 1080,
                scale_numerator: 1,
                scale_denominator: 1,
                rotation: Rotation::None,
                primary: true,
            }],
            semantic_generation: 1,
            input_capabilities: input_capability::KEYBOARD | input_capability::POINTER_MOTION,
        };
        let encoded = parameters.encode();
        let decoded = DesktopSurfaceParameters::decode(&encoded).unwrap();
        assert_eq!(decoded.captured_origin_x, -1920);
        assert_eq!(decoded.semantic_generation, 1);
        assert_eq!(decoded.topology.len(), 1);

        let mut session = Session::connect(ProducerConfig::offline_desktop()).unwrap();
        let mut definition = surface(1, 1);
        definition.semantic_profile = DESKTOP_CONTENT.into();
        definition.profile_parameters = encoded;
        let created = session
            .create_surface(definition, &RequestMetadata::default())
            .unwrap();
        let stored = created.definition().unwrap().profile_parameters;
        assert_eq!(
            DesktopSurfaceParameters::decode(&stored)
                .unwrap()
                .input_capabilities,
            input_capability::KEYBOARD | input_capability::POINTER_MOTION
        );
    }

    #[test]
    fn secret_bearing_configuration_has_no_debug_surface() {
        let authentication = ProducerAuthentication::root_hex(&"11".repeat(32)).unwrap();
        let config = ProducerConfig {
            authentication,
            dry_run: true,
            ..ProducerConfig::default()
        };
        let session = Session::connect(config).unwrap();
        let debug = format!("{session:?}");
        assert!(!debug.contains(&"11".repeat(8)));
        assert!(!debug.contains("ROOT_SECRET"));
    }

    #[test]
    fn surface_mapping_change_advances_only_surface_generation() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let handle = session
            .create_surface(surface(1, 3), &RequestMetadata::default())
            .unwrap();
        let mut replacement = handle.definition().unwrap();
        replacement.logical_width = 3;
        session
            .update_surface(&handle, replacement, &RequestMetadata::default())
            .unwrap();
        assert_eq!(handle.generation().get(), 2);
        assert_eq!(handle.revision().get(), 2);
    }

    #[test]
    fn desktop_input_requires_explicit_profile_and_uses_a_separate_lane() {
        let default_session = Session::connect(ProducerConfig::offline()).unwrap();
        assert_eq!(
            default_session.open_input_lane(1).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );

        let mut config = ProducerConfig::offline();
        config.target_profile = DESKTOP_SURFACE.into();
        config.required_profiles = vec![
            DESKTOP_INPUT.into(),
            DESKTOP_SURFACE.into(),
            LIVE_MEDIA.into(),
            CORE_CONTROL.into(),
        ];
        config.optional_profiles = vec![];
        let session = Session::connect(config).unwrap();
        let lane = session.open_input_lane(1).unwrap();
        assert_eq!(lane.generation(), 1);
        let status = lane
            .set_binding(&InputBinding {
                producer_epoch: InputEpoch::ONE,
                context_id: 1,
                surface_id: 7,
                surface_generation: SurfaceGeneration::ONE,
                requested_classes: INPUT_CLASS_KEYBOARD,
                reason: 6,
                requested_watchdog_us: 2_000_000,
            })
            .unwrap();
        assert_eq!(status.state, 1);
        assert_eq!(status.effective_classes, INPUT_CLASS_KEYBOARD);
        lane.close().unwrap();
    }

    #[test]
    fn input_events_remain_generation_qualified_until_final_injection_gate() {
        let binding = InputTuple {
            producer_epoch: InputEpoch::ONE,
            grant_generation: GrantGeneration::ONE,
            context_id: 1,
            surface_id: 7,
            surface_generation: SurfaceGeneration::ONE,
        };
        let key = InputEvent::Key {
            binding,
            usage: 0x04,
            pressed: true,
        };
        let raw = InputLaneEvent::Input {
            record_type: messages::KEY_INPUT,
            surface_id: 7,
            payload: key.payload(),
        };
        assert_eq!(raw.decode_input(1920, 1080).unwrap(), key);

        let wrong_owner = InputLaneEvent::Input {
            record_type: messages::KEY_INPUT,
            surface_id: 8,
            payload: key.payload(),
        };
        assert!(wrong_owner.decode_input(1920, 1080).is_err());
    }

    #[test]
    fn input_and_track_queue_pressure_fail_closed_without_dropping_transitions() {
        let pending = PendingInput {
            requests: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::from([InputLaneEvent::Input {
                record_type: messages::KEY_INPUT,
                surface_id: 7,
                payload: vec![],
            }])),
            closed: AtomicBool::new(false),
        };
        close_input_lane(&pending, "test lane loss");
        assert!(pending.closed.load(Ordering::Acquire));
        let events = lock(&pending.events, "input events").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events.front(),
            Some(InputLaneEvent::LaneClosed { diagnostic })
                if diagnostic == "test lane loss"
        ));
        drop(events);

        let channel_events = Mutex::new(VecDeque::from(vec![
            ChannelEvent::NeedKeyframe(vec![]);
            MAX_CHANNEL_EVENTS
        ]));
        assert!(push_channel_event(&channel_events, ChannelEvent::NeedFullFrame(vec![])).is_err());
        assert_eq!(
            lock(&channel_events, "channel events").unwrap().len(),
            MAX_CHANNEL_EVENTS
        );
    }

    #[test]
    fn control_loss_stops_existing_media_and_input_lanes() {
        let mut config = ProducerConfig::offline();
        config.target_profile = DESKTOP_SURFACE.into();
        config.required_profiles = vec![
            DESKTOP_INPUT.into(),
            DESKTOP_SURFACE.into(),
            LIVE_MEDIA.into(),
            CORE_CONTROL.into(),
        ];
        config.optional_profiles = vec![];
        let mut session = Session::connect(config).unwrap();
        let surface = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        let lane = session.open_input_lane(1).unwrap();

        drop(session);

        assert_eq!(
            channel
                .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            lane.set_binding(&InputBinding {
                producer_epoch: InputEpoch::ONE,
                context_id: 1,
                surface_id: surface.id(),
                surface_generation: surface.generation(),
                requested_classes: INPUT_CLASS_KEYBOARD,
                reason: 6,
                requested_watchdog_us: 2_000_000,
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(matches!(
            lane.take_event().unwrap(),
            Some(InputLaneEvent::LaneClosed { .. })
        ));
    }

    #[test]
    fn channel_advance_closes_old_transport_and_preserves_track_media_ids() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let first = session.open_track_channel(&track).unwrap();
        first
            .send_raster(0, 5, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();

        assert_eq!(
            session
                .advance_channel(&track, 1, &RequestMetadata::default())
                .unwrap(),
            ChannelGeneration::new(2)
        );
        assert_eq!(
            first
                .send_raster(0, 6, &[0, 0, 0, 255].repeat(4), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );

        let second = session.open_track_channel(&track).unwrap();
        assert!(
            second
                .send_raster(0, 5, &[0, 0, 0, 255].repeat(4), false)
                .is_err()
        );
        second
            .send_raster(0, 6, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn status_wait_and_flush_keep_generation_and_epoch_domains_separate() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        channel
            .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();

        let surface_status = session.query_surface(&surface).unwrap();
        assert_eq!(surface_status.generation, SurfaceGeneration::ONE);
        let track_status = session.query_track(&track).unwrap();
        assert_eq!(track_status.channel_generation, ChannelGeneration::ONE);
        assert_eq!(track_status.last_media_id, 1);
        assert_eq!(track_status.cumulative_media_records, 1);
        let waited = session
            .wait_track(
                &track,
                TrackWaitCondition::MilestoneSet,
                Some(MILESTONE_CHANNEL_ACCEPTED),
                1_000_000,
            )
            .unwrap();
        assert_eq!(waited.channel_generation, ChannelGeneration::ONE);
        assert_eq!(
            session
                .wait_track(
                    &track,
                    TrackWaitCondition::PlaybackEnded,
                    None,
                    vivid_protocol::MAX_TRACK_WAIT_TIMEOUT_US + 1,
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        session.flush(&track, 1).unwrap();
        assert!(session.flush(&track, 1).is_err());
        assert!(
            channel
                .send_raster(0, 2, &[0, 0, 0, 255].repeat(4), false)
                .is_err()
        );
        channel
            .send_raster(1, 2, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn resumed_session_can_adopt_owner_qualified_handles_and_advance_channels() {
        let mut original = Session::connect(ProducerConfig::offline()).unwrap();
        let surface = original
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        let track = original
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let old_channel = original.open_track_channel(&track).unwrap();
        old_channel
            .send_raster(0, 10, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
        drop(original);

        let mut resumed = Session::connect(ProducerConfig::offline()).unwrap();
        resumed.adopt_surface(&surface).unwrap();
        resumed.adopt_track(&track).unwrap();
        assert!(resumed.allocate_id().unwrap() > 9);
        resumed
            .advance_channel(&track, 1, &RequestMetadata::default())
            .unwrap();
        let channel = resumed.open_track_channel(&track).unwrap();
        channel
            .send_raster(0, 11, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn leased_sessions_prepare_secret_redacted_resume_authentication() {
        let root = Session::connect(ProducerConfig::offline()).unwrap();
        let Err(error) = root.resume_authentication() else {
            panic!("root session unexpectedly produced resume authentication");
        };
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);

        let mut config = ProducerConfig::offline();
        config.authentication = ProducerAuthentication::LeaseActivation {
            context_id: 4,
            lease_id: 8,
            activation_secret: Secret32::new([0x55; 32]),
            attempt_id: [0x44; auth::ATTEMPT_ID_BYTES],
            proof_of_possession: None,
        };
        let leased = Session::connect(config).unwrap();
        match leased.resume_authentication().unwrap() {
            ProducerAuthentication::Resume {
                context_id,
                lease_id,
                session_id,
                resume_generation,
                ..
            } => {
                assert_eq!((context_id, lease_id), (4, 8));
                assert_eq!(session_id, OFFLINE_SESSION_ID);
                assert_eq!(resume_generation, 0);
            }
            _ => panic!("leased session did not return resume authentication"),
        }
    }

    #[test]
    fn actionable_track_loss_is_scoped_by_complete_owner_identity() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        session
            .create_surface(surface(1, 7), &RequestMetadata::default())
            .unwrap();
        session
            .create_surface(surface(2, 7), &RequestMetadata::default())
            .unwrap();
        let first = session
            .create_track(raster_track(1, 7, 9), &RequestMetadata::default())
            .unwrap();
        let second = session
            .create_track(raster_track(2, 7, 9), &RequestMetadata::default())
            .unwrap();
        let first_channel = session.open_track_channel(&first).unwrap();
        let second_channel = session.open_track_channel(&second).unwrap();

        apply_track_lost(
            9,
            &vec![
                (0, Value::Unsigned(1)),
                (1, Value::Unsigned(7)),
                (2, Value::Unsigned(9)),
                (3, Value::Unsigned(18)),
                (4, Value::Unsigned(3)),
                (5, Value::Map(vec![])),
                (6, Value::Text("decoder lost".into())),
            ],
            &session.tracks,
        )
        .unwrap();

        assert!(lock(&first.inner, "track").unwrap().destroyed);
        assert!(!lock(&second.inner, "track").unwrap().destroyed);
        assert_eq!(
            first_channel
                .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        second_channel
            .send_raster(0, 1, &[0, 0, 0, 255].repeat(4), false)
            .unwrap();
    }

    #[test]
    fn terminal_target_changes_and_marker_transports_are_generation_safe() {
        let mut session = Session::connect(ProducerConfig::offline()).unwrap();
        let mut changed = session.info().target_descriptor.clone();
        changed[6].1 = Value::Bool(false);
        changed.push((9, Value::Unsigned(2)));
        changed.push((10, Value::Unsigned(1)));
        assert_eq!(
            session.apply_target_changed(&changed).unwrap(),
            TargetGeneration::new(2)
        );
        assert_eq!(session.info().target_generation, TargetGeneration::new(2));
        assert_eq!(session.info().target_descriptor[6].1.as_bool(), Some(false));

        let mut settled = changed.clone();
        settled[6].1 = Value::Bool(true);
        assert_eq!(
            session.apply_target_changed(&settled).unwrap(),
            TargetGeneration::new(2),
            "the final settle keeps the last geometry generation"
        );
        assert_eq!(session.info().target_descriptor[6].1.as_bool(), Some(true));
        assert!(
            session.apply_target_changed(&settled).is_err(),
            "a duplicate settled event must not masquerade as new target truth"
        );

        let mut changed_without_generation = settled.clone();
        changed_without_generation[3].1 = Value::Unsigned(25);
        assert!(
            session
                .apply_target_changed(&changed_without_generation)
                .is_err(),
            "geometry cannot change without advancing the target generation"
        );

        let context_id = session.info().root_context_id;
        let marker = session.conpty_anchor_marker(context_id, 77).unwrap();
        let parsed = anchor::parse_conpty_marker(&marker).unwrap();
        assert_eq!((parsed.context_id, parsed.anchor_id), (context_id, 77));
        let status = session.query_anchor(context_id, 77).unwrap();
        assert_eq!(status.state, 0);
        assert_eq!(status.target_generation, Some(TargetGeneration::new(2)));
    }
}
