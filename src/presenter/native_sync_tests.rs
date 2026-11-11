//! Private synchronized-start conformance while native hold recovery is unfinished.

// Rust guideline compliant 2026-10-07
use super::*;
use crate as sdk;
use crate::{PlayOptions, StartPolicy, TrackMode};
use vivid_protocol::track::MILESTONE_TRACK_LOST;
#[path = "../../tests/support/native_av.rs"]
pub(super) mod av;
use av::{Peer, clock, display, until};
#[path = "native_recovery_tests.rs"]
mod replacement;
#[path = "native_retirement_tests.rs"]
mod retirement;

fn leased_config(presenter: &VirtualVivid) -> crate::ProducerConfig {
    let mut config = crate::ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        ..Default::default()
    };
    let mut profiles = config.required_profiles.clone();
    profiles.extend(config.optional_profiles.clone());
    profiles.sort();
    profiles.dedup();
    profiles.retain(|profile| {
        lock(&presenter.state)
            .config
            .supported_profiles
            .contains(profile)
    });
    let (definition, mut activation) = crate::SessionLeaseBuilder::new(1, 9)
        .permitted_profiles(profiles)
        .contract(ResourceContract::new(
            [u64::MAX / 4; vivid_protocol::resource::RESOURCE_COUNT],
        ))
        .cleanup_policy(CleanupPolicy::SuspendOnUncleanLoss)
        .disconnect_grace_us(5_000_000)
        .build()
        .unwrap();
    presenter.issue_lease(1, definition).unwrap();
    config.authentication =
        crate::ProducerAuthentication::lease_activation_bytes(1, 9, activation.take().unwrap())
            .unwrap();
    config
}

