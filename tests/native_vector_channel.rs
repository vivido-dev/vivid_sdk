//! Real channel retirement and lease recovery with reused vector identities.

#![cfg(all(feature = "presenter", feature = "vector-render"))]

// Rust guideline compliant 2026-10-07
use std::{
    io::{self, Read},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use vivid_protocol::{
    overlay::wire::{SetWindow, WindowAddress},
    overlay::{WindowMode, WindowOptions},
    vector::{Brush, Canvas, Color, Frame, Path, Rect},
};
use vivid_sdk::presenter::{
    ConnectionCancel, MediaConfig, NativeVectorHost, PresenterConfig, PresenterListener, SourceKey,
    Transport, VectorFrame, VirtualVivid,
};
use vivid_sdk::*;

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool, usize)>,
    changed: Condvar,
}
impl Gate {
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut state = self.state.lock().unwrap();
        while state.2 == 0 {
            assert!(Instant::now() < deadline);
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(10))
                .unwrap()
                .0;
        }
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct DelayedRead {
    stream: TcpStream,
    gate: Arc<Gate>,
}
impl Read for DelayedRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.stream.read(bytes);
        if matches!(result, Ok(0))
            || result.as_ref().is_err_and(|error| {
                matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::NotConnected
                )
            })
        {
            let mut state = self.gate.state.lock().unwrap();
            if state.0 && !state.1 {
                state.2 += 1;
                self.gate.changed.notify_all();
                let deadline = Instant::now() + Duration::from_secs(5);
                while !state.1 && Instant::now() < deadline {
                    state = self
                        .gate
                        .changed
                        .wait_timeout(state, Duration::from_millis(10))
                        .unwrap()
                        .0;
                }
            }
        }
        result
    }
}
struct Listener {
    tcp: TcpListener,
    gate: Arc<Gate>,
}
impl PresenterListener for Listener {
    fn endpoint(&self) -> String {
        format!("tcp:{}", self.tcp.local_addr().unwrap())
    }
    fn accept(&self) -> io::Result<Transport> {
        let (stream, _) = self.tcp.accept()?;
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        let reader = DelayedRead {
            stream: stream.try_clone()?,
            gate: self.gate.clone(),
        };
        let writer = stream.try_clone()?;
        let cancel = stream.try_clone()?;
        Ok(Transport::new(
            Box::new(reader),
            Box::new(writer),
            ConnectionCancel::new(move || {
                let _ = cancel.shutdown(Shutdown::Both);
            }),
            Arc::new(move |timeout| stream.set_read_timeout(timeout)),
        ))
    }
}
fn config(presenter: &VirtualVivid, authentication: ProducerAuthentication) -> ProducerConfig {
    let mut profiles =
        PresenterConfig::terminal_with_overlay(MediaConfig::default()).supported_profiles;
    // The native host supplies baseline timed playback, not synchronized starts.
    profiles.retain(|profile| profile != vivid_protocol::registry::TIMED_MEDIA_SYNC);
    profiles.sort();
    profiles.dedup();
    ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        endpoint_interactive: Some(presenter.endpoint()),
        endpoint_realtime: Some(presenter.endpoint()),
        endpoint_bulk: Some(presenter.endpoint()),
        authentication,
        required_profiles: profiles,
        optional_profiles: Vec::new(),
        ..ProducerConfig::default()
    }
}
fn connect(presenter: &VirtualVivid) -> Session {
    Session::connect(config(
        presenter,
        ProducerAuthentication::root_hex(&presenter.issue_pane_capability(1).unwrap()).unwrap(),
    ))
    .unwrap()
}
fn track_config(surface: &Surface) -> TrackConfiguration {
    TrackConfiguration {
        direction: Default::default(),
        context_id: surface.context_id(),
        surface_id: surface.id(),
        track_id: 2,
        slot: SLOT_VECTOR,
        mode: TrackMode::Live,
        lane: LaneClass::Bulk,
        maximum_record_body: 65536,
        maximum_rate_millihertz: 60_000,
        maximum_encoded_bits_per_second: 65536 * 8 * 60,
        maximum_records_per_second: 60,
        maximum_inflight_body_bytes: 65536,
        target_latency_us: 0,
        maximum_latency_us: 0,
        retained_pixel_charge: 48 * 48,
        kind: KindConfiguration::VectorScene(VectorConfiguration {
            width: 48,
            height: 48,
            maximum_scene_bytes: 65524,
        }),
    }
}
fn window(session: &mut Session, x: f64) -> (Surface, Track, TrackChannel) {
    window_mode(session, x, WindowMode::Floating)
}
fn window_mode(session: &mut Session, x: f64, mode: WindowMode) -> (Surface, Track, TrackChannel) {
    let definition = SurfaceBuilder::new(session, 48, 48)
        .unwrap()
        .titled(SurfaceRole::Figure, "native channel test")
        .build()
        .unwrap();
    let surface = session
        .create_surface(definition, &RequestMetadata::default())
        .unwrap();
    let track = session
        .create_track(track_config(&surface), &RequestMetadata::default())
        .unwrap();
    let channel = session.open_track_channel(&track).unwrap();
    session
        .set_overlay_window(&SetWindow {
            address: WindowAddress {
                context_id: surface.context_id(),
                surface_id: surface.id(),
                generation: surface.generation().get(),
            },
            expected_revision: 0,
            options: WindowOptions::new(Rect::new(x, 0., 48., 48.).unwrap(), mode),
        })
        .unwrap();
    (surface, track, channel)
}
fn submit(
    session: &mut Session,
    surface: &Surface,
    track: &Track,
    channel: &TrackChannel,
    revision: u64,
    color: u32,
) {
    let mut canvas = Canvas::new();
    canvas
        .fill(
            Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
            Brush::Solid(Color(color)),
        )
        .unwrap();
    canvas
        .push(vivid_protocol::vector::Command::Hit {
            id: 7,
            path: Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
            role: vivid_protocol::vector::HitRole::Input,
            cursor: None,
        })
        .unwrap();
    channel
        .send_vector(&Frame {
            epoch: 1,
            revision,
            canvas,
        })
        .unwrap();
    session
        .wait_track(
            track,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_OUTPUT_READY),
            1_000_000,
        )
        .unwrap();
    session.query_surface(surface).unwrap();
    session
        .activate_tracks(
            surface,
            &[SlotBinding {
                slot: SLOT_VECTOR,
                track_id: track.id(),
                expected_channel_generation: track.channel_generation(),
                required_milestone: MILESTONE_OUTPUT_READY,
            }],
            &RequestMetadata::default(),
        )
        .unwrap();
}
fn prepared(host: &mut NativeVectorHost, count: usize) -> VectorFrame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = host.prepare(1, 1.).unwrap();
        if frame.drawings().len() == count {
            return frame;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_delayed_old_channel_exit_cannot_detach_a_recreated_track_or_its_peer() {
    let gate = Arc::new(Gate::default());
    let _release = Release(gate.clone());
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            Listener {
                tcp,
                gate: gate.clone(),
            },
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    let mut first = connect(&presenter);
    let mut peer = connect(&presenter);
    let (a, old, old_channel) = window(&mut first, 0.);
    let (b, peer_track, peer_channel) = window(&mut peer, 64.);
    assert_eq!(
        (a.context_id(), a.id(), old.id()),
        (b.context_id(), b.id(), peer_track.id())
    );
    submit(&mut first, &a, &old, &old_channel, 1, 0xff0000ff);
    submit(&mut peer, &b, &peer_track, &peer_channel, 1, 0xffffffff);
    let frame = prepared(&mut host, 2);
    assert!(host.presented(&frame).unwrap());
    drop(frame);
    gate.state.lock().unwrap().0 = true;
    first
        .destroy_track(&old, &RequestMetadata::default())
        .unwrap();
    gate.wait();
    let replacement = first
        .create_track(track_config(&a), &RequestMetadata::default())
        .unwrap();
    let channel = first.open_track_channel(&replacement).unwrap();
    assert_eq!(
        old.channel_generation(),
        replacement.channel_generation(),
        "the retired incarnation deliberately reuses generation one"
    );
    submit(&mut first, &a, &replacement, &channel, 2, 0x0000ffff);
    assert_eq!(
        presenter.active_connection_count(),
        5,
        "the old channel must still be held before cleanup"
    );
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(5);
    while presenter.active_connection_count() != 4 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(first.query_track(&replacement).unwrap().attachment_state, 1);
    assert_eq!(peer.query_track(&peer_track).unwrap().attachment_state, 1);
    submit(&mut first, &a, &replacement, &channel, 3, 0x00ff00ff);
    submit(&mut peer, &b, &peer_track, &peer_channel, 2, 0xffffffff);
    let frame = prepared(&mut host, 2);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(
        presenter
            .projection_snapshot(&std::collections::HashSet::from([1]))
            .sources
            .len(),
        2
    );
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn clean_root_close_retires_native_resources_and_preserves_a_peer_with_reused_ids() {
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            vivid_sdk::presenter::SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    let mut first = connect(&presenter);
    let mut peer = connect(&presenter);
    let (a, track, channel) = window(&mut first, 0.);
    let (b, peer_track, peer_channel) = window(&mut peer, 64.);
    submit(&mut first, &a, &track, &channel, 1, 0xff0000ff);
    submit(&mut peer, &b, &peer_track, &peer_channel, 1, 0x0000ffff);
    let source = SourceKey {
        producer: first.info().session_id,
        context: a.context_id(),
        surface: a.id(),
        track: track.id(),
    };
    let frame = prepared(&mut host, 2);
    let dead = Arc::downgrade(
        &frame
            .drawings()
            .iter()
            .find(|drawing| drawing.window.context().session().session_id() == source.producer)
            .unwrap()
            .compiled,
    );
    let live = Arc::downgrade(
        &frame
            .drawings()
            .iter()
            .find(|drawing| drawing.window.context().session().session_id() != source.producer)
            .unwrap()
            .compiled,
    );
    assert!(host.presented(&frame).unwrap());
    drop(frame);
    first.close().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while presenter.desktop_session_generation(source).is_some() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let frame = prepared(&mut host, 1);
    assert!(host.presented(&frame).unwrap());
    assert!(
        dead.upgrade().is_none(),
        "clean close must release the native displayed-scene reference"
    );
    assert!(live.upgrade().is_some());
    assert_eq!(peer.query_track(&peer_track).unwrap().attachment_state, 1);
    peer.close().unwrap();
}

#[test]
fn recreated_surface_cannot_reuse_a_cached_scene_or_an_old_display_receipt() {
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            vivid_sdk::presenter::SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    let mut first = connect(&presenter);
    let mut peer = connect(&presenter);
    let lane = first.open_overlay_input_lane(1).unwrap();
    lane.renew(2_000_000, Duration::from_secs(1)).unwrap();
    let (a, old, old_channel) = window(&mut first, 0.);
    let (b, peer_track, peer_channel) = window(&mut peer, 64.);
    submit(&mut first, &a, &old, &old_channel, 1, 0xff0000ff);
    submit(&mut peer, &b, &peer_track, &peer_channel, 1, 0x0000ffff);
    let shown = prepared(&mut host, 2);
    assert!(host.presented(&shown).unwrap());
    let original = host.input_target(&shown).unwrap();
    use vivid_sdk::presenter::{VectorInputEvent as Input, VectorInputResult as Result};
    let pointer = |down| Input::Pointer {
        position: [10., 10.],
        button: Some((0, down)),
        modifiers: 0,
    };
    assert_eq!(
        host.dispatch_input(&original, pointer(true)).unwrap(),
        Result::Delivered { text: false }
    );
    drop(shown);
    let stale = prepared(&mut host, 2);
    let definition = a.definition().unwrap();
    first
        .destroy_surface(&a, &RequestMetadata::default())
        .unwrap();
    let replacement = first
        .create_surface(definition, &RequestMetadata::default())
        .unwrap();
    let track = first
        .create_track(track_config(&replacement), &RequestMetadata::default())
        .unwrap();
    let channel = first.open_track_channel(&track).unwrap();
    first
        .set_overlay_window(&SetWindow {
            address: WindowAddress {
                context_id: replacement.context_id(),
                surface_id: replacement.id(),
                generation: replacement.generation().get(),
            },
            expected_revision: 0,
            options: WindowOptions::new(Rect::new(0., 0., 48., 48.).unwrap(), WindowMode::Floating),
        })
        .unwrap();
    submit(&mut first, &replacement, &track, &channel, 1, 0x00ff00ff);
    assert!(
        !host.presented(&stale).unwrap(),
        "an old physical image cannot display the replacement"
    );
    let fresh = prepared(&mut host, 2);
    let old_drawing = stale
        .drawings()
        .iter()
        .find(|drawing| drawing.window.context().session().session_id() == first.info().session_id)
        .unwrap();
    let owner = old_drawing.window;
    let new_drawing = fresh
        .drawings()
        .iter()
        .find(|drawing| drawing.window == owner)
        .unwrap();
    assert_eq!(old_drawing.submission, new_drawing.submission);
    assert!(!Arc::ptr_eq(&old_drawing.compiled, &new_drawing.compiled));
    let peer_drawing = stale
        .drawings()
        .iter()
        .find(|drawing| drawing.window != owner)
        .unwrap();
    let peer_owner = peer_drawing.window;
    assert!(
        Arc::ptr_eq(
            &peer_drawing.compiled,
            &fresh
                .drawings()
                .iter()
                .find(|drawing| drawing.window == peer_owner)
                .unwrap()
                .compiled,
        ),
        "replacing one owner preserves the peer cache"
    );
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        host.dispatch_input(&original, pointer(false)).unwrap(),
        Result::Rejected
    );
    assert_eq!(peer.query_track(&peer_track).unwrap().attachment_state, 1);
    first.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn lease_suspension_revokes_old_vector_input_and_preserves_the_peer() {
    let listener = vivid_sdk::presenter::SocketListener::bind("tcp:127.0.0.1:0").unwrap();
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            listener,
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    let mut profiles =
        PresenterConfig::terminal_with_overlay(MediaConfig::default()).supported_profiles;
    // The native host supplies baseline timed playback, not synchronized starts.
    profiles.retain(|profile| profile != vivid_protocol::registry::TIMED_MEDIA_SYNC);
    profiles.sort();
    profiles.dedup();
    let (definition, mut activation) = SessionLeaseBuilder::new(1, 9)
        .permitted_profiles(profiles)
        .contract(vivid_protocol::resource::ResourceContract::new(
            [u64::MAX / 4; vivid_protocol::resource::RESOURCE_COUNT],
        ))
        .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss)
        .disconnect_grace_us(5_000_000)
        .build()
        .unwrap();
    presenter.issue_lease(1, definition).unwrap();
    let mut first = Session::connect(config(
        &presenter,
        ProducerAuthentication::lease_activation_bytes(1, 9, activation.take().unwrap()).unwrap(),
    ))
    .unwrap();
    let mut peer = connect(&presenter);
    let (a, track, old_channel) = window(&mut first, 0.);
    let (b, peer_track, peer_channel) = window(&mut peer, 64.);
    submit(&mut first, &a, &track, &old_channel, 1, 0xff0000ff);
    submit(&mut peer, &b, &peer_track, &peer_channel, 1, 0x0000ffff);
    let frame = prepared(&mut host, 2);
    assert!(host.presented(&frame).unwrap());
    drop(frame);
    let stale = prepared(&mut host, 2);
    let source = SourceKey {
        producer: first.info().session_id,
        context: a.context_id(),
        surface: a.id(),
        track: track.id(),
    };
    let authentication = first.resume_authentication().unwrap();
    (first.cancel_handle())();
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(5);
    while presenter.desktop_session_generation(source).is_some() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut resumed = Session::connect(config(&presenter, authentication)).unwrap();
    assert!(!host.presented(&stale).unwrap());
    drop(stale);
    assert!(
        !presenter
            .overlay_pointer(1, 2., 2., Some((1, true)), 0)
            .unwrap(),
        "the old composed frame must not authorize a resumed session"
    );
    resumed.adopt_surface(&a).unwrap();
    resumed.adopt_track(&track).unwrap();
    resumed
        .advance_channel(&track, 1, &RequestMetadata::default())
        .unwrap();
    let channel = resumed.open_track_channel(&track).unwrap();
    let _ = old_channel.close();
    drop(old_channel);
    submit(&mut resumed, &a, &track, &channel, 2, 0x00ff00ff);
    submit(&mut peer, &b, &peer_track, &peer_channel, 2, 0xffffffff);
    let frame = prepared(&mut host, 2);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(resumed.query_track(&track).unwrap().attachment_state, 1);
    assert_eq!(peer.query_track(&peer_track).unwrap().attachment_state, 1);
    resumed.close().unwrap();
    peer.close().unwrap();
}

#[test]
fn native_held_reports_reject_a_closed_input_lane_without_revoking_peer_holds() {
    for role in [
        vivid_protocol::vector::HitRole::Input,
        vivid_protocol::vector::HitRole::Drag,
        vivid_protocol::vector::HitRole::Resize(4),
    ] {
        use vivid_sdk::presenter::{
            SocketListener, VectorInputEvent as Input, VectorInputResult as Result,
        };
        let presenter = Arc::new(
            VirtualVivid::start_configured_eventless(
                SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
                PresenterConfig::terminal_with_overlay(MediaConfig::default()),
            )
            .unwrap(),
        );
        let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
        presenter.update_metrics(1, 16, 16, (8, 8));
        let mut first = connect(&presenter);
        let mut peer = connect(&presenter);
        let lane = first.open_overlay_input_lane(1).unwrap();
        let peer_lane = peer.open_overlay_input_lane(1).unwrap();
        lane.renew(2_000_000, Duration::from_secs(1)).unwrap();
        peer_lane.renew(2_000_000, Duration::from_secs(1)).unwrap();
        let (a, at, ac) = window(&mut first, 0.);
        let (b, bt, bc) = window(&mut peer, 64.);
        submit(&mut first, &a, &at, &ac, 1, 0xff0000ff);
        submit(&mut peer, &b, &bt, &bc, 1, 0x0000ffff);
        let mut canvas = Canvas::new();
        canvas
            .push(vivid_protocol::vector::Command::Hit {
                id: 7,
                path: Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
                role,
                cursor: None,
            })
            .unwrap();
        ac.send_vector(&Frame {
            epoch: 1,
            revision: 2,
            canvas,
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let frame = loop {
            let frame = prepared(&mut host, 2);
            if frame.drawings().iter().any(|drawing| {
                drawing.window.context().session().session_id() == first.info().session_id
                    && drawing.submission.revision == 2
            }) {
                break frame;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(host.presented(&frame).unwrap());
        let old = host.input_target(&frame).unwrap();
        drop(frame);
        let pointer = |x, down| Input::Pointer {
            position: [x, 10.],
            button: Some((0, down)),
            modifiers: 0,
        };
        assert_eq!(
            host.dispatch_input(&old, pointer(10., true)).unwrap(),
            Result::Delivered { text: false }
        );
        lane.close().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while host.retains_input_target(&old) {
            host.flush_input();
            assert!(
                Instant::now() < deadline,
                "closed input lane retained its pointer binding"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            host.dispatch_input(&old, pointer(10., false)).unwrap(),
            Result::Rejected
        );
        let frame = prepared(&mut host, 2);
        assert!(host.presented(&frame).unwrap());
        let target = host.input_target(&frame).unwrap();
        drop(frame);
        assert_eq!(
            host.dispatch_input(&target, pointer(74., true)).unwrap(),
            Result::Delivered { text: false }
        );
        assert_eq!(
            host.dispatch_input(&old, pointer(10., false)).unwrap(),
            Result::Rejected
        );
        assert!(host.retains_input_target(&target));
        let frame = prepared(&mut host, 2);
        assert!(host.presented(&frame).unwrap());
        assert_eq!(
            host.dispatch_input(&target, pointer(74., false)).unwrap(),
            Result::Delivered { text: false }
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(OverlayLaneEvent::Input(event)) =
                peer_lane.wait_event(Duration::from_millis(10)).unwrap()
                && matches!(
                    event.event,
                    vivid_protocol::overlay::Event::Pointer {
                        button: Some((0, false)),
                        ..
                    }
                )
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "peer release was lost after another lane closed"
            );
        }
        presenter.shutdown();
    }
}

#[test]
fn an_overlay_only_watchdog_retires_modal_input_but_preserves_both_owners_pixels() {
    use vivid_sdk::presenter::{
        SocketListener, VectorInputEvent as Input, VectorInputResult as Result,
    };
    let mut config = PresenterConfig::terminal_with_overlay(MediaConfig::default());
    config
        .supported_profiles
        .retain(|profile| profile != vivid_protocol::registry::DESKTOP_INPUT);
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            config,
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    let mut first = connect(&presenter);
    let mut peer = connect(&presenter);
    let lane = first.open_overlay_input_lane(7).unwrap();
    let peer_lane = peer.open_overlay_input_lane(1).unwrap();
    lane.renew(5_000_000, Duration::from_secs(1)).unwrap();
    peer_lane.renew(5_000_000, Duration::from_secs(1)).unwrap();
    let (b, bt, bc) = window(&mut peer, 64.);
    let (a, at, ac) = window_mode(&mut first, 0., WindowMode::Modal);
    submit(&mut first, &a, &at, &ac, 1, 0xff0000ff);
    submit(&mut peer, &b, &bt, &bc, 1, 0x0000ffff);
    let frame = prepared(&mut host, 2);
    assert!(host.presented(&frame).unwrap());
    let old = host.input_target(&frame).unwrap();
    drop(frame);
    lane.renew(250_000, Duration::from_secs(1)).unwrap();
    let pointer = |x| Input::Pointer {
        position: [x, 10.],
        button: Some((0, true)),
        modifiers: 0,
    };
    assert_eq!(
        host.dispatch_input(&old, pointer(10.)).unwrap(),
        Result::Delivered { text: false }
    );
    // No host polling or input dispatch drives the timeout: the maintenance task must do it.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(OverlayLaneEvent::ConnectionLost { .. }) =
            lane.wait_event(Duration::from_millis(10)).unwrap()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "overlay-only watchdog did not close the expired lane"
        );
    }
    assert!(lane.renew(5_000_000, Duration::from_secs(1)).is_err());
    assert!(
        first.open_overlay_input_lane(7).is_err(),
        "expired generation reopened"
    );
    first.query_surface(&a).unwrap();
    peer.query_surface(&b).unwrap();
    let frame = prepared(&mut host, 2);
    assert!(host.presented(&frame).unwrap());
    let fresh = host.input_target(&frame).unwrap();
    drop(frame);
    assert_eq!(
        host.dispatch_input(&old, pointer(10.)).unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&fresh, pointer(74.)).unwrap(),
        Result::Delivered { text: false }
    );
    let replacement = first.open_overlay_input_lane(8).unwrap();
    replacement
        .renew(5_000_000, Duration::from_secs(1))
        .unwrap();
    presenter.shutdown();
}

#[test]
fn renewal_generation_is_authenticated_and_a_delayed_old_lane_exit_preserves_its_replacement() {
    use vivid_protocol::{
        auth::ChannelKey,
        messages::{self, Envelope, LaneOpen},
        overlay::wire::Renew,
        wire::{Connection, ConnectionKind, ConnectionReader, Endpoint},
    };
    let gate = Arc::new(Gate::default());
    let _release = Release(gate.clone());
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let presenter = VirtualVivid::start_configured_eventless(
        Listener {
            tcp,
            gate: gate.clone(),
        },
        PresenterConfig::terminal_with_overlay(MediaConfig::default()),
    )
    .unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    let session = connect(&presenter);
    let connection = Connection::open(
        &Endpoint::parse(&presenter.endpoint()).unwrap(),
        ConnectionKind::Lane,
    )
    .unwrap();
    let (mut reader, writer) = connection.split().unwrap();
    reader
        .set_read_deadline(Some(Duration::from_secs(3)))
        .unwrap();
    let mut open = LaneOpen {
        session_id: session.info().session_id,
        lane_generation: 3,
        client_nonce: [8; 16],
        authentication_tag: [0; 16],
    };
    open.sign(&ChannelKey::new(*session.channel_key().expose()));
    writer
        .write_record(
            messages::LANE_OPEN,
            0,
            0,
            &Envelope::new(1, open.payload()).encode().unwrap(),
        )
        .unwrap();
    assert_eq!(
        reader.read_record().unwrap().record_type,
        messages::LANE_ACCEPTED
    );
    let mut duplicate = Connection::open(
        &Endpoint::parse(&presenter.endpoint()).unwrap(),
        ConnectionKind::Lane,
    )
    .unwrap();
    duplicate
        .write_record(
            messages::LANE_OPEN,
            0,
            0,
            &Envelope::new(1, open.payload()).encode().unwrap(),
        )
        .unwrap();
    let rejected = duplicate.read_record().unwrap();
    assert_eq!(rejected.record_type, messages::ERROR);
    assert_eq!(
        messages::parse_error_reply(&rejected.body).unwrap().code,
        messages::ERROR_CHANNEL_BUSY
    );
    let reply = |reader: &mut ConnectionReader, request| {
        loop {
            let record = reader.read_record().unwrap();
            if messages::decode_control(&record.body).unwrap().request_id == request {
                break record.record_type;
            }
        }
    };
    writer
        .write_record(
            messages::OVERLAY_INPUT_RENEW,
            0,
            0,
            &Envelope::new(
                2,
                Renew {
                    lane_generation: 4,
                    watchdog_us: 5_000_000,
                }
                .payload()
                .unwrap(),
            )
            .encode()
            .unwrap(),
        )
        .unwrap();
    assert_eq!(reply(&mut reader, 2), messages::ERROR);
    writer
        .write_record(
            messages::OVERLAY_INPUT_RENEW,
            0,
            0,
            &Envelope::new(
                3,
                Renew {
                    lane_generation: 3,
                    watchdog_us: 5_000_000,
                }
                .payload()
                .unwrap(),
            )
            .encode()
            .unwrap(),
        )
        .unwrap();
    assert_eq!(reply(&mut reader, 3), messages::OK);
    gate.state.lock().unwrap().0 = true;
    let replacement = session.open_overlay_input_lane(4).unwrap();
    gate.wait();
    replacement
        .renew(5_000_000, Duration::from_secs(1))
        .unwrap();
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(3);
    while presenter.active_connection_count() > 2 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    replacement
        .renew(5_000_000, Duration::from_secs(1))
        .unwrap();
    assert!(session.open_overlay_input_lane(3).is_err());
    replacement
        .renew(5_000_000, Duration::from_secs(1))
        .unwrap();
    presenter.shutdown();
}

#[test]
fn equal_pixels_require_fresh_display_context_after_authenticated_lane_replacement() {
    use vivid_sdk::presenter::{
        SocketListener, VectorInputEvent as Input, VectorInputResult as Result,
    };
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 16, 16, (8, 8));
    let mut session = connect(&presenter);
    let lane = session.open_overlay_input_lane(1).unwrap();
    lane.renew(5_000_000, Duration::from_secs(1)).unwrap();
    let (a, at, ac) = window(&mut session, 0.);
    submit(&mut session, &a, &at, &ac, 1, 0xff0000ff);
    let shown = prepared(&mut host, 1);
    assert!(host.presented(&shown).unwrap());
    let original = host.input_target(&shown).unwrap();
    lane.renew(5_000_000, Duration::from_secs(1)).unwrap();
    let unchanged = prepared(&mut host, 1);
    assert!(
        shown.same_input_context(&unchanged),
        "ordinary renewal churned the GUI image context"
    );
    drop(unchanged);
    let pending = prepared(&mut host, 1);
    let replacement = session.open_overlay_input_lane(2).unwrap();
    replacement
        .renew(5_000_000, Duration::from_secs(1))
        .unwrap();
    assert!(
        !host.presented(&pending).unwrap(),
        "old authenticated context admitted after replacement"
    );
    assert!(host.input_target(&shown).is_none());
    drop(pending);
    let fresh = prepared(&mut host, 1);
    assert!(!shown.same_input_context(&fresh));
    assert!(
        Arc::ptr_eq(&shown.drawings()[0].compiled, &fresh.drawings()[0].compiled),
        "input renewal must reuse immutable compiled pixels"
    );
    assert!(host.presented(&fresh).unwrap());
    let target = host.input_target(&fresh).unwrap();
    let pointer = || Input::Pointer {
        position: [10., 10.],
        button: Some((0, true)),
        modifiers: 0,
    };
    assert_eq!(
        host.dispatch_input(&original, pointer()).unwrap(),
        Result::Rejected
    );
    assert_eq!(
        host.dispatch_input(&target, pointer()).unwrap(),
        Result::Delivered { text: false }
    );
    presenter.shutdown();
}
