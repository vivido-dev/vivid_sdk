//! Native reservation pressure must not retire a same-ID cached peer.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::presenter::SocketListener;
use crate::{OverlaySession, OverlayWindowOptions, ProducerAuthentication, ProducerConfig};
use vivid_protocol::overlay::wire::PresentationOutcome;
use vivid_protocol::vector::{Brush, Canvas, Color, Path, Rect};

#[test]
fn reservation_pressure_retires_only_the_owner_needing_new_scene_bytes() {
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    presenter.update_metrics(1, 16, 16, (8, 8));
    let mut host = NativeVectorHost::attach(Arc::clone(&presenter), "").unwrap();
    let connect = || {
        let secret = presenter.issue_pane_capability(1).unwrap();
        OverlaySession::connect(ProducerConfig {
            authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
            endpoint_control: Some(presenter.endpoint()),
            endpoint_interactive: Some(presenter.endpoint()),
            endpoint_bulk: Some(presenter.endpoint()),
            endpoint_realtime: Some(presenter.endpoint()),
            ..Default::default()
        })
        .unwrap()
    };
    let first = connect();
    let peer = connect();
    let options = |x| {
        OverlayWindowOptions::new(
            Rect::new(x, 0., 48., 48.).unwrap(),
            vivid_protocol::overlay::WindowMode::Floating,
        )
    };
    let a = first.create_window(options(0.)).unwrap();
    let b = peer.create_window(options(64.)).unwrap();
    let scene = || {
        let mut canvas = Canvas::new();
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
                Brush::Solid(Color(0xff22_44ff)),
            )
            .unwrap();
        canvas
    };
    let one = a.submit(scene()).unwrap();
    let peer_one = b.submit(scene()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let shown = loop {
        let frame = host.prepare(1, 1.).unwrap();
        if frame.drawings().len() == 2 {
            break frame;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(
        shown.drawings()[0].submission.address,
        shown.drawings()[1].submission.address
    );
    assert_ne!(
        shown.drawings()[0].window.context().session(),
        shown.drawings()[1].window.context().session()
    );
    assert!(host.presented(&shown).unwrap());
    assert_eq!(
        one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    assert_eq!(
        peer_one.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Presented)
    );
    let peer_window = shown
        .drawings()
        .iter()
        .find(|drawing| drawing.bounds.origin.x.get() == 64.)
        .unwrap()
        .window;
    drop(shown);
    let pressure = {
        let mut state = lock(&presenter.state);
        let used: usize = state
            .native_charges
            .iter()
            .filter(|(_, pin)| pin.strong_count() != 0)
            .map(|(bytes, _)| *bytes)
            .sum();
        let limit = usize::try_from(state.config.media.aggregate_retained_bytes).unwrap();
        charge(&mut state, limit - used).unwrap()
    };
    let two = a.submit(scene()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while a.reconcile().unwrap().accepted_revision != two.revision() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let remaining = host
        .prepare(1, 1.)
        .expect("pressure must not fail the whole pane");
    assert_eq!(remaining.drawings().len(), 1);
    assert_eq!(remaining.drawings()[0].window, peer_window);
    assert!(host.presented(&remaining).unwrap());
    assert_eq!(
        two.wait(Duration::from_secs(3)).unwrap(),
        Some(PresentationOutcome::Superseded)
    );
    assert_eq!(b.reconcile().unwrap().presented_revision, 1);
    drop(pressure);
    first.close().unwrap();
    peer.close().unwrap();
}
