//! Authenticated audio-master video scheduling and independent owner retirement.
#![cfg(all(
    feature = "presenter",
    feature = "vector-video",
    feature = "vector-audio",
    feature = "testing"
))]

// Rust guideline compliant 2026-10-07
use std::time::Duration;
use vivid_protocol::track::{
    MILESTONE_BUFFERED_ENDED, MILESTONE_CLOCK_STARTED, MILESTONE_EOS_ACCEPTED,
    MILESTONE_OUTPUT_READY, MILESTONE_PRESENTED, MILESTONE_TRACK_LOST,
};
use vivid_sdk::*;

use vivid_sdk as sdk;
#[path = "support/native_av.rs"]
mod av;
use av::{Peer, clock, display, fixture, fixture_device, until};

#[test]
fn hidden_startup_and_visibility_pause_only_their_timed_pane_and_preserve_user_pause() {
    let (p, mut host) = fixture();
    p.update_metrics(2, 32, 32, (1, 1));
    assert!(host.set_visible(0, false).is_err());
    assert!(host.set_visible(3, false).is_err());
    host.set_visible(1, false).unwrap();
    let mut a = Peer::new(&p, 0);
    let mut b = Peer::pane(&p, 2, 0, TrackMode::Timed);
    assert!(
        !a.session
            .supports(vivid_protocol::registry::TIMED_MEDIA_SYNC)
    );
    for peer in [&mut a, &mut b] {
        peer.audio(0, 1, 0, 30);
        for i in 0..6 {
            peer.video(0, i + 1, i as i64 * 40_000);
        }
        peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
        peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
        peer.session.play(&peer.audio, 0, 0, 200_000).unwrap();
    }
    b.wait(&b.audio, MILESTONE_PRESENTED);
    for _ in 0..15 {
        let _ = host.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    let hidden = host.prepare(1, 1.).unwrap();
    assert!(!host.presented(&hidden).unwrap());
    host.set_visible(1, true).unwrap();
    assert!(!host.presented(&hidden).unwrap());
    drop(hidden);
    a.wait(&a.audio, MILESTONE_PRESENTED);
    until(|| {
        display(&mut host);
        clock(&a, &a.video).0 == 2
    });
    let stale = host.prepare(1, 1.).unwrap();
    host.set_visible(1, false).unwrap();
    until(|| {
        let _ = host.poll();
        clock(&a, &a.audio).0 == 3 && clock(&a, &a.video).0 == 3
    });
    let frozen = clock(&a, &a.audio).1;
    let peer_clock = clock(&b, &b.audio).1;
    std::thread::sleep(Duration::from_millis(80));
    let _ = host.poll();
    assert_eq!(clock(&a, &a.audio).1, frozen);
    assert_eq!(clock(&a, &a.video).1, frozen);
    assert!(clock(&b, &b.audio).1 > peer_clock);
    let error = a
        .session
        .wait_track(&a.video, TrackWaitCondition::PlaybackStarted, None, 100_000)
        .unwrap_err();
    assert_eq!(
        error
            .get_ref()
            .and_then(|e| e.downcast_ref::<PresenterError>())
            .unwrap()
            .code,
        vivid_protocol::messages::ERROR_NOT_VISIBLE
    );
    host.set_visible(1, true).unwrap();
    assert!(!host.presented(&stale).unwrap());
    drop(stale);
    until(|| {
        display(&mut host);
        clock(&a, &a.audio).1 > frozen
    });
    a.session.pause(&a.video).unwrap();
    until(|| clock(&a, &a.audio).0 == 3);
    let paused = clock(&a, &a.audio).1;
    host.set_visible(1, false).unwrap();
    host.set_visible(1, true).unwrap();
    std::thread::sleep(Duration::from_millis(40));
    let _ = host.poll();
    assert_eq!(clock(&a, &a.audio).1, paused);
    assert_eq!(clock(&a, &a.video).1, paused);
    for peer in [&a, &b] {
        assert_eq!(
            peer.status(&peer.audio).milestones & MILESTONE_TRACK_LOST,
            0
        );
        assert_eq!(
            peer.status(&peer.video).milestones & MILESTONE_TRACK_LOST,
            0
        );
    }
}

#[test]
fn hiding_live_media_does_not_pause_its_audio_clock() {
    let (p, mut host) = fixture();
    let a = Peer::live(&p, 0);
    a.audio(0, 1, 0, 20);
    a.wait(&a.audio, MILESTONE_PRESENTED);
    let before = clock(&a, &a.audio).1;
    host.set_visible(1, false).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let _ = host.poll();
    assert!(clock(&a, &a.audio).1 > before);
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn visibility_controller_freezes_timed_audio_while_the_render_host_does_not_poll() {
    let (p, mut host) = fixture();
    p.update_metrics(2, 32, 32, (1, 1));
    let mut a = Peer::new(&p, 0);
    let mut b = Peer::pane(&p, 2, 0, TrackMode::Timed);
    let control = host.visibility_control(1).unwrap();
    for peer in [&mut a, &mut b] {
        peer.audio(0, 1, 0, 30);
        peer.video(0, 1, 0);
        peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
        peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
        peer.session.play(&peer.audio, 0, 0, 200_000).unwrap();
        peer.wait(&peer.audio, MILESTONE_PRESENTED);
    }
    let old = host.prepare(1, 1.).unwrap();
    assert!(host.presented(&old).unwrap());
    let clone = control.clone();
    let (sent, reply) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || sent.send(clone.set_visible(false)).unwrap());
    assert!(reply.recv_timeout(Duration::from_secs(3)).unwrap().unwrap());
    thread.join().unwrap();
    // No renderer poll services this control or the following frozen interval.
    until(|| clock(&a, &a.audio).0 == 3);
    let frozen = clock(&a, &a.audio).1;
    let peer_clock = clock(&b, &b.audio).1;
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(clock(&a, &a.audio).1, frozen);
    assert!(clock(&b, &b.audio).1 > peer_clock);
    assert!(control.set_visible(true).unwrap());
    assert!(!host.presented(&old).unwrap());
    until(|| clock(&a, &a.audio).1 > frozen);
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_TRACK_LOST, 0);
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn video_targeted_play_gates_the_audio_master_on_both_prebuffers_and_pauses_the_group() {
    linked_play(true);
}

#[test]
#[ignore = "opens the actual default audio output device"]
fn default_device_audio_master_gates_linked_video_and_resumes_retained_pcm() {
    linked_play(false);
}

fn linked_play(test: bool) {
    let (p, mut host) = fixture_device(test);
    let mut a = Peer::new(&p, 0);
    let mut b = Peer::new(&p, 16);
    for peer in [&a, &b] {
        peer.audio(0, 1, 0, 8);
        peer.video(0, 1, 0);
        peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
        peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
    }
    a.session.play(&a.video, 0, 100_000, 200_000).unwrap();
    b.session.play(&b.audio, 0, 0, 200_000).unwrap();
    b.wait(&b.audio, MILESTONE_PRESENTED);
    std::thread::sleep(Duration::from_millis(110));
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(clock(&a, &a.video).1, 0);
    a.video(0, 2, 40_000);
    a.video(0, 3, 80_000);
    a.ac.eos().unwrap();
    a.vc.eos().unwrap();
    a.wait(&a.audio, MILESTONE_EOS_ACCEPTED | MILESTONE_PRESENTED);
    a.session.pause(&a.video).unwrap();
    until(|| clock(&a, &a.audio).0 == 3 && clock(&a, &a.video).0 == 3);
    let frozen = clock(&a, &a.audio).1;
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clock(&a, &a.audio).1, frozen);
    assert_eq!(clock(&a, &a.video).1, frozen);
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_BUFFERED_ENDED, 0);
    a.session.play(&a.audio, frozen, 0, 200_000).unwrap();
    until(|| {
        display(&mut host);
        a.status(&a.audio).milestones & MILESTONE_BUFFERED_ENDED != 0
            && a.status(&a.video).milestones & MILESTONE_BUFFERED_ENDED != 0
    });
    assert!(a.status(&a.audio).last_presentation_id > 0);
    assert!(a.status(&a.audio).last_presented_pts_us.abs_diff(160_000) < 125);
    assert_eq!(b.status(&b.audio).media_epoch, 0);
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn short_audio_eos_and_audio_loss_relinquish_the_clock_without_losing_video_or_a_peer() {
    for eos in [true, false] {
        let (p, mut host) = fixture();
        let mut a = Peer::new(&p, 0);
        let b = Peer::new(&p, 16);
        a.audio(0, 1, 0, 2);
        b.audio(0, 1, 700_000, 1);
        b.wait(&b.audio, MILESTONE_OUTPUT_READY);
        for (id, pts) in [(1, 0), (2, 100_000), (3, 200_000)] {
            a.video(0, id, pts);
        }
        a.wait(&a.video, MILESTONE_OUTPUT_READY);
        if eos {
            a.ac.eos().unwrap();
            a.wait(&a.audio, MILESTONE_EOS_ACCEPTED);
        }
        a.vc.eos().unwrap();
        a.session.play(&a.audio, 0, 0, 200_000).unwrap();
        a.wait(&a.audio, MILESTONE_PRESENTED);
        if !eos {
            a.ac.close().unwrap();
            a.wait(&a.audio, MILESTONE_TRACK_LOST);
        }
        if eos {
            a.wait(&a.audio, MILESTONE_BUFFERED_ENDED);
        }
        // Healthy video can still pause/resume after its audio master has
        // finished or failed. It must not reopen the retired device timeline.
        a.session.pause(&a.video).unwrap();
        let frozen = clock(&a, &a.video).1;
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(clock(&a, &a.video).1, frozen);
        a.session.play(&a.video, frozen, 0, 200_000).unwrap();
        until(|| {
            display(&mut host);
            a.status(&a.video).milestones & MILESTONE_BUFFERED_ENDED != 0
        });
        assert!(clock(&a, &a.video).1 >= 240_000);
        assert_eq!(a.status(&a.video).last_presented_pts_us, 200_000);
        assert_eq!(a.status(&a.video).milestones & MILESTONE_TRACK_LOST, 0);
        assert_eq!(
            b.status(&b.audio).milestones & (MILESTONE_PRESENTED | MILESTONE_TRACK_LOST),
            0
        );
    }
}

