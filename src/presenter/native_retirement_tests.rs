//! Native sink failure, frontend replacement and stale failure isolation.

// Rust guideline compliant 2026-10-07
use super::replacement::{hold, request, resumed_play};
use super::*;

fn audio_key(peer: &Peer) -> TrackKey {
    TrackKey {
        track: 1,
        ..key(peer)
    }
}
fn start(peer: &mut Peer) {
    peer.audio(0, 1, 0, 30);
    peer.video(0, 1, 0);
    peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
    peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
    play(peer, 0, 0);
    peer.wait(&peer.audio, MILESTONE_CLOCK_STARTED);
}
fn recreate(peer: &mut Peer) {
    let audio = peer.audio.configuration().unwrap();
    let video = peer.video.configuration().unwrap();
    peer.session
        .destroy_track(&peer.video, &Default::default())
        .unwrap();
    peer.session
        .destroy_track(&peer.audio, &Default::default())
        .unwrap();
    peer.audio = peer
        .session
        .create_track(audio, &Default::default())
        .unwrap();
    peer.video = peer
        .session
        .create_track(video, &Default::default())
        .unwrap();
    peer.session.flush(&peer.audio, 3).unwrap();
    peer.session.flush(&peer.video, 7).unwrap();
    peer.ac = peer.session.open_track_channel(&peer.audio).unwrap();
    peer.vc = peer.session.open_track_channel(&peer.video).unwrap();
    let surface = crate::Surface {
        inner: peer.session.surfaces[&(1, 1)].clone(),
    };
    peer.session.query_surface(&surface).unwrap();
    peer.session
        .activate_tracks(
            &surface,
            &[
                (&peer.audio, crate::SLOT_AUDIO),
                (&peer.video, crate::SLOT_PRIMARY_VIDEO),
            ]
            .map(|(track, slot)| crate::SlotBinding {
                slot,
                track_id: track.id(),
                expected_channel_generation: track.channel_generation(),
                required_milestone: vivid_protocol::track::MILESTONE_CHANNEL_ACCEPTED,
            }),
            &Default::default(),
        )
        .unwrap();
}

#[test]
fn pause_and_visibility_holds_do_not_offer_device_retry() {
    let (presenter, mut host) = fixture();
    let control = host.visibility_control(1).unwrap();
    let mut a = Peer::new(&presenter, 0);
    start(&mut a);
    a.session.pause(&a.video).unwrap();
    assert!(!control.media_recovery_pending());
    control.set_visible(false).unwrap();
    let hidden = hold(&a);
    assert_eq!(hidden.reasons, vivid_protocol::timed::HOLD_NOT_VISIBLE);
    assert!(!control.media_recovery_pending());
    assert!(!control.recover_media());
    // A final already-admitted callback may refine the frozen position.
    let after = hold(&a);
    assert_eq!(after.reasons, hidden.reasons);
    assert!(!after.recovery_required);
}

#[test]
fn device_failure_preserves_intent_freezes_its_group_and_recovers_without_touching_a_peer() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let control = host.visibility_control(1).unwrap();
    let peer_control = host.visibility_control(2).unwrap();
    assert!(!control.media_recovery_pending());
    let mut a = Peer::new(&presenter, 0);
    let mut b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    start(&mut a);
    start(&mut b);
    let peer_before = clock(&b, &b.audio).1;
    native_audio::fail_output_for_test(&lock(&presenter.state), audio_key(&a));
    until(|| a.status(&a.audio).milestones & MILESTONE_TRACK_LOST != 0 && hold(&a).held);
    let frozen = hold(&a);
    assert!(control.media_recovery_pending());
    assert!(!peer_control.media_recovery_pending());
    assert!(frozen.playing_intent && frozen.recovery_required);
    assert_eq!(frozen.reasons, vivid_protocol::timed::HOLD_DOWNSTREAM);
    let position = frozen.position.unwrap();
    assert_eq!(
        (
            position.track_id,
            position.channel_generation,
            position.epoch
        ),
        (1, 1, 0)
    );
    let video_before = clock(&a, &a.video).1;
    thread::sleep(Duration::from_millis(30));
    host.poll();
    assert_eq!(clock(&a, &a.video).1, video_before);
    assert!(clock(&b, &b.audio).1 > peer_before);
    assert!(b.status(&b.audio).playback_hold.is_none());
    assert!(control.recover_media());
    assert!(!control.media_recovery_pending());
    let recovery = request(&a);
    let released = hold(&a);
    assert!(!released.held && released.recovery_required && released.playing_intent);
    assert_eq!(recovery.hold_serial, Some(released.serial));
    assert_eq!(recovery.presentation_pts_us, Some(position.pts_us));
    assert_eq!(clock(&a, &a.video).1, video_before);
    assert!(!control.recover_media());
    recreate(&mut a);
    a.audio(3, 1, position.pts_us, 10);
    a.video(7, 1, position.pts_us);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, position.pts_us, 0, released.serial).unwrap_or_else(|e| {
        panic!(
            "replacement PLAY failed: {e}; audio={:?}; video={:?}",
            a.status(&a.audio),
            a.status(&a.video)
        )
    });
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| {
        display(&mut host);
        !hold(&a).recovery_required
    });
    assert!(b.status(&b.video).playback_hold.is_none());
}

