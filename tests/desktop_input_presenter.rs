//! Production desktop input over real authenticated lanes and media channels.
#![cfg(feature = "presenter")]

// Rust guideline compliant 2026-10-07
use std::time::{Duration, Instant};
use vivid_protocol::{
    messages::LaneClass,
    revision::{InputEpoch, SurfaceGeneration},
    surface::CoordinateModel,
    target::DesktopTarget,
    track::{
        KindConfiguration, MILESTONE_OUTPUT_READY, RasterConfiguration, TrackConfiguration,
        TrackMode,
    },
};
use vivid_sdk::{
    DESKTOP_CONTENT, DesktopSurfaceParameters, INPUT_CLASS_KEYBOARD, INPUT_CLASS_KNOWN_MASK,
    InputBinding, InputEvent, InputLane, InputLaneEvent, ProducerAuthentication, ProducerConfig,
    RequestMetadata, Session, SlotBinding, Surface, SurfaceDefinition, SurfaceDescriptor,
    TrackChannel, TrackWaitCondition,
    presenter::{
        MediaConfig, PresenterConfig, PresenterListener as _, SocketListener, SourceKey,
        VirtualVivid,
    },
};

fn target() -> DesktopTarget {
    DesktopTarget {
        origin_x: 0,
        origin_y: 0,
        width: 2,
        height: 2,
        outputs: vec![],
        settled: true,
        topology_revision: 1,
    }
}

fn start() -> (VirtualVivid, String) {
    let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured(
        listener,
        PresenterConfig::desktop_with_input(MediaConfig::default(), target()),
        None,
    )
    .unwrap();
    (presenter, endpoint)
}

struct Peer {
    session: Session,
    surface: Surface,
    channel: TrackChannel,
    track: vivid_sdk::Track,
    lane: InputLane,
    source: SourceKey,
}

impl Peer {
    fn new(presenter: &VirtualVivid, endpoint: &str, pane: u64, presented: bool) -> Self {
        Self::with_capability(presenter, endpoint, pane, presented).0
    }

