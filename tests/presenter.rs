//! The shared test presenter, driven by the real SDK producer.
//!
//! These are the tests that keep the harness honest: every assertion here is about what a producer
//! and presenter actually exchange, so a presenter that drifts from the SDK's expectations fails
//! here rather than silently making some other crate's suite pass.

#![cfg(feature = "testing")]

use std::io;
use std::sync::Arc;
use std::time::Duration;

use vivid_protocol::messages::LaneClass;
use vivid_protocol::revision::{InputEpoch, SurfaceGeneration};
use vivid_protocol::surface::CoordinateModel;
use vivid_protocol::wire::{Connection, Endpoint};
use vivid_sdk::testing::{Fault, ROOT_SECRET_HEX, Script, TargetKind, TestPresenter};
use vivid_sdk::{
    CORE_CONTROL, CleanupPolicy, ConnectionFactory, ConnectionKind, DESKTOP_CONTENT, DESKTOP_INPUT,
    DESKTOP_SURFACE, DesktopSurfaceParameters, INPUT_CLASS_KEYBOARD, INPUT_CLASS_POINTER_AXIS,
    INPUT_CLASS_POINTER_MOTION, InputBinding, LIVE_MEDIA, OBSERVABILITY, OutputDescriptor,
    ProducerAuthentication, ProducerConfig, RequestMetadata, Rotation, Session,
    SessionLeaseDefinition, SurfaceDefinition, SurfaceDescriptor, SurfaceRole, input_capability,
};

struct BoundTestFactory {
    endpoint: Endpoint,
    binding: [u8; 32],
}

impl ConnectionFactory for BoundTestFactory {
    fn open(&self, kind: ConnectionKind, _lane: Option<LaneClass>) -> io::Result<Connection> {
        Connection::open(&self.endpoint, kind)
    }

    fn carrier_binding_key(&self) -> [u8; 32] {
        self.binding
    }
}

fn desktop_session(presenter: &TestPresenter) -> Session {
    Session::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint().to_owned()),
        authentication: ProducerAuthentication::root_hex(ROOT_SECRET_HEX).unwrap(),
        ..ProducerConfig::desktop()
    })
    .expect("a desktop producer connects to a desktop presenter")
}

fn desktop_surface(context_id: u64, surface_id: u64) -> SurfaceDefinition {
    SurfaceDefinition {
        context_id,
        surface_id,
        semantic_profile: DESKTOP_CONTENT.into(),
        coordinate_model: CoordinateModel::DesktopLogicalPixels,
        logical_width: 1920,
        logical_height: 1080,
        scale_numerator: 1,
        scale_denominator: 1,
        rotation: 0,
        descriptor: SurfaceDescriptor {
            role: SurfaceRole::Desktop,
            title: "desktop".into(),
            semantic_content_revision: 1,
            semantic_availability: 0,
            locator_hint: String::new(),
        },
        policy: 0,
        profile_parameters: DesktopSurfaceParameters {
            captured_origin_x: 0,
            captured_origin_y: 0,
            topology: vec![OutputDescriptor {
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
            semantic_generation: 1,
            input_capabilities: input_capability::KNOWN_MASK,
        }
        .encode(),
    }
}

#[test]
fn a_desktop_producer_negotiates_a_desktop_target() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let session = desktop_session(&presenter);
    let info = session.info();
    assert_eq!(info.target_profile, DESKTOP_SURFACE);
    let target = info
        .desktop_target()
        .expect("a desktop session reports a parsed desktop target");
    assert_eq!((target.width, target.height), (1920, 1080));
    assert_eq!(target.outputs.len(), 1);
    assert!(info.target_settled().unwrap());
    for profile in [CORE_CONTROL, DESKTOP_SURFACE, LIVE_MEDIA] {
        assert!(session.supports(profile), "{profile} was not accepted");
    }
    assert!(session.supports(DESKTOP_INPUT));
    assert!(session.supports(OBSERVABILITY));
    session.close().unwrap();
}

#[test]
fn a_terminal_producer_against_a_desktop_presenter_is_refused() {
    // A profile mismatch is a test-setup error. Answering it with the wrong descriptor shape would
    // hide the mistake until some much later assertion failed for an unrelated-looking reason.
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let result = Session::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint().to_owned()),
        authentication: ProducerAuthentication::root_hex(ROOT_SECRET_HEX).unwrap(),
        ..ProducerConfig::default()
    });
    assert!(result.is_err(), "a terminal producer must not be welcomed");
}

#[test]
fn welcome_confirmation_rejects_a_carrier_binding_disagreement() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let factory = Arc::new(BoundTestFactory {
        endpoint: Endpoint::parse(presenter.endpoint()).unwrap(),
        binding: [0x5a; 32],
    });
    let result = Session::connect_with_factory(
        ProducerConfig {
            authentication: ProducerAuthentication::root_hex(ROOT_SECRET_HEX).unwrap(),
            ..ProducerConfig::desktop()
        },
        factory,
    );
    assert!(
        result.is_err(),
        "zero/exporter disagreement must fail WELCOME authentication"
    );
}