fn resume(presenter: &VirtualVivid, session: &crate::Session) -> crate::Session {
    let authentication = session.resume_authentication().unwrap();
    let id = session.info().session_id;
    (session.cancel_handle())();
    until(|| lock(&presenter.state).sessions[&id].closed);
    crate::Session::connect(crate::ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        authentication,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn lease_reconciliation_preserves_frozen_native_hold_and_intent_without_retagging() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let control = host.visibility_control(1).unwrap();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let mut b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    for peer in [&a, &b] {
        peer.audio(0, 1, 0, 40);
        peer.video(0, 1, 0);
        peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
        peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
    }
    play(&mut a, 0, 0);
    play(&mut b, 0, 0);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    b.wait(&b.audio, MILESTONE_CLOCK_STARTED);
    control.set_visible(false).unwrap();
    until(|| {
        a.status(&a.audio)
            .playback_hold
            .unwrap()
            .position
            .is_some_and(|p| !p.estimated)
    });
    let frozen = a.status(&a.video).playback_hold.unwrap();
    let before = clock(&b, &b.audio).1;
    let mut resumed = resume(&presenter, &a.session);
    let hold = resumed
        .query_track(&a.video)
        .unwrap()
        .playback_hold
        .unwrap();
    assert!(hold.serial > frozen.serial);
    assert!(hold.held && hold.recovery_required && hold.playing_intent);
    assert_eq!(hold.position, frozen.position);
    assert_eq!(hold.reasons, vivid_protocol::timed::HOLD_NOT_VISIBLE);
    control.set_visible(true).unwrap();
    let shown = resumed
        .query_track(&a.audio)
        .unwrap()
        .playback_hold
        .unwrap();
    assert!(shown.serial > hold.serial);
    assert!(!shown.held && shown.recovery_required && shown.playing_intent);
    assert_eq!(shown.reasons, 0);
    assert_eq!(shown.position, frozen.position);
    thread::sleep(Duration::from_millis(25));
    assert_eq!(
        resumed.query_track(&a.audio).unwrap().playback_hold,
        Some(shown.clone())
    );
    assert!(clock(&b, &b.audio).1 > before);
    assert!(b.status(&b.audio).playback_hold.is_none());
    let mut replacements = Vec::new();
    for track in [&a.audio, &a.video] {
        let generation = resumed
            .advance_channel(track, 1, &Default::default())
            .unwrap();
        assert_eq!(generation.get(), 2);
        replacements.push(resumed.open_track_channel(track).unwrap());
        let status = resumed.query_track(track).unwrap();
        assert_eq!(
            status.playback_hold.as_ref().unwrap().position,
            frozen.position
        );
        assert_eq!(status.playback_hold.as_ref().unwrap().serial, shown.serial);
    }
    resumed.pause(&a.video).unwrap();
    let paused = resumed
        .query_track(&a.audio)
        .unwrap()
        .playback_hold
        .unwrap();
    assert!(paused.serial > shown.serial);
    assert!(!paused.playing_intent);
    assert_eq!(paused.position, frozen.position);
    resumed
        .play_with(
            &a.video,
            PlayOptions {
                start_pts_us: 700_000,
                minimum_buffer_us: 0,
                maximum_latency_us: 200_000,
                start_policy: StartPolicy::Synchronized,
                hold_serial: Some(paused.serial),
            },
        )
        .unwrap();
    let sought = resumed
        .query_track(&a.audio)
        .unwrap()
        .playback_hold
        .unwrap();
    assert!(sought.serial > paused.serial);
    assert!(sought.playing_intent && sought.recovery_required);
    assert_eq!(sought.position, None);
    let again = resume(&presenter, &resumed);
    let current = again.query_track(&a.video).unwrap().playback_hold.unwrap();
    assert!(current.serial > sought.serial);
    assert_eq!(current.position, sought.position);
    assert_eq!(current.playing_intent, sought.playing_intent);
    again.close().unwrap();
    // GOODBYE's reply drains before transport teardown and scoped cleanup finish.
    until(|| {
        !lock(&presenter.state)
            .surfaces
            .contains_key(&key(&a).surface)
    });
    assert_eq!(b.status(&b.video).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn visible_lease_loss_creates_unknown_startup_hold_instead_of_using_play_target() {
    let (presenter, _host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    play(&mut a, 420_000, 0);
    assert!(a.status(&a.video).playback_hold.is_none());
    let resumed = resume(&presenter, &a.session);
    let hold = resumed
        .query_track(&a.audio)
        .unwrap()
        .playback_hold
        .unwrap();
    assert!(!hold.held && hold.recovery_required && hold.playing_intent);
    assert_eq!(hold.reasons, 0);
    assert_eq!(hold.position, None);
    assert_eq!(
        resumed.query_track(&a.video).unwrap().playback_hold,
        Some(hold)
    );
    resumed.close().unwrap();
}

#[test]
fn paused_lease_intent_survives_and_baseline_leases_do_not_gain_hold_authority() {
    for sync in [true, false] {
        let (presenter, _host) = if sync { fixture() } else { av::fixture() };
        let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
        if sync {
            play(&mut a, 100_000, 0);
        } else {
            a.session.play(&a.video, 100_000, 0, 200_000).unwrap();
        }
        a.session.pause(&a.video).unwrap();
        let resumed = resume(&presenter, &a.session);
        let status = resumed.query_track(&a.audio).unwrap();
        if sync {
            let hold = status.playback_hold.unwrap();
            assert!(!hold.held && hold.recovery_required);
            assert!(!hold.playing_intent);
            assert_eq!(hold.position, None);
        } else {
            assert!(status.playback_hold.is_none());
            while let Some(event) = resumed.take_event().unwrap() {
                assert!(!matches!(event, crate::SessionEvent::PlaybackHold(_)));
            }
        }
        resumed.close().unwrap();
    }
}

#[test]
fn visible_lease_loss_keeps_a_physical_clock_in_its_original_channel_and_epoch() {
    let (presenter, _host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    a.audio(0, 1, 0, 40);
    a.video(0, 1, 0);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    play(&mut a, 0, 0);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    let beginning = Instant::now();
    let before = clock(&a, &a.audio).1;
    let resumed = resume(&presenter, &a.session);
    let hold = resumed
        .query_track(&a.audio)
        .unwrap()
        .playback_hold
        .unwrap();
    let position = hold.position.unwrap();
    assert!(!hold.held && hold.recovery_required && hold.playing_intent);
    assert_eq!(hold.reasons, 0);
    assert_eq!(
        (
            position.track_id,
            position.channel_generation,
            position.epoch
        ),
        (1, 1, 0)
    );
    assert!(position.pts_us >= before);
    // Include callback quantization and fixture scheduling, while distinguishing
    // the actual clock from decoded PCM queued far ahead of it.
    let elapsed = i64::try_from(beginning.elapsed().as_micros()).unwrap();
    assert!(position.pts_us <= before + elapsed * 2 + 100_000);
    resumed.close().unwrap();
}

#[test]
fn video_only_lease_hold_freezes_software_time_and_survives_another_resume() {
    let (presenter, _host) = fixture();
    let mut a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    a.session
        .destroy_track(&a.audio, &Default::default())
        .unwrap();
    a.video(0, 1, 0);
    a.video(0, 2, 40_000);
    until(|| a.status(&a.video).last_decoded_pts_us == 40_000);
    play(&mut a, 0, 0);
    a.wait(&a.video, MILESTONE_CLOCK_STARTED);
    let before = clock(&a, &a.video).1;
    let resumed = resume(&presenter, &a.session);
    let hold = resumed
        .query_track(&a.video)
        .unwrap()
        .playback_hold
        .unwrap();
    let position = hold.position.unwrap();
    assert!(!hold.held && hold.recovery_required && hold.playing_intent);
    assert_eq!(position.track_id, 2);
    assert!(!position.estimated);
    assert!(position.pts_us >= before);
    let again = resume(&presenter, &resumed);
    let current = again.query_track(&a.video).unwrap().playback_hold.unwrap();
    assert!(current.serial > hold.serial);
    assert_eq!(current.position, hold.position);
    assert_eq!(current.playing_intent, hold.playing_intent);
    again.close().unwrap();
}

#[test]
fn suspension_serial_exhaustion_retires_only_the_owning_lease() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    host.set_visible(1, false).unwrap();
    let a = Peer::configured(leased_config(&presenter), 0, TrackMode::Timed);
    let b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    let id = a.session.info().session_id;
    let authentication = a.session.resume_authentication().unwrap();
    lock(&presenter.state)
        .surfaces
        .get_mut(&key(&a).surface)
        .unwrap()
        .hold
        .as_mut()
        .unwrap()
        .serial = u64::MAX;
    (a.session.cancel_handle())();
    until(|| !lock(&presenter.state).sessions.contains_key(&id));
    {
        let state = lock(&presenter.state);
        assert!(!state.leases.contains_key(&(1, 9)));
        assert!(!state.surfaces.contains_key(&key(&a).surface));
    }
    assert!(
        crate::Session::connect(crate::ProducerConfig {
            endpoint_control: Some(presenter.endpoint()),
            authentication,
            ..Default::default()
        })
        .is_err()
    );
    assert!(b.status(&b.video).playback_hold.is_none());
    assert!(hold_events(&b).is_empty());
}

fn hold_events(peer: &Peer) -> Vec<crate::PlaybackHold> {
    let mut holds = Vec::new();
    while let Some(event) = peer.session.take_event().unwrap() {
        if let crate::SessionEvent::PlaybackHold(hold) = event {
            if let Some(previous) = holds.last() {
                let previous: &crate::PlaybackHold = previous;
                assert!(hold.serial > previous.serial);
            }
            holds.push(hold);
        }
    }
    holds
}

#[test]
fn hidden_startup_publishes_unknown_position_and_intent_with_monotonic_serials() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let control = host.visibility_control(1).unwrap();
    control.set_visible(false).unwrap();
    let mut a = Peer::new(&presenter, 0);
    let b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    let initial = a.status(&a.audio).playback_hold.unwrap();
    assert_eq!(initial.serial, 1);
    assert!(initial.held);
    assert!(!initial.playing_intent);
    assert!(!initial.recovery_required);
    assert_eq!(initial.reasons, vivid_protocol::timed::HOLD_NOT_VISIBLE);
    assert_eq!(initial.position, None);
    play(&mut a, -20_000, 0);
    let playing = a.status(&a.video).playback_hold.unwrap();
    assert!(playing.serial > initial.serial);
    assert!(playing.playing_intent);
    assert_eq!(playing.position, None);
    a.session.pause(&a.audio).unwrap();
    let paused = a.status(&a.video).playback_hold.unwrap();
    assert!(paused.serial > playing.serial);
    assert!(!paused.playing_intent);
    control.set_visible(true).unwrap();
    let released = a.status(&a.audio).playback_hold.unwrap();
    assert!(released.serial > paused.serial);
    assert!(!released.held);
    assert_eq!(released.reasons, 0);
    assert_eq!(released.position, None);
    let events = hold_events(&a);
    assert_eq!(events, vec![initial, playing, paused, released]);
    assert!(b.status(&b.audio).playback_hold.is_none());
    assert!(hold_events(&b).is_empty());
}

#[test]
fn native_hold_refines_callback_position_preserves_pause_and_rejects_old_seek_feedback() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let control = host.visibility_control(1).unwrap();
    let mut a = Peer::new(&presenter, 0);
    let mut b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    for peer in [&a, &b] {
        peer.audio(0, 1, 0, 50);
        peer.video(0, 1, 0);
        peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
        peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
    }
    play(&mut a, 0, 0);
    play(&mut b, 0, 0);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    b.wait(&b.audio, MILESTONE_CLOCK_STARTED);
    let peer_before = clock(&b, &b.audio).1;
    control.set_visible(false).unwrap();
    until(|| {
        a.status(&a.audio)
            .playback_hold
            .is_some_and(|hold| hold.position.is_some_and(|position| !position.estimated))
    });
    let frozen = a.status(&a.video).playback_hold.unwrap();
    let position = frozen.position.unwrap();
    assert!(frozen.playing_intent);
    assert!(!frozen.recovery_required);
    assert_eq!(
        (
            position.track_id,
            position.channel_generation,
            position.epoch
        ),
        (1, 1, 0)
    );
    assert_eq!(position.pts_us, clock(&a, &a.audio).1);
    std::thread::sleep(Duration::from_millis(35));
    assert_eq!(a.status(&a.video).playback_hold, Some(frozen.clone()));
    assert!(clock(&b, &b.audio).1 > peer_before);
    a.session.pause(&a.video).unwrap();
    let paused = a.status(&a.audio).playback_hold.unwrap();
    assert!(paused.serial > frozen.serial);
    assert!(!paused.playing_intent);
    control.set_visible(true).unwrap();
    let released = a.status(&a.video).playback_hold.unwrap();
    assert_eq!(released.position, Some(position));
    assert!(!released.held);
    assert!(!released.playing_intent);
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(clock(&a, &a.audio).1, position.pts_us);
    control.set_visible(false).unwrap();
    // A new PLAY at the retained position still retires the old operation's
    // pause feedback. Native arbitrary seeks additionally require FLUSH.
    play(&mut a, position.pts_us, 0);
    let sought = a.status(&a.video).playback_hold.unwrap();
    assert!(sought.serial > released.serial);
    assert!(sought.held && sought.playing_intent);
    assert_eq!(sought.position, None);
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(a.status(&a.audio).playback_hold.unwrap().position, None);
    let events = hold_events(&a);
    assert!(events.iter().any(|hold| hold.position == Some(position)));
    assert_eq!(events.last(), Some(&sought));
    a.session.flush(&a.audio, 1).unwrap();
    a.session.flush(&a.video, 1).unwrap();
    play(&mut a, 700_000, 0);
    assert_eq!(a.status(&a.video).playback_hold.unwrap().position, None);
    assert!(b.status(&b.video).playback_hold.is_none());
    assert!(hold_events(&b).is_empty());
}

#[test]
fn native_hold_serial_exhaustion_closes_only_its_authenticated_owner() {
    let (presenter, mut host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let control = host.visibility_control(1).unwrap();
    control.set_visible(false).unwrap();
    let a = Peer::new(&presenter, 0);
    let b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    {
        let mut state = lock(&presenter.state);
        state
            .surfaces
            .get_mut(&key(&a).surface)
            .unwrap()
            .hold
            .as_mut()
            .unwrap()
            .serial = u64::MAX;
    }
    control.set_visible(true).unwrap();
    until(|| a.session.query_track(&a.audio).is_err());
    assert!(b.status(&b.audio).playback_hold.is_none());
    assert!(hold_events(&b).is_empty());
}

#[test]
fn video_only_hold_uses_its_software_clock_and_released_status_stays_frozen() {
    let (presenter, mut host) = fixture();
    let control = host.visibility_control(1).unwrap();
    let mut a = Peer::new(&presenter, 0);
    a.session
        .destroy_track(&a.audio, &Default::default())
        .unwrap();
    for n in 0..3 {
        a.video(0, n + 1, n as i64 * 40_000);
    }
    until(|| a.status(&a.video).last_decoded_pts_us == 80_000);
    play(&mut a, 0, 0);
    a.wait(&a.video, MILESTONE_CLOCK_STARTED);
    control.set_visible(false).unwrap();
    let held = a.status(&a.video).playback_hold.unwrap();
    let position = held.position.unwrap();
    assert_eq!(position.track_id, 2);
    assert!(!position.estimated);
    assert!(held.playing_intent && !held.recovery_required);
    thread::sleep(Duration::from_millis(25));
    assert_eq!(a.status(&a.video).playback_hold, Some(held.clone()));
    control.set_visible(true).unwrap();
    let released = a.status(&a.video).playback_hold.unwrap();
    assert!(released.serial > held.serial);
    assert!(!released.held);
    assert_eq!(released.position, held.position);
    thread::sleep(Duration::from_millis(25));
    assert!(clock(&a, &a.video).1 > position.pts_us);
    assert_eq!(a.status(&a.video).playback_hold, Some(released.clone()));
    assert_eq!(hold_events(&a).last(), Some(&released));
}

#[test]
fn baseline_native_sessions_never_receive_unnegotiated_hold_state() {
    let (presenter, mut host) = av::fixture();
    let control = host.visibility_control(1).unwrap();
    control.set_visible(false).unwrap();
    let a = Peer::new(&presenter, 0);
    assert!(!a.session.supports(registry::TIMED_MEDIA_SYNC));
    assert!(a.status(&a.audio).playback_hold.is_none());
    control.set_visible(true).unwrap();
    assert!(a.status(&a.video).playback_hold.is_none());
    assert!(hold_events(&a).is_empty());
}

fn fixture() -> (Arc<VirtualVivid>, native_vector::NativeVectorHost) {
    let (presenter, mut host) = av::fixture();
    // Exercise the real authenticated protocol without advertising an incomplete
    // hold/recovery contract from production NativeVectorHost attachments.
    host.use_test_synchronized_media().unwrap();
    (presenter, host)
}
fn play(peer: &mut Peer, start: i64, minimum: u64) {
    assert!(peer.session.supports(registry::TIMED_MEDIA_SYNC));
    peer.session
        .play_with(
            &peer.video,
            PlayOptions {
                start_pts_us: start,
                minimum_buffer_us: minimum,
                maximum_latency_us: 200_000,
                start_policy: StartPolicy::Synchronized,
                hold_serial: None,
            },
        )
        .unwrap();
}
fn pending(peer: &Peer, target: i64) {
    assert_eq!(
        peer.status(&peer.audio).milestones & MILESTONE_CLOCK_STARTED,
        0
    );
    assert_eq!(
        peer.status(&peer.video).milestones & MILESTONE_CLOCK_STARTED,
        0
    );
    assert_eq!(clock(peer, &peer.video).1, target);
}
fn key(peer: &Peer) -> TrackKey {
    TrackKey {
        surface: SurfaceKey {
            session: peer.session.info().session_id,
            context: 1,
            surface: 1,
        },
        track: 2,
    }
}

#[test]
fn zero_minimum_waits_for_an_eligible_picture_and_each_new_play_has_a_fresh_barrier() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    let b = Peer::live(&presenter, 16);
    a.audio(0, 1, 0, 30);
    b.audio(0, 1, 0, 10);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    b.wait(&b.audio, MILESTONE_PRESENTED);
    play(&mut a, 0, 0);
    let peer_clock = clock(&b, &b.audio).1;
    thread::sleep(Duration::from_millis(40));
    let _ = host.poll();
    pending(&a, 0);
    assert!(clock(&b, &b.audio).1 > peer_clock);
    a.video(0, 1, -40_000);
    until(|| a.status(&a.video).last_decoded_pts_us == -40_000);
    pending(&a, 0);
    a.video(0, 2, 0);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| {
        display(&mut host);
        clock(&a, &a.video).0 == 2
    });
    assert_ne!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    a.session.pause(&a.video).unwrap();
    until(|| clock(&a, &a.audio).0 == 3);
    let frozen = clock(&a, &a.audio).1;
    play(&mut a, frozen, 0);
    thread::sleep(Duration::from_millis(40));
    let _ = host.poll();
    pending(&a, frozen);
    a.video(0, 3, frozen + 20_000);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| {
        display(&mut host);
        a.status(&a.video).last_presented_pts_us == frozen + 20_000
    });
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn accepted_eos_does_not_release_audio_until_the_admitted_decoder_has_drained() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    a.audio(0, 1, 0, 8);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    play(&mut a, 0, 0);
    let key = key(&a);
    let admission = {
        // Reproduce the admission-to-decoder handoff under its real state lock.
        // The EOS worker has accepted control state but has not drained FFmpeg.
        let mut state = lock(&presenter.state);
        let surface = state.surfaces[&key.surface].state.generation;
        let track = state.tracks.get_mut(&key).unwrap();
        track.eos_epoch = Some(0);
        track.state.milestones |= MILESTONE_EOS_ACCEPTED;
        track.accepted_surface_generation = Some((surface, track.state.channel_generation));
        native_video::Admission::capture(
            &state,
            key,
            ChannelGeneration::ONE,
            state.tracks[&key].channel_writer.as_ref().unwrap(),
        )
        .unwrap()
    };
    thread::sleep(Duration::from_millis(40));
    let _ = host.poll();
    pending(&a, 0);
    assert!(lock(&presenter.state).tracks[&key].video_drained.is_none());
    let mut decoder = native_video::Decoder::default();
    decoder
        .finish(&presenter.state, &presenter.delivery_changed, &admission)
        .unwrap();
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    assert_eq!(lock(&presenter.state).tracks[&key].video_drained, Some(0));
}