    fn with_capability(
        presenter: &VirtualVivid,
        endpoint: &str,
        pane: u64,
        presented: bool,
    ) -> (Self, String) {
        let capability = presenter.issue_pane_capability(pane).unwrap();
        let mut session = Session::connect(ProducerConfig {
            endpoint_control: Some(endpoint.into()),
            authentication: ProducerAuthentication::root_hex(&capability).unwrap(),
            ..ProducerConfig::desktop()
        })
        .unwrap();
        let context = session.info().root_context_id;
        let surface = session
            .create_surface(
                SurfaceDefinition {
                    context_id: context,
                    surface_id: 1,
                    semantic_profile: DESKTOP_CONTENT.into(),
                    coordinate_model: CoordinateModel::DesktopLogicalPixels,
                    logical_width: 2,
                    logical_height: 2,
                    scale_numerator: 1,
                    scale_denominator: 1,
                    rotation: 0,
                    descriptor: SurfaceDescriptor {
                        role: vivid_sdk::SurfaceRole::Desktop,
                        title: String::new(),
                        semantic_content_revision: 1,
                        semantic_availability: 0,
                        locator_hint: String::new(),
                    },
                    policy: 0,
                    profile_parameters: DesktopSurfaceParameters {
                        captured_origin_x: 0,
                        captured_origin_y: 0,
                        topology: vec![],
                        semantic_generation: 1,
                        input_capabilities: INPUT_CLASS_KNOWN_MASK,
                    }
                    .encode(),
                },
                &RequestMetadata::default(),
            )
            .unwrap();
        let track = session
            .create_track(
                TrackConfiguration {
                    direction: Default::default(),
                    context_id: context,
                    surface_id: 1,
                    track_id: 1,
                    slot: 1,
                    mode: TrackMode::Live,
                    lane: LaneClass::Bulk,
                    maximum_record_body: 1024,
                    maximum_rate_millihertz: 60_000,
                    maximum_encoded_bits_per_second: 1_000_000,
                    maximum_records_per_second: 60,
                    maximum_inflight_body_bytes: 1024,
                    kind: KindConfiguration::Raster(RasterConfiguration {
                        width: 2,
                        height: 2,
                        alpha_mode: 1,
                        delta_enabled: false,
                        maximum_delta_operations: 1,
                        zstd_enabled: false,
                    }),
                    target_latency_us: 0,
                    maximum_latency_us: 0,
                    retained_pixel_charge: 4,
                },
                &RequestMetadata::default(),
            )
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        channel.send_raster(0, 1, &[255; 16], false).unwrap();
        session
            .wait_track(
                &track,
                TrackWaitCondition::MilestoneSet,
                Some(MILESTONE_OUTPUT_READY),
                5_000_000,
            )
            .unwrap();
        session
            .activate_tracks(
                &surface,
                &[SlotBinding {
                    slot: 1,
                    track_id: 1,
                    expected_channel_generation: track.channel_generation(),
                    required_milestone: MILESTONE_OUTPUT_READY,
                }],
                &RequestMetadata::default(),
            )
            .unwrap();
        let source = SourceKey {
            producer: session.info().session_id,
            context,
            surface: 1,
            track: 1,
        };
        assert!(
            presenter.desktop_input_target(source).is_err(),
            "accepted media is not presentation proof"
        );
        if presented {
            let event = presenter
                .wait_media_event(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            presenter.complete_bridge_delivery(event.delivery_id, true);
        }
        let lane = session.open_input_lane(1).unwrap();
        (
            Self {
                session,
                surface,
                channel,
                track,
                lane,
                source,
            },
            capability,
        )
    }

    fn binding(&self, epoch: u64) -> InputBinding {
        InputBinding {
            producer_epoch: InputEpoch::new(epoch),
            context_id: self.surface.context_id(),
            surface_id: 1,
            surface_generation: self.surface.generation(),
            requested_classes: INPUT_CLASS_KEYBOARD,
            reason: 6,
            requested_watchdog_us: 250_000,
        }
    }

    fn next(&self) -> InputLaneEvent {
        self.lane
            .wait_event(Duration::from_secs(3))
            .unwrap()
            .expect("input event")
    }
}

#[test]
fn session_status_pages_cover_slots_and_input_without_cross_owner_cursor_invalidation() {
    use vivid_protocol::session_status::{Kind, Query};
    let (presenter, endpoint) = start();
    let mut first = Peer::new(&presenter, &endpoint, 1, true);
    let mut second = Peer::new(&presenter, &endpoint, 2, true);
    let query = Query {
        maximum_entries: 1,
        ..Default::default()
    };
    let initial = first.session.query_session_page(&query).unwrap();
    assert!(initial.input_disabled);
    assert_eq!(initial.usage, [1, 1, 0]);
    let continuation = Query {
        cursor: initial.cursor.clone(),
        ..query.clone()
    };
    let foreign = second
        .session
        .query_session_page(&continuation)
        .unwrap_err();
    assert_eq!(
        foreign
            .get_ref()
            .unwrap()
            .downcast_ref::<vivid_sdk::PresenterError>()
            .unwrap()
            .code,
        vivid_protocol::messages::ERROR_PRECONDITION_FAILED
    );
    let mut changed = second.surface.definition().unwrap();
    changed.descriptor.title = "independent owner".into();
    changed.descriptor.semantic_content_revision += 1;
    second
        .session
        .update_surface(&second.surface, changed, &RequestMetadata::default())
        .unwrap();
    let mut page = initial.clone();
    let mut entries = page.entries.clone();
    while let Some(cursor) = page.cursor.take() {
        page = first
            .session
            .query_session_page(&Query {
                cursor: Some(cursor),
                ..query.clone()
            })
            .unwrap();
        assert_eq!(page.session_revision, initial.session_revision);
        entries.extend(page.entries.iter().copied());
    }
    assert_eq!(
        entries.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
        vec![Kind::Context, Kind::Surface, Kind::Track, Kind::Slot]
    );
    assert_eq!(entries[2].object_id, first.track.id());
    assert_eq!(entries[3].detail, first.track.id());
    let target = presenter.desktop_input_target(first.source).unwrap();
    presenter
        .set_desktop_input_target(Some(target), INPUT_CLASS_KEYBOARD)
        .unwrap();
    assert_eq!(first.lane.set_binding(&first.binding(1)).unwrap().state, 1);
    let armed = first.session.query_session_page(&query).unwrap();
    assert!(!armed.input_disabled);
    assert!(armed.session_revision > initial.session_revision);
    assert!(first.session.query_session_page(&continuation).is_err());
    let mut changed = first.surface.definition().unwrap();
    changed.descriptor.title = "new revision".into();
    changed.descriptor.semantic_content_revision += 1;
    first
        .session
        .update_surface(&first.surface, changed, &RequestMetadata::default())
        .unwrap();
    assert!(
        first
            .session
            .query_session_page(&Query {
                cursor: armed.cursor,
                ..query
            })
            .is_err()
    );
    assert_eq!(
        second
            .session
            .query_session_page(&Default::default())
            .unwrap()
            .usage,
        [1, 1, 0]
    );
}

#[test]
fn session_status_respects_small_negotiated_bodies_and_pages_every_surface() {
    use vivid_protocol::session_status::{Kind, Query};
    let (presenter, endpoint) = start();
    let original = Peer::new(&presenter, &endpoint, 1, true);
    let secret = presenter.issue_pane_capability(2).unwrap();
    let mut session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        maximum_control_body: 1024,
        ..ProducerConfig::desktop()
    })
    .unwrap();
    for id in 1..=60 {
        let mut definition = original.surface.definition().unwrap();
        definition.context_id = session.info().root_context_id;
        definition.surface_id = id;
        session
            .create_surface(definition, &RequestMetadata::default())
            .unwrap();
    }
    let mut query = Query::default();
    let first = session.query_session_page(&query).unwrap();
    assert!(first.entries.len() < 61 && first.cursor.is_some());
    let revision = first.session_revision;
    let mut entries = Vec::new();
    let mut page = first;
    loop {
        assert_eq!(page.usage, [60, 0, 0]);
        assert_eq!(page.session_revision, revision);
        entries.extend(page.entries.iter().copied());
        let Some(cursor) = page.cursor else {
            break;
        };
        query.cursor = Some(cursor);
        page = session.query_session_page(&query).unwrap();
    }
    assert_eq!(entries.len(), 61);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.kind == Kind::Surface)
            .map(|entry| entry.surface_id)
            .collect::<Vec<_>>(),
        (1..=60).collect::<Vec<_>>()
    );
    assert_eq!(
        original
            .session
            .query_session_page(&Default::default())
            .unwrap()
            .usage,
        [1, 1, 0]
    );
}

