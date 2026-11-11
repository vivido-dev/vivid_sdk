//! Authenticated replacement transactions after native lease loss.

// Rust guideline compliant 2026-10-07
use super::*;
use crate::{ChannelEvent, PlaybackHold, Session};
use vivid_protocol::timed::{NeedKeyframe, PRESENTATION_RESUMED};

pub(super) fn hold(peer: &Peer) -> PlaybackHold {
    peer.status(&peer.video).playback_hold.unwrap()
}
pub(super) fn reconnect(presenter: &VirtualVivid, peer: &mut Peer, audio: bool) -> Session {
    let mut fresh = resume(presenter, &peer.session);
    let surface = crate::Surface {
        inner: peer.session.surfaces[&(1, 1)].clone(),
    };
    fresh.adopt_surface(&surface).unwrap();
    fresh.adopt_track(&peer.video).unwrap();
    if audio {
        fresh.adopt_track(&peer.audio).unwrap();
    }
    std::mem::replace(&mut peer.session, fresh)
}
pub(super) fn request(peer: &Peer) -> NeedKeyframe {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(
            Instant::now() < deadline,
            "native replacement request was not delivered"
        );
        if let Some(ChannelEvent::NeedKeyframe(payload)) =
            peer.vc.wait_event(Duration::from_millis(20)).unwrap()
        {
            let request = NeedKeyframe::decode(&Value::Map(payload)).unwrap();
            if request.reason == PRESENTATION_RESUMED {
                return request;
            }
        }
    }
}
pub(super) fn replace(peer: &mut Peer, audio: bool) {
    if audio {
        peer.session
            .advance_channel(&peer.audio, 1, &Default::default())
            .unwrap();
        peer.session.flush(&peer.audio, 3).unwrap();
        peer.ac = peer.session.open_track_channel(&peer.audio).unwrap();
    }
    peer.session
        .advance_channel(&peer.video, 1, &Default::default())
        .unwrap();
    peer.session.flush(&peer.video, 7).unwrap();
    peer.vc = peer.session.open_track_channel(&peer.video).unwrap();
}
pub(super) fn resumed_play(
    peer: &mut Peer,
    target: i64,
    minimum: u64,
    serial: u64,
) -> io::Result<()> {
    peer.session.play_with(
        &peer.video,
        PlayOptions {
            start_pts_us: target,
            minimum_buffer_us: minimum,
            maximum_latency_us: 200_000,
            start_policy: StartPolicy::Synchronized,
            hold_serial: Some(serial),
        },
    )
}