#[test]
fn linked_play_preflights_all_members_and_flush_pauses_only_its_owner_group() {
    let (p, mut host) = fixture();
    let mut a = Peer::new(&p, 0);
    let mut b = Peer::new(&p, 16);
    for peer in [&a, &b] {
        peer.audio(0, 1, 0, 20);
        peer.video(0, 1, 0);
        peer.wait(&peer.audio, MILESTONE_OUTPUT_READY);
        peer.wait(&peer.video, MILESTONE_OUTPUT_READY);
    }
    assert!(a.session.play(&a.audio, 0, 0, 300_000).is_err());
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    a.session.play(&a.audio, 0, 0, 200_000).unwrap();
    b.session.play(&b.video, 0, 0, 200_000).unwrap();
    a.wait(&a.audio, MILESTONE_PRESENTED);
    b.wait(&b.audio, MILESTONE_PRESENTED);
    let old = host.prepare(1, 1.).unwrap();
    a.session.flush(&a.video, 1).unwrap();
    until(|| clock(&a, &a.audio).0 == 3);
    let frozen = clock(&a, &a.audio).1;
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(clock(&a, &a.audio).1, frozen);
    assert!(clock(&b, &b.audio).1 > frozen);
    a.session.flush(&a.audio, 1).unwrap();
    a.audio(1, 21, 1_000_000, 3);
    a.video(1, 2, 1_000_000);
    a.wait(&a.audio, MILESTONE_OUTPUT_READY);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    host.presented(&old).unwrap();
    assert_eq!(a.status(&a.video).last_presentation_id, 0);
    a.session.play(&a.video, 1_000_000, 0, 200_000).unwrap();
    a.ac.eos().unwrap();
    a.vc.eos().unwrap();
    until(|| {
        display(&mut host);
        a.status(&a.audio).milestones & MILESTONE_BUFFERED_ENDED != 0
            && a.status(&a.video).milestones & MILESTONE_BUFFERED_ENDED != 0
    });
    assert_eq!(a.status(&a.audio).media_epoch, 1);
    assert_eq!(a.status(&a.video).last_presented_pts_us, 1_000_000);
    assert_eq!(b.status(&b.audio).media_epoch, 0);
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn resumed_play_with_a_new_buffer_waits_for_video_and_fresh_callback_progress() {
    let (p, _host) = fixture();
    let mut a = Peer::new(&p, 0);
    let b = Peer::new(&p, 16);
    a.audio(0, 1, 0, 20);
    a.video(0, 1, 0);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    a.session.play(&a.audio, 0, 0, 200_000).unwrap();
    a.wait(&a.audio, MILESTONE_PRESENTED);
    a.session.pause(&a.video).unwrap();
    until(|| clock(&a, &a.audio).0 == 3);
    let frozen = clock(&a, &a.audio).1;
    a.session.play(&a.video, frozen, 100_000, 200_000).unwrap();
    std::thread::sleep(Duration::from_millis(110));
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(clock(&a, &a.audio).1, frozen);
    for (id, pts) in [(2, 40_000), (3, 80_000), (4, 120_000)] {
        a.video(0, id, pts);
    }
    a.wait(&a.audio, MILESTONE_CLOCK_STARTED);
    assert!(clock(&a, &a.audio).1 > frozen);
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
}

#[test]
fn live_future_video_waits_for_its_owner_audio_clock_and_keeps_only_the_latest_pts() {
    let (p, mut host) = fixture();
    let a = Peer::live(&p, 0);
    let b = Peer::live(&p, 16);
    a.audio(0, 1, 0, 4);
    a.wait(&a.audio, MILESTONE_PRESENTED);
    a.video(0, 1, 0);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    until(|| {
        display(&mut host);
        a.status(&a.video).milestones & MILESTONE_PRESENTED != 0
    });
    b.audio(0, 1, 0, 8);
    b.video(0, 1, 0);
    until(|| {
        display(&mut host);
        b.status(&b.video).milestones & MILESTONE_PRESENTED != 0
    });
    for (id, pts) in [(2, 400_000), (3, 600_000)] {
        a.video(0, id, pts);
    }
    until(|| a.status(&a.video).last_decoded_pts_us == 600_000);
    a.video(0, 4, 500_000);
    until(|| a.status(&a.video).cumulative_media_records == 4);
    display(&mut host);
    assert_eq!(a.status(&a.video).last_decoded_pts_us, 600_000);
    assert_eq!(a.status(&a.video).last_presented_pts_us, 0);
    assert_eq!(host.prepare(1, 1.).unwrap().rasters().len(), 2);
    a.vc.eos().unwrap();
    a.wait(&a.video, MILESTONE_EOS_ACCEPTED);
    a.vc.close().unwrap();
    assert_eq!(a.status(&a.video).milestones & MILESTONE_BUFFERED_ENDED, 0);
    a.audio(0, 5, 600_000, 4);
    a.ac.eos().unwrap();
    until(|| {
        display(&mut host);
        a.status(&a.video).milestones & MILESTONE_BUFFERED_ENDED != 0
    });
    assert_eq!(a.status(&a.video).last_presented_pts_us, 600_000);
    assert_eq!(b.status(&b.video).last_presented_pts_us, 0);
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn live_preview_is_immediate_before_audio_has_a_clock_then_new_future_pictures_wait() {
    let (p, mut host) = fixture();
    let a = Peer::live(&p, 0);
    let b = Peer::live(&p, 16);
    a.video(0, 1, 2_000_000);
    until(|| {
        display(&mut host);
        a.status(&a.video).milestones & MILESTONE_PRESENTED != 0
    });
    assert_eq!(a.status(&a.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
    assert_eq!(a.status(&a.video).last_presented_pts_us, 2_000_000);
    a.audio(0, 1, 0, 4);
    a.wait(&a.audio, MILESTONE_PRESENTED);
    a.video(0, 2, 2_100_000);
    until(|| a.status(&a.video).last_decoded_pts_us == 2_100_000);
    b.video(0, 1, 0);
    until(|| {
        display(&mut host);
        b.status(&b.video).milestones & MILESTONE_PRESENTED != 0
    });
    assert_eq!(a.status(&a.video).last_presented_pts_us, 2_000_000);
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_CLOCK_STARTED, 0);
}

#[test]
fn live_pending_clean_eos_video_survives_audio_loss_without_borrowing_a_peer_clock() {
    let (p, mut host) = fixture();
    let a = Peer::live(&p, 0);
    let b = Peer::live(&p, 16);
    a.audio(0, 1, 0, 4);
    a.wait(&a.audio, MILESTONE_PRESENTED);
    a.video(0, 1, 1_000_000);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    a.vc.eos().unwrap();
    a.wait(&a.video, MILESTONE_EOS_ACCEPTED);
    a.vc.close().unwrap();
    b.audio(0, 1, 2_000_000, 4);
    b.video(0, 1, 2_000_000);
    until(|| {
        display(&mut host);
        b.status(&b.video).milestones & MILESTONE_PRESENTED != 0
    });
    assert_eq!(
        a.status(&a.video).milestones & (MILESTONE_PRESENTED | MILESTONE_BUFFERED_ENDED),
        0
    );
    a.ac.close().unwrap();
    a.wait(&a.audio, MILESTONE_TRACK_LOST);
    until(|| {
        display(&mut host);
        a.status(&a.video).milestones & MILESTONE_BUFFERED_ENDED != 0
    });
    assert_eq!(a.status(&a.video).last_presented_pts_us, 1_000_000);
    assert_eq!(a.status(&a.video).milestones & MILESTONE_TRACK_LOST, 0);
    assert_eq!(b.status(&b.video).last_presented_pts_us, 2_000_000);
    assert_eq!(b.status(&b.audio).milestones & MILESTONE_TRACK_LOST, 0);
}

#[test]
fn live_flush_and_recreated_track_retire_future_pixels_even_after_clean_eos() {
    let (p, mut host) = fixture();
    let mut a = Peer::live(&p, 0);
    let b = Peer::live(&p, 16);
    a.audio(0, 1, 0, 4);
    a.video(0, 1, 1_000_000);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    b.audio(0, 1, 0, 4);
    b.video(0, 1, 0);
    until(|| {
        display(&mut host);
        b.status(&b.video).milestones & MILESTONE_PRESENTED != 0
    });
    a.session.flush(&a.video, 1).unwrap();
    a.video(1, 2, 2_000_000);
    a.wait(&a.video, MILESTONE_OUTPUT_READY);
    a.vc.eos().unwrap();
    a.wait(&a.video, MILESTONE_EOS_ACCEPTED);
    a.vc.close().unwrap();
    let configuration = a.video.configuration().unwrap();
    a.session
        .destroy_track(&a.video, &Default::default())
        .unwrap();
    a.video = a
        .session
        .create_track(configuration, &Default::default())
        .unwrap();
    a.vc = a.session.open_track_channel(&a.video).unwrap();
    assert_eq!(a.video.channel_generation().get(), 1);
    a.vc.eos().unwrap();
    a.wait(&a.video, MILESTONE_BUFFERED_ENDED);
    display(&mut host);
    let status = a.status(&a.video);
    assert_eq!(status.media_epoch, 0);
    assert_eq!(status.last_presentation_id, 0);
    assert_eq!(
        status.milestones & (MILESTONE_PRESENTED | MILESTONE_OUTPUT_READY),
        0
    );
    assert_eq!(host.prepare(1, 1.).unwrap().rasters().len(), 1);
    assert_eq!(b.status(&b.video).milestones & MILESTONE_TRACK_LOST, 0);
}