#[test]
fn host_replacement_retains_sessions_but_requires_new_tracks_and_fresh_receipts() {
    let (presenter, mut host) = fixture();
    let old_control = host.visibility_control(1).unwrap();
    let mut a = Peer::new(&presenter, 0);
    start(&mut a);
    let stale = host.prepare(1, 1.).unwrap();
    let (peer_presenter, _peer_host) = fixture();
    let mut b = Peer::new(&peer_presenter, 0);
    start(&mut b);
    let peer_before = clock(&b, &b.audio).1;
    drop(host);
    let frozen = hold(&a);
    assert!(frozen.held && frozen.playing_intent && frozen.recovery_required);
    assert_eq!(frozen.reasons, vivid_protocol::timed::HOLD_DETACHED);
    assert!(!old_control.recover_media());
    assert!(!old_control.media_recovery_pending());
    let mut replacement =
        native_vector::NativeVectorHost::attach_replacement(presenter.clone(), "").unwrap();
    assert!(replacement.presented(&stale).is_err());
    assert!(!old_control.set_visible(false).unwrap());
    assert!(hold(&a).held);
    assert_eq!(hold(&a).reasons, vivid_protocol::timed::HOLD_NOT_VISIBLE);
    replacement.set_visible(1, true).unwrap();
    let released = hold(&a);
    assert!(!released.held && released.recovery_required && released.playing_intent);
    assert_eq!(released.position, frozen.position);
    assert!(native_vector::NativeVectorHost::attach_replacement(presenter.clone(), "").is_err());
    recreate(&mut a);
    let target = frozen.position.unwrap().pts_us;
    a.audio(3, 1, target, 10);
    a.video(7, 1, target);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, target, 0, released.serial).unwrap();
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| {
        display(&mut replacement);
        !hold(&a).recovery_required
    });
    assert!(clock(&b, &b.audio).1 > peer_before);
    assert!(b.status(&b.video).playback_hold.is_none());
}

#[test]
fn late_audio_failure_cannot_retire_a_recreated_track_with_the_same_ids() {
    let (presenter, _host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    start(&mut a);
    let old = {
        let state = lock(&presenter.state);
        let key = audio_key(&a);
        native_audio::Admission::capture(
            &state,
            key,
            state.tracks[&key].state.channel_generation,
            state.tracks[&key].channel_writer.as_ref().unwrap(),
        )
        .unwrap()
    };
    a.session.pause(&a.video).unwrap();
    recreate(&mut a);
    native_audio::failed(&mut lock(&presenter.state), &old);
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_TRACK_LOST, 0);
    assert!(a.status(&a.audio).playback_hold.is_none());
}