#[test]
fn eligibility_epochs_and_owner_isolation_use_real_authenticated_lanes() {
    let (presenter, endpoint) = start();
    let first = Peer::new(&presenter, &endpoint, 1, true);
    let second = Peer::new(&presenter, &endpoint, 2, true);
    assert_eq!(first.source.context, second.source.context);
    assert_eq!(first.source.surface, second.source.surface);
    assert_ne!(first.source.producer, second.source.producer);
    let a = presenter.desktop_input_target(first.source).unwrap();
    let b = presenter.desktop_input_target(second.source).unwrap();
    let denied = first.binding(1);
    assert_eq!(first.lane.set_binding(&denied).unwrap().state, 2);
    presenter
        .set_desktop_input_target(Some(a), INPUT_CLASS_KEYBOARD)
        .unwrap();
    assert_eq!(
        first.lane.set_binding(&denied).unwrap().state,
        2,
        "focus does not reinstate an old epoch"
    );
    assert_eq!(first.lane.set_binding(&first.binding(2)).unwrap().state, 1);
    let binding = presenter.desktop_input_binding(a).unwrap();
    let press = InputEvent::Key {
        binding,
        usage: 0x04,
        pressed: true,
    };
    assert!(presenter.send_desktop_input(a, press).unwrap());
    assert!(
        !presenter.send_desktop_input(a, press).unwrap(),
        "synthetic repeat suppressed"
    );
    assert_eq!(first.next().decode_input(2, 2).unwrap(), press);
    presenter
        .set_desktop_input_target(Some(b), INPUT_CLASS_KEYBOARD)
        .unwrap();
    assert!(matches!(first.next(), InputLaneEvent::Revoked(_)));
    assert!(!presenter.send_desktop_input(a, press).unwrap());
    assert_eq!(
        second.lane.set_binding(&second.binding(1)).unwrap().state,
        1
    );
    let peer_binding = presenter.desktop_input_binding(b).unwrap();
    presenter.revoke_pane(1);
    let other = InputEvent::Key {
        binding: peer_binding,
        usage: 0x05,
        pressed: true,
    };
    assert!(
        presenter.send_desktop_input(b, other).unwrap(),
        "teardown must preserve the other owner"
    );
    assert_eq!(second.next().decode_input(2, 2).unwrap(), other);
    let _ = first.session.close();
    second.session.close().unwrap();
}