#[test]
fn replacement_request_carries_frozen_position_and_waits_for_fresh_group_readiness() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let mut b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    for peer in [&a, &b] {
        peer.audio(0, 1, 0, 30);
        peer.video(0, 1, 0);
        peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
        peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
    }
    play(&mut a, 0, 0);
    play(&mut b, 0, 0);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    b.wait(&b.audio, MILESTONE_CLOCK_STARTED);
    let _old = reconnect(&presenter, &mut a, true);
    let released = hold(&a);
    assert!(!released.held && released.recovery_required);
    let target = released.position.unwrap().pts_us;
    a.vc = a.session.open_track_channel(&a.video).unwrap();
    let recovery = request(&a);
    assert_eq!(recovery.hold_serial, Some(released.serial));
    assert_eq!(recovery.presentation_pts_us, Some(target));
    assert_eq!(recovery.address.channel_generation.get(), 1);
    assert!(resumed_play(&mut a, target, 0, released.serial).is_err());
    let peer_before = clock(&b, &b.audio).1;
    replace(&mut a, true);
    a.audio(3, 31, target - 40_000, 20);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.video(7, 2, target - 40_000);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, target, 0, released.serial).unwrap();
    thread::sleep(Duration::from_millis(25));
    pending(&a, target);
    assert!(hold(&a).recovery_required);
    assert!(clock(&b, &b.audio).1 > peer_before);
    a.video(7, 3, target);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| {
        display(&mut host);
        !hold(&a).recovery_required
    });
    let completed = hold(&a);
    assert!(completed.serial > released.serial);
    assert!(!completed.held && completed.playing_intent);
    assert_eq!(a.status(&a.audio).channel_generation.get(), 2);
    assert_eq!(a.status(&a.video).media_epoch, 7);
    assert_eq!(a.status(&a.audio).media_epoch, 3);
    assert!(b.status(&b.video).playback_hold.is_none());
    while a.vc.take_event().unwrap().is_some() {}
    thread::sleep(Duration::from_millis(25));
    assert!(a.vc.take_event().unwrap().is_none());
    assert_eq!(hold(&a), completed);
    // A delayed reason-6 notification from the completed transition must not
    // poison this generation's media sender or trigger another replacement.
    let (writer, payload) = {
        let state = lock(&presenter.state);
        let key = key(&a);
        let track = &state.tracks[&key];
        let mut payload = vivid_protocol::track::need_keyframe_payload(
            track_address(key, track),
            u32::MAX,
            PRESENTATION_RESUMED,
        );
        payload.push((8, Value::Unsigned(released.serial)));
        (track.channel_writer.as_ref().unwrap().clone(), payload)
    };
    writer
        .write_record(
            messages::NEED_KEYFRAME,
            2,
            &Envelope::new(0, payload).encode().unwrap(),
        )
        .unwrap();
    assert_eq!(request(&a).hold_serial, Some(released.serial));
    a.vc.send_video(media::VideoPacket {
        epoch: 7,
        packet_id: 4,
        key: false,
        pts_us: target + 40_000,
        dts_us: target + 40_000,
        duration_us: 40_000,
        data: &include_bytes!("../../video/fixtures/red.h264")[..652],
    })
    .unwrap();
    until(|| a.status(&a.video).last_decoded_pts_us == target + 40_000);
    assert_eq!(hold(&a), completed);
}

#[test]
fn unknown_replacement_position_is_absent_and_stale_serial_does_not_mutate_playback() {
    let (presenter, _host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    play(&mut a, 420_000, 0);
    let _old = reconnect(&presenter, &mut a, true);
    let released = hold(&a);
    a.vc = a.session.open_track_channel(&a.video).unwrap();
    let recovery = request(&a);
    assert_eq!(recovery.presentation_pts_us, None);
    assert_eq!(recovery.hold_serial, Some(released.serial));
    replace(&mut a, true);
    assert!(resumed_play(&mut a, 600_000, 0, released.serial - 1).is_err());
    assert_eq!(hold(&a), released);
    a.audio(3, 1, 600_000, 10);
    a.video(7, 1, 600_000);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, 600_000, 0, released.serial).unwrap();
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| !hold(&a).recovery_required);
}

#[test]
fn paused_replacement_primes_without_consuming_and_only_an_explicit_play_starts_it() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let _old = reconnect(&presenter, &mut a, true);
    let released = hold(&a);
    assert!(!released.playing_intent);
    a.vc = a.session.open_track_channel(&a.video).unwrap();
    request(&a);
    replace(&mut a, true);
    a.video(7, 1, 0);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, 0, 100_000, released.serial).unwrap();
    a.session.pause(&a.video).unwrap();
    a.audio(3, 1, 0, 10);
    until(|| {
        display(&mut host);
        !hold(&a).recovery_required && a.status(&a.video).milestones & MILESTONE_PRESENTED != 0
    });
    let paused = hold(&a);
    assert!(!paused.playing_intent);
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(clock(&a, &a.video).1, 0);
    resumed_play(&mut a, 0, 0, paused.serial).unwrap();
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
}

#[test]
fn hidden_replacement_does_not_request_until_shown_and_damps_unchanged_state() {
    let (presenter, mut host) = fixture();
    let control = host.visibility_control(1).unwrap();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    control.set_visible(false).unwrap();
    let _old = reconnect(&presenter, &mut a, true);
    a.vc = a.session.open_track_channel(&a.video).unwrap();
    assert!(
        a.vc.wait_event(Duration::from_millis(30))
            .unwrap()
            .is_none()
    );
    control.set_visible(true).unwrap();
    let recovery = request(&a);
    let released = hold(&a);
    assert_eq!(recovery.hold_serial, Some(released.serial));
    for _ in 0..8 {
        host.poll();
        a.status(&a.audio);
    }
    assert!(
        a.vc.wait_event(Duration::from_millis(30))
            .unwrap()
            .is_none()
    );
}