#[test]
fn host_generation_exhaustion_does_not_claim_the_retired_frontend() {
    let (presenter, host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    start(&mut a);
    lock(&presenter.state).native_host_generation = u64::MAX;
    drop(host);
    let before = hold(&a);
    assert!(native_vector::NativeVectorHost::attach_replacement(presenter.clone(), "").is_err());
    let state = lock(&presenter.state);
    assert_eq!(state.native_host_generation, u64::MAX);
    assert!(!state.overlay_relay.local_host_available);
    assert_eq!(
        state.surfaces[&key(&a).surface].hold.as_ref(),
        Some(&before)
    );
}

#[test]
fn device_failure_without_a_started_clock_keeps_position_unknown() {
    let (presenter, _host) = fixture();
    let a = Peer::new(&presenter, 0);
    a.audio(0, 1, 420_000, 1);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    native_audio::fail_output_for_test(&lock(&presenter.state), audio_key(&a));
    until(|| a.status(&a.audio).milestones & MILESTONE_TRACK_LOST != 0 && hold(&a).held);
    let frozen = hold(&a);
    assert!(!frozen.playing_intent);
    assert_eq!(frozen.position, None);
    assert_eq!(frozen.reasons, vivid_protocol::timed::HOLD_DOWNSTREAM);
}

#[test]
fn lease_resume_does_not_clear_a_blocked_device_reason() {
    let (presenter, mut host) = fixture();
    let control = host.visibility_control(1).unwrap();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    start(&mut a);
    native_audio::fail_output_for_test(&lock(&presenter.state), audio_key(&a));
    until(|| a.status(&a.audio).milestones & MILESTONE_TRACK_LOST != 0 && hold(&a).held);
    let frozen = hold(&a);
    let mut fresh = resume(&presenter, &a.session);
    let surface = crate::Surface {
        inner: a.session.surfaces[&(1, 1)].clone(),
    };
    fresh.adopt_surface(&surface).unwrap();
    fresh.adopt_track(&a.video).unwrap();
    let _old = std::mem::replace(&mut a.session, fresh);
    let resumed = hold(&a);
    assert!(resumed.held && resumed.recovery_required && resumed.playing_intent);
    assert_eq!(resumed.position, frozen.position);
    assert_eq!(resumed.reasons, vivid_protocol::timed::HOLD_DOWNSTREAM);
    a.vc = a.session.open_track_channel(&a.video).unwrap();
    assert!(
        a.vc.wait_event(Duration::from_millis(25))
            .unwrap()
            .is_none()
    );
    assert!(control.recover_media());
    assert_eq!(request(&a).hold_serial, Some(hold(&a).serial));
}

#[test]
fn pane_recovery_visits_every_owner_and_leaves_other_panes_held() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let control = host.visibility_control(1).unwrap();
    let other_control = host.visibility_control(2).unwrap();
    let mut a = Peer::new(&presenter, 0);
    let mut b = Peer::new(&presenter, 16);
    let mut c = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    for peer in [&mut a, &mut b, &mut c] {
        start(peer);
        native_audio::fail_output_for_test(&lock(&presenter.state), audio_key(peer));
        until(|| {
            peer.status(&peer.audio).milestones & MILESTONE_TRACK_LOST != 0 && hold(peer).held
        });
    }
    let other = hold(&c);
    assert!(control.media_recovery_pending());
    assert!(other_control.media_recovery_pending());
    assert!(control.recover_media());
    assert!(!control.media_recovery_pending());
    assert!(other_control.media_recovery_pending());
    for peer in [&a, &b] {
        let released = hold(peer);
        assert!(!released.held && released.recovery_required);
        assert_eq!(request(peer).hold_serial, Some(released.serial));
    }
    assert_eq!(hold(&c), other);
    assert!(!control.recover_media());
}

#[test]
fn failed_hold_notification_still_freezes_and_reconciles_only_its_owner() {
    let (presenter, _host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let mut b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    start(&mut a);
    start(&mut b);
    let before = clock(&b, &b.audio).1;
    {
        let mut state = lock(&presenter.state);
        state.sessions[&a.session.info().session_id].writer.close();
        native_hold::device_failed(&mut state, audio_key(&a));
        let hold = state.surfaces[&key(&a).surface].hold.as_ref().unwrap();
        assert!(hold.held && hold.recovery_required && hold.playing_intent);
        assert_eq!(hold.reasons, vivid_protocol::timed::HOLD_DOWNSTREAM);
        for track in [audio_key(&a), key(&a)] {
            assert!(!state.tracks[&track].playing);
            assert!(state.timed_track_held(track));
        }
        assert!(state.surfaces[&key(&b).surface].hold.is_none());
    }
    let resumed = resume(&presenter, &a.session);
    let frozen = resumed
        .query_track(&a.video)
        .unwrap()
        .playback_hold
        .unwrap();
    assert!(frozen.held && frozen.recovery_required && frozen.playing_intent);
    assert_eq!(frozen.reasons, vivid_protocol::timed::HOLD_DOWNSTREAM);
    thread::sleep(Duration::from_millis(25));
    assert!(clock(&b, &b.audio).1 > before);
    assert!(b.status(&b.audio).playback_hold.is_none());
}

#[test]
fn a_failure_in_the_replacement_host_requires_its_own_device_recovery_signal() {
    let (presenter, host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    start(&mut a);
    drop(host);
    let mut replacement =
        native_vector::NativeVectorHost::attach_replacement(presenter.clone(), "").unwrap();
    replacement.set_visible(1, true).unwrap();
    assert!(!hold(&a).held);
    recreate(&mut a);
    a.audio(3, 1, 0, 1);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    native_audio::fail_output_for_test(&lock(&presenter.state), audio_key(&a));
    until(|| a.status(&a.audio).milestones & MILESTONE_TRACK_LOST != 0);
    let frozen = hold(&a);
    assert!(frozen.held && frozen.recovery_required);
    assert_eq!(frozen.reasons, vivid_protocol::timed::HOLD_DOWNSTREAM);
    let control = replacement.visibility_control(1).unwrap();
    assert!(control.recover_media());
    assert!(!hold(&a).held);
    assert!(!control.recover_media());
}
