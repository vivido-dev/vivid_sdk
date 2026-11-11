//! Real-session recovery with overlapping local IDs and interrupted operations.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{
    CleanupPolicy, EstablishmentAttempt, ProducerAuthentication, SessionLeaseBuilder,
    presenter::{
        MediaConfig, PresenterConfig, PresenterListener as _, SocketListener, VirtualVivid,
    },
};
use vivid_protocol::{
    cbor::Value,
    messages::{self, LaneClass},
    resource::{RESOURCE_COUNT, ResourceContract},
    scene::{Fit, SceneNode},
    surface::{CoordinateModel, DesktopSurfaceParameters, SurfaceDescriptor, SurfaceRole},
    target::DesktopTarget,
    track::{MILESTONE_OUTPUT_READY, RasterConfiguration, TrackMode},
};

fn presenter() -> (VirtualVivid, String) {
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

fn config(endpoint: &str, authentication: ProducerAuthentication) -> ProducerConfig {
    ProducerConfig {
        endpoint_control: Some(endpoint.into()),
        authentication,
        ..ProducerConfig::desktop()
    }
}

fn leased(presenter: &VirtualVivid, endpoint: &str, pane: u64) -> Session {
    let (definition, mut activation) = SessionLeaseBuilder::new(1, pane)
        .permitted_profiles(vec![
            crate::CORE_CONTROL.into(),
            crate::DESKTOP_SURFACE.into(),
            crate::LIVE_MEDIA.into(),
            crate::DESKTOP_INPUT.into(),
            crate::OBSERVABILITY.into(),
        ])
        .contract(ResourceContract::new([u64::MAX / 4; RESOURCE_COUNT]))
        .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss)
        .disconnect_grace_us(5_000_000)
        .build()
        .unwrap();
    presenter.issue_lease(pane, definition).unwrap();
    Session::connect(config(
        endpoint,
        ProducerAuthentication::lease_activation_bytes(1, pane, activation.take().unwrap())
            .unwrap(),
    ))
    .unwrap()
}