#[test]
fn video_only_replacement_uses_new_software_timeline() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    a.session
        .destroy_track(&a.audio, &Default::default())
        .unwrap();
    let _old = reconnect(&presenter, &mut a, false);
    a.vc = a.session.open_track_channel(&a.video).unwrap();
    let recovery = request(&a);
    replace(&mut a, false);
    a.video(7, 1, 200_000);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, 200_000, 0, recovery.hold_serial.unwrap()).unwrap();
    until(|| {
        display(&mut host);
        !hold(&a).recovery_required && a.status(&a.video).milestones & MILESTONE_PRESENTED != 0
    });
    assert!(clock(&a, &a.video).1 >= 200_000);
}

#[test]
fn a_video_replacement_cannot_reuse_its_retired_audio_generation() {
    let (presenter, _host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let _old = reconnect(&presenter, &mut a, true);
    let released = hold(&a);
    replace(&mut a, false);
    assert!(resumed_play(&mut a, 0, 0, released.serial).is_err());
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(hold(&a), released);
    a.session
        .advance_channel(&a.audio, 1, &Default::default())
        .unwrap();
    a.session.flush(&a.audio, 3).unwrap();
    a.ac = a.session.open_track_channel(&a.audio).unwrap();
    a.audio(3, 1, 0, 10);
    a.video(7, 1, 0);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, 0, 0, released.serial).unwrap();
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| !hold(&a).recovery_required);
}

#[test]
fn paused_empty_replacement_finishes_only_after_both_decoders_drain() {
    let (presenter, _host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let _old = reconnect(&presenter, &mut a, true);
    let released = hold(&a);
    replace(&mut a, true);
    resumed_play(&mut a, 0, 0, released.serial).unwrap();
    a.session.pause(&a.video).unwrap();
    a.ac.eos().unwrap();
    assert!(hold(&a).recovery_required);
    a.vc.eos().unwrap();
    until(|| !hold(&a).recovery_required);
    assert!(!hold(&a).playing_intent);
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
}

#[test]
fn empty_audio_replacement_hands_clock_authority_to_current_video() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let _old = reconnect(&presenter, &mut a, true);
    let released = hold(&a);
    replace(&mut a, true);
    a.ac.eos().unwrap();
    a.wait(&a.audio, MILESTONE_BUFFERED_ENDED);
    a.video(7, 1, 0);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    resumed_play(&mut a, 0, 0, released.serial).unwrap();
    until(|| {
        display(&mut host);
        !hold(&a).recovery_required
    });
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_ne!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    let before = clock(&a, &a.video).1;
    thread::sleep(Duration::from_millis(25));
    assert!(clock(&a, &a.video).1 > before);
}

#[test]
fn audio_only_replacement_completes_from_its_own_callback_clock() {
    let (presenter, _host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    a.session
        .destroy_track(&a.video, &Default::default())
        .unwrap();
    let mut fresh = resume(&presenter, &a.session);
    let surface = crate::Surface {
        inner: a.session.surfaces[&(1, 1)].clone(),
    };
    fresh.adopt_surface(&surface).unwrap();
    fresh.adopt_track(&a.audio).unwrap();
    let _old = std::mem::replace(&mut a.session, fresh);
    let released = a.status(&a.audio).playback_hold.unwrap();
    assert!(!released.held && released.recovery_required);
    a.session
        .advance_channel(&a.audio, 1, &Default::default())
        .unwrap();
    a.session.flush(&a.audio, 3).unwrap();
    a.ac = a.session.open_track_channel(&a.audio).unwrap();
    a.audio(3, 1, 0, 10);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.session
        .play_with(
            &a.audio,
            PlayOptions {
                start_pts_us: 0,
                minimum_buffer_us: 0,
                maximum_latency_us: 200_000,
                start_policy: StartPolicy::Synchronized,
                hold_serial: Some(released.serial),
            },
        )
        .unwrap();
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| !a.status(&a.audio).playback_hold.unwrap().recovery_required);
    assert!(a.ac.take_event().unwrap().is_none());
}
