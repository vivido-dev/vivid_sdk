//! Real canonical lane changes fence retained compositions without source resume.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::presenter::{
    MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VirtualVivid,
};
use std::time::Instant;

#[test]
fn canonical_lane_replacement_fences_retention_for_either_same_id_owner() {
    for replace_peer in [false, true] {
        let presenter = Arc::new(
            VirtualVivid::start_configured_eventless(
                SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
                PresenterConfig::terminal_with_overlay(MediaConfig::default()),
            )
            .unwrap(),
        );
        presenter.update_metrics(1, 128, 128, (1, 1));
        let mut host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
        let connect = || {
            let secret = presenter.issue_pane_capability(1).unwrap();
            OverlaySession::connect(ProducerConfig {
                endpoint_control: Some(presenter.endpoint()),
                endpoint_interactive: Some(presenter.endpoint()),
                endpoint_realtime: Some(presenter.endpoint()),
                endpoint_bulk: Some(presenter.endpoint()),
                authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
                ..ProducerConfig::default()
            })
            .unwrap()
        };
        let first = connect();
        let peer = connect();
        let a = first
            .create_window(OverlayWindowOptions::new(
                Rect::new(0., 0., 48., 48.).unwrap(),
                WindowMode::Floating,
            ))
            .unwrap();
        let b = peer
            .create_window(OverlayWindowOptions::new(
                Rect::new(64., 0., 48., 48.).unwrap(),
                WindowMode::Floating,
            ))
            .unwrap();
        let mut canvas = Canvas::new();
        canvas
            .fill(
                Path::rectangle(Rect::new(0., 0., 48., 48.).unwrap()).unwrap(),
                Brush::Solid(Color(0xff0000ff)),
            )
            .unwrap();
        let one = a.submit(canvas.clone()).unwrap();
        let two = b.submit(canvas).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let displayed = loop {
            let frame = host.prepare(1, 1.).unwrap();
            if frame.drawings().len() == 2 {
                break frame;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(
            displayed.drawings()[0].submission,
            displayed.drawings()[1].submission
        );
        assert_ne!(
            displayed.drawings()[0].window,
            displayed.drawings()[1].window
        );
        assert!(host.presented(&displayed).unwrap());
        for receipt in [&one, &two] {
            assert_eq!(
                receipt.wait(Duration::from_secs(3)).unwrap(),
                Some(PresentationOutcome::Presented)
            );
        }
        let selected = displayed.drawings()[0].window;
        drop(
            host.prepare_retaining(1, 1., &displayed, &[selected])
                .unwrap(),
        );
        let changing = if replace_peer { &peer } else { &first };
        let (new_lane, owner, resume) = {
            let session = changing.session.lock().unwrap();
            let session = session.as_ref().unwrap();
            (
                session.open_overlay_input_lane(2).unwrap(),
                session.info().session_id,
                session.info().resume_generation,
            )
        };
        new_lane.renew(5_000_000, Duration::from_secs(1)).unwrap();
        assert_eq!(new_lane.generation(), 2);
        {
            let session = changing.session.lock().unwrap();
            let session = session.as_ref().unwrap();
            assert_eq!(
                (session.info().session_id, session.info().resume_generation),
                (owner, resume)
            );
            assert!(
                session
                    .query_overlay_window(Query {
                        context_id: 1,
                        surface_id: 1
                    })
                    .is_ok()
            );
        }
        assert_eq!(
            host.prepare_retaining(1, 1., &displayed, &[selected])
                .unwrap_err()
                .to_string(),
            "stale retained native composition"
        );
        assert!(!host.presented(&displayed).unwrap());
        let fresh = host.prepare(1, 1.).unwrap();
        assert!(host.presented(&fresh).unwrap());
        assert_eq!(a.reconcile().unwrap().presented_revision, 1);
        assert_eq!(b.reconcile().unwrap().presented_revision, 1);
        for receipt in [&one, &two] {
            assert_eq!(
                receipt.wait(Duration::ZERO).unwrap(),
                Some(PresentationOutcome::Presented)
            );
        }
        new_lane.close().unwrap();
        first.close().unwrap();
        peer.close().unwrap();
        presenter.shutdown();
    }
}