#[test]
fn the_presenter_serves_contexts_and_bounded_session_leases() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let mut session = desktop_session(&presenter);

    let context = session
        .create_context(
            &vivid_sdk::ContextDefinition {
                context_id: 2,
                parent_context_id: session.info().root_context_id,
                operation_classes: vivid_sdk::OP_SURFACE_TRACK_MEDIA | vivid_sdk::OP_SCENE,
                label: "worker".into(),
                lifetime_us: 60_000_000,
                requested_contract: session.info().resource_contract.clone(),
            },
            &RequestMetadata::default(),
        )
        .expect("the presenter answers CREATE_CONTEXT");
    assert_eq!(context.context_id, 2);
    assert_ne!(context.revision, 0);

    let definition = SessionLeaseDefinition {
        context_id: 2,
        lease_id: 3,
        activation_verifier: [9; 32],
        activation_timeout_us: 20_000_000,
        requested_disconnect_grace_us: 5_000_000,
        cleanup_policy: CleanupPolicy::SuspendOnUncleanLoss,
        permitted_profiles: {
            let mut profiles = vec![
                CORE_CONTROL.to_owned(),
                DESKTOP_SURFACE.to_owned(),
                LIVE_MEDIA.to_owned(),
            ];
            profiles.sort();
            profiles
        },
        requested_contract: session.info().resource_contract.clone(),
        client_public_key: None,
    };
    let lease = session
        .create_session_lease(&definition, &RequestMetadata::default())
        .expect("the presenter answers CREATE_SESSION_LEASE");
    assert_eq!(lease.lease_id, 3);
    assert_eq!(lease.state, 1, "a fresh lease is ISSUED");
    assert!(lease.activation_timeout_us <= definition.activation_timeout_us);
    assert!(lease.disconnect_grace_us <= definition.requested_disconnect_grace_us);
    assert_eq!(
        lease.cleanup_policy,
        CleanupPolicy::SuspendOnUncleanLoss as u64
    );
    session.close().unwrap();
}

#[test]
fn the_interactive_lane_grants_and_narrows_input() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    // Desktop §5.2: a presenter narrows the requested classes and never broadens them. Local
    // policy refusing the axis class is exactly what the script models.
    presenter
        .script()
        .deny_input_classes(INPUT_CLASS_POINTER_AXIS);
    let mut session = desktop_session(&presenter);
    session
        .create_surface(desktop_surface(1, 1), &RequestMetadata::default())
        .unwrap();

    let lane = session.open_input_lane(1).expect("the lane is accepted");
    assert_eq!(lane.generation(), 1);
    assert_eq!(presenter.lanes(), vec![1]);

    let status = lane
        .set_binding(&InputBinding {
            producer_epoch: InputEpoch::new(1),
            context_id: 1,
            surface_id: 1,
            surface_generation: SurfaceGeneration::new(1),
            requested_classes: INPUT_CLASS_KEYBOARD
                | INPUT_CLASS_POINTER_MOTION
                | INPUT_CLASS_POINTER_AXIS,
            reason: 0,
            requested_watchdog_us: 2_000_000,
        })
        .expect("the presenter answers SET_INPUT_BINDING");

    assert_eq!(status.state, 1, "the grant is enabled");
    assert_eq!(
        status.effective_classes,
        INPUT_CLASS_KEYBOARD | INPUT_CLASS_POINTER_MOTION,
        "the denied class is narrowed away, not granted"
    );
    assert_eq!(
        status.effective_classes & INPUT_CLASS_POINTER_AXIS,
        0,
        "a denied class must never appear in an effective grant"
    );
    assert_ne!(status.grant_generation, 0);

    let observed = presenter.input_bindings();
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0].requested_classes & INPUT_CLASS_POINTER_AXIS,
        INPUT_CLASS_POINTER_AXIS
    );
    assert_eq!(observed[0].effective_classes & INPUT_CLASS_POINTER_AXIS, 0);
    session.close().unwrap();
}

#[test]
fn a_disabling_binding_reports_a_disabled_grant() {
    let presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let mut session = desktop_session(&presenter);
    session
        .create_surface(desktop_surface(1, 1), &RequestMetadata::default())
        .unwrap();
    let lane = session.open_input_lane(1).unwrap();
    lane.set_binding(&InputBinding {
        producer_epoch: InputEpoch::new(1),
        context_id: 1,
        surface_id: 1,
        surface_generation: SurfaceGeneration::new(1),
        requested_classes: INPUT_CLASS_KEYBOARD,
        reason: 0,
        requested_watchdog_us: 2_000_000,
    })
    .unwrap();

    // A disable names no surface and carries no watchdog; the presenter reports state 0 and the
    // producer must not read a grant out of it.
    let status = lane
        .set_binding(&InputBinding {
            producer_epoch: InputEpoch::new(2),
            context_id: 0,
            surface_id: 0,
            surface_generation: SurfaceGeneration::ZERO,
            requested_classes: 0,
            reason: 1,
            requested_watchdog_us: 0,
        })
        .expect("a disable is answered");
    assert_eq!(status.state, 0);
    assert_eq!(status.effective_classes, 0);
    assert_eq!(presenter.input_bindings().len(), 2);
    session.close().unwrap();
}