#[test]
fn renewals_lane_replacement_and_target_changes_never_restore_old_grants() {
    let (presenter, endpoint) = start();
    let peer = Peer::new(&presenter, &endpoint, 1, true);
    let eligible = presenter.desktop_input_target(peer.source).unwrap();
    presenter
        .set_desktop_input_target(Some(eligible), INPUT_CLASS_KNOWN_MASK)
        .unwrap();
    let request = peer.binding(1);
    peer.lane.set_binding(&request).unwrap();
    let original = presenter.desktop_input_binding(eligible).unwrap();
    // Occupy the source's one-record flow allowance without retiring delivery.
    // Renewal and revocation must remain independent of saturated bulk traffic.
    peer.channel.send_raster(0, 2, &[0; 16], false).unwrap();
    let _blocked_media = presenter
        .wait_media_event(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut renewals = 0;
    while renewals < 3 && Instant::now() < deadline {
        if let InputLaneEvent::Renew(renewal) = peer.next() {
            assert_eq!(renewal.binding, original);
            renewals += 1;
        }
    }
    assert_eq!(renewals, 3);
    assert_eq!(presenter.desktop_input_binding(eligible), Some(original));
    assert!(
        peer.session.open_input_lane(1).is_err(),
        "lane generations cannot be replayed"
    );
    let new_lane = peer.session.open_input_lane(2).unwrap();
    assert!(presenter.desktop_input_binding(eligible).is_none());
    presenter
        .set_desktop_input_target(Some(eligible), INPUT_CLASS_KEYBOARD)
        .unwrap();
    assert_ne!(new_lane.set_binding(&request).unwrap().state, 1);
    let fresh = peer.binding(2);
    new_lane.set_binding(&fresh).unwrap();
    let current = presenter.desktop_input_binding(eligible).unwrap();
    assert!(current.grant_generation > original.grant_generation);
    presenter.update_desktop_target(1, target(), 1).unwrap();
    assert!(presenter.desktop_input_binding(eligible).is_none());
    assert!(
        presenter
            .set_desktop_input_target(Some(eligible), INPUT_CLASS_KEYBOARD)
            .is_err()
    );
    assert!(matches!(
        new_lane.wait_event(Duration::from_secs(2)).unwrap(),
        Some(InputLaneEvent::Revoked(_))
    ));
    peer.session.close().unwrap();
}

#[test]
fn old_media_ack_cannot_authorize_a_new_surface_generation() {
    let (presenter, endpoint) = start();
    let mut peer = Peer::new(&presenter, &endpoint, 1, false);
    let old_event = presenter
        .wait_media_event(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let mut replacement = peer.surface.definition().unwrap();
    replacement.scale_numerator = 2;
    peer.session
        .update_surface(&peer.surface, replacement, &RequestMetadata::default())
        .unwrap();
    assert_eq!(peer.surface.generation(), SurfaceGeneration::new(2));
    presenter.complete_bridge_delivery(old_event.delivery_id, true);
    presenter.complete_retained_hydration(peer.source);
    assert!(presenter.desktop_input_target(peer.source).is_err());
    peer.channel.send_raster(0, 2, &[0; 16], false).unwrap();
    let current = presenter
        .wait_media_event(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    presenter.complete_bridge_delivery(current.delivery_id, true);
    let token = presenter.desktop_input_target(peer.source).unwrap();
    presenter
        .set_desktop_input_target(Some(token), INPUT_CLASS_KEYBOARD)
        .unwrap();
    assert_eq!(peer.lane.set_binding(&peer.binding(1)).unwrap().state, 1);
    peer.session.close().unwrap();
}

#[test]
fn desktop_disconnect_preserves_the_root_principal_and_other_owner() {
    let (presenter, endpoint) = start();
    let (first, capability) = Peer::with_capability(&presenter, &endpoint, 1, true);
    let peer = Peer::new(&presenter, &endpoint, 2, true);
    let stale = presenter.desktop_input_target(first.source).unwrap();
    let survivor = presenter.desktop_input_target(peer.source).unwrap();
    presenter
        .set_desktop_input_target(Some(stale), INPUT_CLASS_KEYBOARD)
        .unwrap();
    first.lane.set_binding(&first.binding(1)).unwrap();
    let binding = presenter.desktop_input_binding(stale).unwrap();
    presenter.disconnect_desktop_session(stale).unwrap();
    assert!(
        !presenter
            .send_desktop_input(
                stale,
                InputEvent::Key {
                    binding,
                    usage: 44,
                    pressed: true
                }
            )
            .unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !presenter.pane_media_summary(1).tracks.is_empty() {
        assert!(
            Instant::now() < deadline,
            "disconnected owner retained resources"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        presenter.desktop_input_target(peer.source).unwrap(),
        survivor
    );
    assert!(presenter.disconnect_desktop_session(stale).is_err());
    let replacement = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&capability).unwrap(),
        ..ProducerConfig::desktop()
    })
    .unwrap();
    assert_ne!(
        replacement.info().session_id,
        first.session.info().session_id
    );
    presenter
        .set_desktop_input_target(Some(survivor), INPUT_CLASS_KEYBOARD)
        .unwrap();
    assert_eq!(peer.lane.set_binding(&peer.binding(1)).unwrap().state, 1);
    replacement.close().unwrap();
    peer.session.close().unwrap();
}

#[test]
fn target_metadata_distinguishes_equal_size_resize_from_producer_acknowledgement() {
    let (presenter, endpoint) = start();
    let mut first = Peer::new(&presenter, &endpoint, 1, true);
    let peer = Peer::new(&presenter, &endpoint, 2, true);
    let unrelated = presenter.desktop_input_target(peer.source).unwrap();
    let mut changed = target();
    changed.width = 3;
    assert_eq!(presenter.update_desktop_target(1, changed, 1).unwrap(), 2);
    assert_eq!(presenter.update_desktop_target(1, target(), 1).unwrap(), 3);
    let pending = presenter.desktop_input_target(first.source).unwrap();
    assert_eq!(pending.logical_size(), (2, 2));
    assert_eq!(pending.target_generation(), 3);
    assert_eq!(pending.semantic_generation(), 1);
    assert_eq!(
        presenter.desktop_input_target(peer.source).unwrap(),
        unrelated
    );
    let mut definition = first.surface.definition().unwrap();
    let mut parameters = DesktopSurfaceParameters::decode(&definition.profile_parameters).unwrap();
    parameters.semantic_generation = 3;
    definition.profile_parameters = parameters.encode();
    first
        .session
        .update_surface(&first.surface, definition, &RequestMetadata::default())
        .unwrap();
    assert!(presenter.desktop_input_target(first.source).is_err());
    first.channel.send_raster(0, 2, &[0; 16], false).unwrap();
    let event = presenter
        .wait_media_event(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    presenter.complete_bridge_delivery(event.delivery_id, true);
    let ready = presenter.desktop_input_target(first.source).unwrap();
    assert_eq!(ready.logical_size(), (2, 2));
    assert_eq!(ready.semantic_generation(), ready.target_generation());
    assert_eq!(
        presenter.desktop_input_target(peer.source).unwrap(),
        unrelated
    );
    first.session.close().unwrap();
    peer.session.close().unwrap();
}

#[test]
fn forged_lane_and_foreign_presenter_tokens_do_not_revoke_the_active_owner() {
    use vivid_protocol::{
        auth::ChannelKey,
        messages::{Envelope, LaneOpen},
        wire::{Connection, ConnectionKind, Endpoint},
    };
    let (presenter, endpoint) = start();
    let peer = Peer::new(&presenter, &endpoint, 1, true);
    let eligible = presenter.desktop_input_target(peer.source).unwrap();
    presenter
        .set_desktop_input_target(Some(eligible), INPUT_CLASS_KEYBOARD)
        .unwrap();
    peer.lane.set_binding(&peer.binding(1)).unwrap();
    let binding = presenter.desktop_input_binding(eligible).unwrap();
    let mut forged =
        Connection::open(&Endpoint::parse(&endpoint).unwrap(), ConnectionKind::Lane).unwrap();
    let mut open = LaneOpen {
        session_id: peer.source.producer,
        lane_generation: 2,
        client_nonce: [7; 16],
        authentication_tag: [0; 16],
    };
    open.sign(&ChannelKey::new([7; 32]));
    let body = Envelope::new(1, open.payload()).encode().unwrap();
    forged
        .write_record(vivid_protocol::messages::LANE_OPEN, 0, 0, &body)
        .unwrap();
    let response = forged.read_record().unwrap();
    assert_eq!(response.record_type, vivid_protocol::messages::ERROR);
    assert_eq!(presenter.desktop_input_binding(eligible), Some(binding));
    let (other, other_endpoint) = start();
    let other_peer = Peer::new(&other, &other_endpoint, 1, true);
    let foreign = other.desktop_input_target(other_peer.source).unwrap();
    assert!(
        presenter
            .set_desktop_input_target(Some(foreign), INPUT_CLASS_KEYBOARD)
            .is_err()
    );
    assert_eq!(presenter.desktop_input_binding(eligible), Some(binding));
    peer.session.close().unwrap();
    other_peer.session.close().unwrap();
}

#[test]
fn pointer_classes_geometry_and_live_surface_invalidation_are_scoped() {
    let (presenter, endpoint) = start();
    let mut peer = Peer::new(&presenter, &endpoint, 1, true);
    let token = presenter.desktop_input_target(peer.source).unwrap();
    presenter
        .set_desktop_input_target(Some(token), INPUT_CLASS_KEYBOARD)
        .unwrap();
    let mut desired = peer.binding(1);
    desired.requested_classes = INPUT_CLASS_KNOWN_MASK;
    let status = peer.lane.set_binding(&desired).unwrap();
    assert_eq!(status.effective_classes, INPUT_CLASS_KEYBOARD);
    let binding = presenter.desktop_input_binding(token).unwrap();
    assert!(
        !presenter
            .send_desktop_input(
                token,
                InputEvent::PointerMotion {
                    binding,
                    x: 0,
                    y: 0
                }
            )
            .unwrap()
    );
    presenter
        .set_desktop_input_target(Some(token), INPUT_CLASS_KNOWN_MASK)
        .unwrap();
    assert!(matches!(peer.next(), InputLaneEvent::Revoked(_)));
    desired.producer_epoch = InputEpoch::new(2);
    peer.lane.set_binding(&desired).unwrap();
    let binding = presenter.desktop_input_binding(token).unwrap();
    let motion = InputEvent::PointerMotion {
        binding,
        x: 1 << 32,
        y: 1 << 32,
    };
    assert!(presenter.send_desktop_input(token, motion).unwrap());
    assert_eq!(peer.next().decode_input(2, 2).unwrap(), motion);
    assert!(
        presenter
            .send_desktop_input(
                token,
                InputEvent::PointerMotion {
                    binding,
                    x: 3 << 32,
                    y: 0
                }
            )
            .is_err()
    );
    assert_eq!(presenter.desktop_input_binding(token), Some(binding));
    let mut invalid = peer.surface.definition().unwrap();
    invalid.logical_width = 0;
    assert!(
        peer.session
            .update_surface(&peer.surface, invalid, &RequestMetadata::default())
            .is_err()
    );
    assert_eq!(presenter.desktop_input_binding(token), Some(binding));
    let mut changed = peer.surface.definition().unwrap();
    changed.scale_numerator = 2;
    peer.session
        .update_surface(&peer.surface, changed, &RequestMetadata::default())
        .unwrap();
    assert!(presenter.desktop_input_binding(token).is_none());
    assert!(matches!(peer.next(), InputLaneEvent::Revoked(_)));
    assert!(!presenter.send_desktop_input(token, motion).unwrap());
    peer.session.close().unwrap();
}

#[test]
fn replacing_a_visual_track_preserves_surface_bound_input() {
    let (presenter, endpoint) = start();
    let mut peer = Peer::new(&presenter, &endpoint, 1, true);
    let token = presenter.desktop_input_target(peer.source).unwrap();
    presenter
        .set_desktop_input_target(Some(token), INPUT_CLASS_KEYBOARD)
        .unwrap();
    peer.lane.set_binding(&peer.binding(1)).unwrap();
    let binding = presenter.desktop_input_binding(token).unwrap();
    let mut config = peer.track.configuration().unwrap();
    config.track_id = 2;
    let new_track = peer
        .session
        .create_track(config, &RequestMetadata::default())
        .unwrap();
    let new_channel = peer.session.open_track_channel(&new_track).unwrap();
    new_channel.send_raster(0, 1, &[0; 16], false).unwrap();
    peer.session
        .wait_track(
            &new_track,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_OUTPUT_READY),
            5_000_000,
        )
        .unwrap();
    peer.session
        .activate_tracks(
            &peer.surface,
            &[SlotBinding {
                slot: 1,
                track_id: 2,
                expected_channel_generation: new_track.channel_generation(),
                required_milestone: MILESTONE_OUTPUT_READY,
            }],
            &RequestMetadata::default(),
        )
        .unwrap();
    peer.session
        .destroy_track(&peer.track, &RequestMetadata::default())
        .unwrap();
    assert_eq!(presenter.desktop_input_binding(token), Some(binding));
    let event = InputEvent::Key {
        binding,
        usage: 0x06,
        pressed: true,
    };
    assert!(presenter.send_desktop_input(token, event).unwrap());
    assert_eq!(peer.next().decode_input(2, 2).unwrap(), event);
    peer.session.close().unwrap();
}

#[test]
fn input_rate_exhaustion_retires_only_that_owners_lane() {
    use vivid_protocol::resource::Resource;
    let (seed, seed_endpoint) = start();
    let seed_peer = Peer::new(&seed, &seed_endpoint, 1, true);
    let mut contract = seed_peer.session.info().resource_contract.clone();
    seed_peer.session.close().unwrap();
    drop(seed);
    contract.set(Resource::InputEventsPerSecond, 4);
    let listener = SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start_configured(
        listener,
        PresenterConfig::desktop_with_input(MediaConfig::default(), target())
            .with_resource_contract(contract),
        None,
    )
    .unwrap();
    let first = Peer::new(&presenter, &endpoint, 1, true);
    let second = Peer::new(&presenter, &endpoint, 2, true);
    let a = presenter.desktop_input_target(first.source).unwrap();
    let b = presenter.desktop_input_target(second.source).unwrap();
    presenter
        .set_desktop_input_target(Some(a), INPUT_CLASS_KEYBOARD)
        .unwrap();
    first.lane.set_binding(&first.binding(1)).unwrap();
    let binding = presenter.desktop_input_binding(a).unwrap();
    for pressed in [true, false, true, false] {
        let event = InputEvent::Key {
            binding,
            usage: 0x04,
            pressed,
        };
        assert!(presenter.send_desktop_input(a, event).unwrap());
        assert_eq!(first.next().decode_input(2, 2).unwrap(), event);
    }
    assert!(
        presenter
            .send_desktop_input(
                a,
                InputEvent::Key {
                    binding,
                    usage: 0x04,
                    pressed: true
                }
            )
            .is_err()
    );
    assert!(presenter.desktop_input_binding(a).is_none());
    presenter
        .set_desktop_input_target(Some(b), INPUT_CLASS_KEYBOARD)
        .unwrap();
    second.lane.set_binding(&second.binding(1)).unwrap();
    let binding = presenter.desktop_input_binding(b).unwrap();
    let event = InputEvent::Key {
        binding,
        usage: 0x05,
        pressed: true,
    };
    assert!(presenter.send_desktop_input(b, event).unwrap());
    assert_eq!(second.next().decode_input(2, 2).unwrap(), event);
    first.session.close().unwrap();
    second.session.close().unwrap();
}