#[test]
fn audio_that_finishes_between_video_ticks_keeps_the_admitted_group_clock_running() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    a.audio(0, 1, 0, 2);
    a.ac.eos().unwrap();
    a.wait(&a.audio, MILESTONE_EOS_ACCEPTED);
    a.video(0, 1, 0);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    play(&mut a, 0, 100_000);
    // No host poll occurs between PLAY and the audio's real callback drain.
    // The last video snapshot still says the audio master had not started.
    a.wait(&a.audio, MILESTONE_BUFFERED_ENDED | MILESTONE_CLOCK_STARTED);
    until(|| {
        display(&mut host);
        clock(&a, &a.video).0 == 2
    });
    assert_ne!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    let before = clock(&a, &a.video).1;
    thread::sleep(Duration::from_millis(20));
    let _ = host.poll();
    assert!(clock(&a, &a.video).1 > before);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn audio_is_the_buffer_clock_and_a_full_video_preroll_does_not_fail_while_waiting() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    a.audio(0, 1, 0, 2);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    play(&mut a, 0, 100_000);
    for i in 0..9 {
        a.video(0, i + 1, i as i64 * 1_000);
    }
    until(|| a.status(&a.video).last_decoded_pts_us == 8_000);
    pending(&a, 0);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_TRACK_LOST, 0);
    a.audio(0, 3, 40_000, 5);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    until(|| {
        display(&mut host);
        a.status(&a.video).last_presented_pts_us == 8_000
    });
}