#[test]
fn a_dropped_reply_leaves_the_request_serviced_and_unanswered() {
    // Security §6.4's retry model rests on this: a lost reply is a lost *reply*. The presenter
    // has already done the work, so a producer that retries must not cause it to be done twice.
    let script = Script::new();
    script.arm(Fault::DropReply {
        record_type: vivid_protocol::registry::record::SURFACE_READY,
        times: 1,
    });
    let presenter = TestPresenter::start_with(
        TargetKind::Desktop {
            width: 1920,
            height: 1080,
        },
        script,
    )
    .unwrap();
    let mut session = desktop_session(&presenter);

    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = session.create_surface(desktop_surface(1, 1), &RequestMetadata::default());
        let _ = send.send(result.is_ok());
    });
    // The call blocks forever waiting for the swallowed reply, which is the point: assert the
    // presenter recorded the request anyway rather than that the producer returned.
    assert!(
        receive.recv_timeout(Duration::from_millis(300)).is_err(),
        "a dropped reply must leave the producer waiting, not error"
    );
    assert!(
        presenter
            .observed()
            .iter()
            .any(|record| record.record_type == vivid_protocol::registry::record::CREATE_SURFACE),
        "the request was serviced even though its reply was dropped"
    );
}

#[test]
fn a_delayed_reply_still_arrives() {
    let script = Script::new();
    script.delay(
        vivid_protocol::registry::record::SURFACE_READY,
        Duration::from_millis(150),
    );
    let presenter = TestPresenter::start_with(
        TargetKind::Desktop {
            width: 1920,
            height: 1080,
        },
        script,
    )
    .unwrap();
    let mut session = desktop_session(&presenter);
    let started = std::time::Instant::now();
    session
        .create_surface(desktop_surface(1, 1), &RequestMetadata::default())
        .expect("a delayed reply is still a reply");
    assert!(
        started.elapsed() >= Duration::from_millis(140),
        "the delay was actually applied"
    );
    session.close().unwrap();
}

#[test]
fn two_owners_reusing_the_same_local_ids_stay_isolated() {
    // The root AGENTS.md rule: two owners deliberately reuse every numeric ID, and one owner's
    // teardown must leave the other byte-for-byte intact.
    let first_presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let second_presenter = TestPresenter::start_desktop(1920, 1080).unwrap();
    let mut first = desktop_session(&first_presenter);
    let mut second = desktop_session(&second_presenter);

    // Identical context, surface, and lane numbers in both sessions.
    let first_surface = first
        .create_surface(desktop_surface(1, 1), &RequestMetadata::default())
        .unwrap();
    let second_surface = second
        .create_surface(desktop_surface(1, 1), &RequestMetadata::default())
        .unwrap();
    assert_eq!(first_surface.id(), second_surface.id());
    assert_eq!(first_surface.context_id(), second_surface.context_id());

    let first_lane = first.open_input_lane(1).unwrap();
    let second_lane = second.open_input_lane(1).unwrap();
    let binding = InputBinding {
        producer_epoch: InputEpoch::new(1),
        context_id: 1,
        surface_id: 1,
        surface_generation: SurfaceGeneration::new(1),
        requested_classes: INPUT_CLASS_KEYBOARD,
        reason: 0,
        requested_watchdog_us: 2_000_000,
    };
    first_lane.set_binding(&binding).unwrap();
    second_lane.set_binding(&binding).unwrap();
    assert_eq!(first_presenter.input_bindings().len(), 1);
    assert_eq!(second_presenter.input_bindings().len(), 1);

    // Tear the first owner down completely.
    first.close().unwrap();

    // The second owner's surface, lane, and next valid update are all untouched.
    assert_eq!(second_surface.id(), 1);
    assert_eq!(second_surface.generation(), SurfaceGeneration::new(1));
    let status = second_lane
        .set_binding(&InputBinding {
            producer_epoch: InputEpoch::new(2),
            ..binding
        })
        .expect("the surviving owner's next binding still works");
    assert_eq!(status.state, 1);
    assert_eq!(second_presenter.input_bindings().len(), 2);
    assert_eq!(
        first_presenter.input_bindings().len(),
        1,
        "the torn-down owner's log did not gain entries from the survivor"
    );
    second.close().unwrap();
}