fn resume(endpoint: &str, session: Session, graph: &mut Graph) -> Session {
    graph.remember_allocator(&session);
    let authentication = session.resume_authentication().unwrap();
    drop(session);
    let mut attempt =
        EstablishmentAttempt::new(config(endpoint, authentication), Duration::from_secs(3))
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match attempt.connect() {
            Ok(session) => return session,
            Err(error) => {
                assert!(Instant::now() < deadline, "resume failed: {error}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn populate(graph: &mut Graph, session: &mut Session) -> (Surface, Track) {
    let context = session.info().root_context_id;
    let surface = graph
        .surface(
            session,
            SurfaceDefinition {
                context_id: context,
                surface_id: 1,
                semantic_profile: crate::DESKTOP_CONTENT.into(),
                coordinate_model: CoordinateModel::DesktopLogicalPixels,
                logical_width: 2,
                logical_height: 2,
                scale_numerator: 1,
                scale_denominator: 1,
                rotation: 0,
                descriptor: SurfaceDescriptor {
                    role: SurfaceRole::ApplicationCanvas,
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
                    input_capabilities: crate::INPUT_CLASS_KNOWN_MASK,
                }
                .encode(),
            },
        )
        .unwrap();
    graph
        .node(
            session,
            &SceneNode {
                owning_context_id: context,
                node_id: 1,
                surface_context_id: context,
                surface_id: 1,
                geometry: vivid_protocol::geometry::NodeGeometry::full_target().encode(),
                fit: Fit::Contain,
                linear_sampling: false,
                z_index: 0,
                visible: true,
                opacity: u16::MAX,
                clip: None,
            },
        )
        .unwrap();
    let track = graph
        .create_track(
            session,
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
        )
        .unwrap();
    (surface, track)
}

#[test]
fn repeated_recovery_reuses_objects_and_invalidates_input_targets() {
    let (presenter, endpoint) = presenter();
    let mut first = leased(&presenter, &endpoint, 1);
    let mut second = leased(&presenter, &endpoint, 2);
    let mut graph = Graph::default();
    let mut peer_graph = Graph::default();
    let (surface, mut track) = populate(&mut graph, &mut first);
    populate(&mut peer_graph, &mut second);
    let peer_before = snapshot(&second).unwrap();
    let owner = first.info().session_id;
    let mut previous = None;
    for generation in 0..=3 {
        assert_eq!(first.info().resume_generation, generation);
        let channel = first.open_track_channel(&track).unwrap();
        channel
            .send_raster(0, generation + 1, &[255; 16], false)
            .unwrap();
        first
            .wait_track(
                &track,
                crate::TrackWaitCondition::MilestoneSet,
                Some(MILESTONE_OUTPUT_READY),
                3_000_000,
            )
            .unwrap();
        first
            .activate_tracks(
                &surface,
                &[crate::SlotBinding {
                    slot: 1,
                    track_id: track.id(),
                    expected_channel_generation: track.channel_generation(),
                    required_milestone: MILESTONE_OUTPUT_READY,
                }],
                &RequestMetadata::default(),
            )
            .unwrap();
        graph.activated(track.id());
        let delivery = presenter
            .wait_media_event(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        presenter.complete_bridge_delivery(delivery.delivery_id, true);
        let source = crate::presenter::SourceKey {
            producer: owner,
            context: 1,
            surface: 1,
            track: track.id(),
        };
        let target = presenter.desktop_input_target(source).unwrap();
        assert_eq!(target.resume_generation(), generation);
        if let Some(stale) = previous {
            assert!(
                presenter
                    .set_desktop_input_target(Some(stale), crate::INPUT_CLASS_KEYBOARD)
                    .is_err()
            );
        }
        presenter
            .set_desktop_input_target(Some(target), crate::INPUT_CLASS_KEYBOARD)
            .unwrap();
        let lane = first.open_input_lane(generation + 1).unwrap();
        let binding = crate::InputBinding {
            producer_epoch: vivid_protocol::revision::InputEpoch::new(generation * 2 + 1),
            context_id: 1,
            surface_id: 1,
            surface_generation: surface.generation(),
            requested_classes: crate::INPUT_CLASS_KEYBOARD,
            reason: 6,
            requested_watchdog_us: 250_000,
        };
        if generation == 0 {
            assert_eq!(lane.set_binding(&binding).unwrap().effective_classes, 0);
            presenter
                .set_lease_desktop_input_classes(1, 1, crate::INPUT_CLASS_KEYBOARD)
                .unwrap();
        }
        let binding = crate::InputBinding {
            producer_epoch: vivid_protocol::revision::InputEpoch::new(generation * 2 + 2),
            ..binding
        };
        assert_eq!(
            lane.set_binding(&binding).unwrap().effective_classes,
            crate::INPUT_CLASS_KEYBOARD
        );
        assert_eq!(
            presenter.desktop_session_generation(source),
            Some(generation)
        );
        previous = Some(target);
        if generation == 3 {
            break;
        }
        drop(lane);
        drop(channel);
        first = resume(&endpoint, first, &mut graph);
        graph.reconcile(&mut first).unwrap();
        assert_eq!(first.info().session_id, owner);
        // The last displayed track can retire before its replacement is
        // published. Supervision must still recognize the live canvas owner.
        assert_eq!(
            presenter.desktop_session_generation(source),
            Some(generation + 1)
        );
        assert!(snapshot(&first).unwrap().input_disabled);
        assert_eq!(snapshot(&second).unwrap(), peer_before);
        track = if let Some(reused) = graph.reusable(&mut first, (2, 2)).unwrap() {
            reused
        } else {
            let mut configuration = track.configuration().unwrap();
            configuration.track_id = first.allocate_id().unwrap();
            graph.create_track(&mut first, configuration).unwrap()
        };
    }
    assert_eq!(snapshot(&first).unwrap().usage, [1, 1, 1]);
    presenter.update_desktop_target(1, target(), 1).unwrap();
    assert_eq!(snapshot(&second).unwrap(), peer_before);
    assert!(presenter.update_desktop_target(3, target(), 1).is_err());
}

#[test]
fn uncertain_create_receipts_and_surface_updates_are_reconciled() {
    let (presenter, endpoint) = presenter();
    let mut session = leased(&presenter, &endpoint, 1);
    let mut graph = Graph::default();
    let (surface, _) = populate(&mut graph, &mut session);
    let original = surface.definition().unwrap();
    let mut changed = original.clone();
    changed.logical_width = 3;
    changed.profile_parameters = DesktopSurfaceParameters {
        captured_origin_x: 0,
        captured_origin_y: 0,
        topology: vec![],
        semantic_generation: 2,
        input_capabilities: crate::INPUT_CLASS_KEYBOARD,
    }
    .encode();
    session
        .update_surface(&surface, changed.clone(), &RequestMetadata::default())
        .unwrap();
    // Model a reply lost after the presenter committed the update.
    surface.inner.lock().unwrap().definition = original;
    graph.surface.as_mut().unwrap().handle = None;
    graph.tracks[0].handle = None;
    graph.node = false;
    session = resume(&endpoint, session, &mut graph);
    graph.reconcile(&mut session).unwrap();
    assert!(graph.node);
    assert_eq!(
        graph
            .surface
            .as_ref()
            .unwrap()
            .handle
            .as_ref()
            .unwrap()
            .definition()
            .unwrap(),
        changed
    );
    assert_eq!(snapshot(&session).unwrap().usage, [1, 1, 1]);
    assert!(session.allocate_id().unwrap() > graph.highwater);
}

#[test]
fn suspension_discards_only_its_owners_pending_transactions() {
    let (presenter, endpoint) = presenter();
    let mut first = leased(&presenter, &endpoint, 1);
    let mut second = leased(&presenter, &endpoint, 2);
    populate(&mut Graph::default(), &mut first);
    populate(&mut Graph::default(), &mut second);
    let begin = |session: &Session| {
        session.request(
            messages::BEGIN_TXN,
            1,
            vec![(0, Value::Unsigned(1)), (1, Value::Unsigned(1))],
            &RequestMetadata::default(),
            Some(1),
            None,
        )
    };
    begin(&first).unwrap();
    begin(&second).unwrap();
    for session in [&first, &second] {
        session
            .request(
                messages::DELETE_NODE,
                1,
                vec![(0, Value::Unsigned(1)), (1, Value::Unsigned(1))],
                &RequestMetadata::default(),
                Some(1),
                None,
            )
            .unwrap();
    }
    let mut graph = Graph::default();
    let first = resume(&endpoint, first, &mut graph);
    begin(&first).unwrap();
    second
        .request(
            messages::COMMIT_TXN,
            1,
            vec![(0, Value::Unsigned(0))],
            &RequestMetadata {
                preconditions: vec![(0, Value::Unsigned(second.info().scene_revision.get()))],
                ..Default::default()
            },
            Some(1),
            Some(second.info().target_generation.get()),
        )
        .unwrap();
    assert_eq!(snapshot(&second).unwrap().usage, [1, 1, 0]);
    assert_eq!(snapshot(&first).unwrap().usage, [1, 1, 1]);
}

#[test]
fn interrupted_retirement_recovers_both_possible_outcomes_without_touching_peer() {
    for applied in [false, true] {
        let (presenter, endpoint) = presenter();
        let mut first = leased(&presenter, &endpoint, 1);
        let mut second = leased(&presenter, &endpoint, 2);
        let mut graph = Graph::default();
        let (_, track) = populate(&mut graph, &mut first);
        let source = crate::presenter::SourceKey {
            producer: first.info().session_id,
            context: 1,
            surface: 1,
            track: track.id(),
        };
        populate(&mut Graph::default(), &mut second);
        let peer_before = snapshot(&second).unwrap();
        // The destroy was planned before transport loss. Its reply is unknown.
        graph.tracks[0].retiring = true;
        if applied {
            first
                .destroy_track(&track, &RequestMetadata::default())
                .unwrap();
            assert_eq!(presenter.desktop_session_generation(source), Some(0));
        }
        first = resume(&endpoint, first, &mut graph);
        graph.reconcile(&mut first).unwrap();
        assert!(graph.tracks.is_empty());
        assert_eq!(presenter.desktop_session_generation(source), Some(1));
        assert_eq!(snapshot(&first).unwrap().usage, [1, 0, 1]);
        assert_eq!(snapshot(&second).unwrap(), peer_before);
    }
}

#[test]
fn recovery_rejects_graphs_outside_its_closed_world_before_cleanup() {
    let (presenter, endpoint) = presenter();
    let mut session = leased(&presenter, &endpoint, 1);
    let mut graph = Graph::default();
    let (surface, _) = populate(&mut graph, &mut session);
    let mut extra = surface.definition().unwrap();
    extra.surface_id = 2;
    session
        .create_surface(extra, &RequestMetadata::default())
        .unwrap();
    session = resume(&endpoint, session, &mut graph);
    assert_eq!(
        graph.reconcile(&mut session).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(snapshot(&session).unwrap().usage, [2, 1, 1]);
}