#[test]
fn synchronized_play_rejects_an_active_video_lost_under_baseline_policy_before_mutation() {
    let (presenter, _host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    a.video(0, 1, 0);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    a.session.play(&a.audio, 0, 100_000, 200_000).unwrap();
    a.session.pause(&a.video).unwrap();
    a.vc.close().unwrap();
    a.wait(&a.video, MILESTONE_TRACK_LOST);
    let audio = TrackKey {
        track: 1,
        ..key(&a)
    };
    let serial = lock(&presenter.state).tracks[&audio].audio_play_serial;
    let result = a.session.play_with(
        &a.audio,
        PlayOptions {
            start_pts_us: 0,
            minimum_buffer_us: 0,
            maximum_latency_us: 200_000,
            start_policy: StartPolicy::Synchronized,
            hold_serial: None,
        },
    );
    assert!(result.is_err());
    let state = lock(&presenter.state);
    let track = &state.tracks[&audio];
    assert_eq!(track.audio_play_serial, serial);
    assert_eq!(track.play_request.start_policy, 1);
    assert!(!track.playing);
}

#[test]
fn pause_cancels_pending_start_but_allows_one_target_picture_to_prime() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0);
    a.audio(0, 1, 0, 8);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    play(&mut a, 20_000, 0);
    a.session.pause(&a.video).unwrap();
    a.video(0, 1, 40_000);
    until(|| {
        display(&mut host);
        a.status(&a.video).last_presentation_id != 0
    });
    let picture = a.status(&a.video).last_presentation_id;
    a.video(0, 2, 80_000);
    until(|| a.status(&a.video).last_decoded_pts_us == 80_000);
    thread::sleep(Duration::from_millis(50));
    display(&mut host);
    pending(&a, 20_000);
    assert_eq!(a.status(&a.video).last_presentation_id, picture);
    play(&mut a, 20_000, 0);
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
}

