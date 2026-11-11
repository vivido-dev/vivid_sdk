//! Output failure remains observable while a native audio ring is saturated.

// Rust guideline compliant 2026-10-07
use super::super::native_sync_tests::av;
use super::*;

#[test]
fn stopped_full_ring_does_not_wait_for_space_before_reporting_failure() {
    let (presenter, _host) = av::fixture();
    let peer = av::Peer::new(&presenter, 0);
    let key = TrackKey {
        surface: SurfaceKey {
            session: peer.session.info().session_id,
            context: 1,
            surface: 1,
        },
        track: 1,
    };
    let scope = {
        let state = lock(&presenter.state);
        let track = &state.tracks[&key];
        Admission::capture(
            &state,
            key,
            track.state.channel_generation,
            track.channel_writer.as_ref().unwrap(),
        )
        .unwrap()
    };
    let output = Output::test_output();
    let channel = output.channel(1).unwrap();
    let capacity = output.ring_capacity_samples() as usize;
    assert_eq!(channel.try_push(&vec![0.25; capacity]).unwrap(), capacity);
    assert_eq!(output.buffered_samples(), capacity as u64);
    output.stop();
    let control = Control::default();
    let shared = Arc::downgrade(&presenter.state);
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::scope(|threads| {
        threads.spawn(|| {
            let mut staging = Staging {
                shared: &shared,
                scope: &scope,
                control: &control,
                output: &output,
                channel: &channel,
                timeline: timeline::Timeline::default(),
                timed: true,
            };
            sender.send(staging.enqueue(&[0.25, 0.25], true)).unwrap();
        });
        let observed = receiver.recv_timeout(Duration::from_secs(1));
        // Release the worker even when the regression returns to waiting for
        // impossible ring space. Scope exit joins it even if the assertion fails.
        control.stop();
        assert!(
            matches!(observed, Ok(Err(_))),
            "failure was not observed: {observed:?}"
        );
    });
}