#[test]
fn decoded_scope_and_active_video_loss_cannot_borrow_another_owners_readiness() {
    let (presenter, _host) = fixture();
    presenter.update_metrics(2, 32, 32, (1, 1));
    let mut a = Peer::new(&presenter, 0);
    let mut b = Peer::pane(&presenter, 2, 0, TrackMode::Timed);
    a.video(0, 1, 100_000);
    b.video(0, 1, 100_000);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    b.wait(&b.video, MILESTONE_OUTPUT_READY);
    // Leave both audio masters waiting for PCM so no scheduling race can
    // consume the decoded pictures while their scope fields are inspected.
    play(&mut a, 100_000, 100_000);
    play(&mut b, 100_000, 100_000);
    let ak = key(&a);
    let bk = key(&b);
    let mut state = lock(&presenter.state);
    let original = state.tracks[&ak]
        .video_playback
        .as_ref()
        .unwrap()
        .queue
        .clone();
    for field in 0..6 {
        let frame = state
            .tracks
            .get_mut(&ak)
            .unwrap()
            .video_playback
            .as_mut()
            .unwrap()
            .queue
            .values_mut()
            .next()
            .unwrap();
        match field {
            0 => frame.generation = ChannelGeneration::new(frame.generation.get() + 1),
            1 => frame.epoch += 1,
            2 => frame.reset += 1,
            3 => frame.resume += 1,
            4 => frame.surface = SurfaceGeneration::new(frame.surface.get() + 1),
            _ => frame.incarnation = std::sync::Weak::new(),
        }
        assert!(
            !native_video::group_ready(&state, ak.surface),
            "stale frame scope {field} released the clock"
        );
        assert!(native_video::group_ready(&state, bk.surface));
        state
            .tracks
            .get_mut(&ak)
            .unwrap()
            .video_playback
            .as_mut()
            .unwrap()
            .queue = original.clone();
    }
    let accepted = state
        .tracks
        .get_mut(&ak)
        .unwrap()
        .accepted_surface_generation
        .take();
    assert!(!native_video::group_ready(&state, ak.surface));
    assert!(native_video::group_ready(&state, bk.surface));
    state
        .tracks
        .get_mut(&ak)
        .unwrap()
        .accepted_surface_generation = accepted;
    state.tracks.get_mut(&ak).unwrap().state.lose().unwrap();
    assert!(!native_video::group_ready(&state, ak.surface));
    assert!(native_video::group_ready(&state, bk.surface));
}
