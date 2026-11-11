//! Device-independent playback, lifetime and capacity invariants.
#![cfg(feature = "testing")]

// Rust guideline compliant 2026-10-07
use vivid_audio_output::{Format, Limits, testing};

fn format() -> Format {
    Format {
        sample_rate: 48_000,
        channels: 2,
    }
}

#[test]
fn retained_controls_do_not_keep_a_device_or_retired_callback_alive() {
    let (output, mut device) = testing::manual(format(), 1, 4).unwrap();
    let controls = output.control();
    output.channel(1).unwrap().push(&[0.5; 4]).unwrap();
    controls.start();
    drop(output);
    controls.start();
    let mut samples = [1.; 4];
    assert!(!device.render(&mut samples).unwrap());
    assert_eq!(samples, [0.; 4]);
    assert_eq!(controls.rendered_frames(), 0);
}

#[test]
fn available_writes_publish_only_complete_prefixes_and_never_wait_for_a_full_ring() {
    let (output, mut device) = testing::manual(format(), 1, 4).unwrap();
    let channel = output.channel(1).unwrap();
    channel.observe_audio_pts(0).unwrap();
    assert_eq!(
        channel
            .try_push(&[0.25, -0.25, 0.5, -0.5, 0.75, -0.75])
            .unwrap(),
        4
    );
    assert_eq!(channel.try_push(&[0.75, -0.75]).unwrap(), 0);
    let control = output.control();
    control.start();
    let mut samples = [0.; 2];
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.25, -0.25]);
    control.pause();
    assert_eq!(channel.try_push(&[0.75, -0.75]).unwrap(), 2);
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.; 2]);
    control.start();
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.5, -0.5]);
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.75, -0.75]);
    control.stop();
    assert!(channel.try_push(&[0.; 2]).is_err());
}

#[test]
fn timed_underrun_advances_silence_and_discards_late_samples_without_touching_a_peer() {
    let (a, mut renderer) = testing::manual(format(), 1, 32).unwrap();
    let (b, mut peer) = testing::manual(format(), 1, 32).unwrap();
    let channel = a.channel(1).unwrap();
    channel.observe_audio_pts(0).unwrap();
    a.configure_play(0, 0).unwrap();
    a.start();
    channel.push(&[0.25; 4]).unwrap();
    let mut samples = [0.; 8];
    renderer.render(&mut samples).unwrap();
    assert_eq!(samples, [0.25, 0.25, 0.25, 0.25, 0., 0., 0., 0.]);
    assert_eq!(
        a.clock_pts(),
        Some(83),
        "timed underrun froze the audio master clock"
    );
    channel
        .push(&[-0.75, -0.75, -0.75, -0.75, 0.5, 0.5, 0.5, 0.5])
        .unwrap();
    let mut next = [0.; 4];
    renderer.render(&mut next).unwrap();
    assert_eq!(
        next, [0.5; 4],
        "late sound was replayed after silence covered its timeline"
    );
    assert_eq!(a.clock_pts(), Some(125));
    let other = b.channel(1).unwrap();
    other.observe_audio_pts(0).unwrap();
    other.push(&[0.75; 4]).unwrap();
    b.configure_live(0);
    b.start();
    peer.render(&mut samples).unwrap();
    assert_eq!(&samples[..4], &[0.75; 4]);
    assert_eq!(
        b.clock_pts(),
        Some(41),
        "live output inherited a peer's underrun policy"
    );
}

#[test]
fn timed_pause_and_eos_do_not_advance_past_retained_media() {
    let (output, mut device) = testing::manual(format(), 1, 32).unwrap();
    let channel = output.channel(1).unwrap();
    channel.observe_audio_pts(0).unwrap();
    output.configure_play(0, 0).unwrap();
    channel.push(&[0.5; 4]).unwrap();
    let mut samples = [1.; 8];
    device.render(&mut samples).unwrap();
    assert_eq!(output.clock_pts(), Some(0));
    assert_eq!(samples, [0.; 8]);
    channel.finish_decode().unwrap();
    channel.signal_eos().unwrap();
    output.start();
    device.render(&mut samples).unwrap();
    assert_eq!(output.clock_pts(), Some(41));
    assert!(output.poll_drained().unwrap().is_ok());
    device.render(&mut samples).unwrap();
    assert_eq!(output.clock_pts(), Some(41));
    output.pause();
    device.render(&mut samples).unwrap();
    assert_eq!(output.clock_pts(), Some(41));
}

#[test]
fn a_first_future_packet_preserves_startup_silence_after_an_initial_underrun() {
    let mono = Format {
        sample_rate: 8_000,
        channels: 1,
    };
    let (output, mut device) = testing::manual(mono, 1, 32).unwrap();
    output.configure_play(0, 0).unwrap();
    output.start();
    let mut samples = [1.; 8];
    device.render(&mut samples).unwrap();
    assert_eq!(output.clock_pts(), Some(1_000));
    let channel = output.channel(1).unwrap();
    channel.observe_audio_pts(2_000).unwrap();
    channel.push(&[0.5; 8]).unwrap();
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.; 8]);
    assert_eq!(output.clock_pts(), Some(2_000));
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.5; 8]);
    assert_eq!(output.clock_pts(), Some(3_000));
}

#[test]
fn admission_is_atomic_and_positive_prebuffer_requires_a_whole_frame() {
    let (output, mut device) = testing::manual(format(), 1, 32).unwrap();
    output.configure_play(10_000, 1).unwrap();
    assert!(!output.ready_to_start());
    assert!(output.configure_play(i64::MIN, 0).is_err());
    assert!(output.configure_play(42_000, u64::MAX).is_err());
    assert_eq!(output.clock_pts(), Some(10_000));
    let channel = output.channel(1).unwrap();
    channel.observe_audio_pts(10_000).unwrap();
    assert!(channel.push(&[0.]).is_err());
    assert!(channel.push(&[f32::NAN, 0.]).is_err());
    assert_eq!(output.buffered_samples(), 0);
    channel.push(&[0.5; 2]).unwrap();
    assert!(output.ready_to_start());
    output.start();
    let mut samples = [0.; 2];
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.5; 2]);
    assert_eq!(output.clock_pts(), Some(10_020));
}

#[test]
fn short_eos_releases_pre_roll_and_drain_waits_for_actual_consumption() {
    let (output, mut device) = testing::manual(format(), 1, 32).unwrap();
    let channel = output.channel(1).unwrap();
    channel.observe_audio_pts(10_000).unwrap();
    output.configure_play(10_000, 333).unwrap();
    channel.push(&[0.5; 4]).unwrap();
    assert!(!output.ready_to_start());
    assert!(output.poll_drained().is_none());
    channel.finish_decode().unwrap();
    channel.signal_eos().unwrap();
    assert!(output.ready_to_start());
    assert!(output.poll_drained().is_none());
    output.start();
    let mut samples = [0.; 8];
    device.render(&mut samples).unwrap();
    assert_eq!(output.clock_pts(), Some(10_041));
    assert!(output.poll_drained().unwrap().is_ok());
}

#[test]
fn invalid_device_geometry_and_ring_claims_are_rejected_without_a_device() {
    assert_eq!(Limits::default().capacity_for(format()).unwrap(), 192_000);
    for value in [
        Limits {
            max_ring_samples: 0,
            ..Default::default()
        },
        Limits {
            max_ring_samples: usize::MAX,
            ..Default::default()
        },
        Limits {
            buffer_duration_us: u64::MAX,
            ..Default::default()
        },
    ] {
        assert!(value.capacity_for(format()).is_err());
    }
    assert!(
        Limits::default()
            .capacity_for(Format {
                sample_rate: u32::MAX,
                channels: 2
            })
            .is_err()
    );
    assert!(
        Limits::default()
            .capacity_for(Format {
                sample_rate: 384_000,
                channels: 32
            })
            .is_err()
    );
    assert!(testing::manual(format(), 0, 32).is_err());
    assert!(testing::manual(format(), 1, 3).is_err());
}

#[test]
fn generation_and_stop_release_full_ring_writers_without_affecting_a_peer() {
    use std::{
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };
    let (output, mut device) = testing::manual(format(), 1, 4).unwrap();
    let (other, mut peer) = testing::manual(format(), 1, 4).unwrap();
    let old = Arc::new(output.channel(1).unwrap());
    old.push(&[0.25; 4]).unwrap();
    let blocked = old.clone();
    let (entered, ready) = mpsc::channel();
    let (done, result) = mpsc::channel();
    let worker = thread::spawn(move || {
        entered.send(()).unwrap();
        done.send(blocked.push(&[0.5; 4])).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(result.recv_timeout(Duration::from_millis(20)).is_err());
    assert!(!output.advance_channel(1, 3));
    assert!(output.advance_channel(1, 2));
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    worker.join().unwrap();
    assert!(old.signal_eos().is_err());
    assert!(old.observe_audio_pts(0).is_err());
    let mut samples = [1.; 4];
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.; 4]);
    let fresh = Arc::new(output.channel(2).unwrap());
    fresh.observe_audio_pts(9_000_000).unwrap();
    fresh.push(&[0.75; 4]).unwrap();
    output.configure_play(9_000_000, 0).unwrap();
    output.start();
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.75; 4]);
    fresh.push(&[0.5; 4]).unwrap();
    output.pause();
    let (done, result) = mpsc::channel();
    let writer = fresh.clone();
    let worker = thread::spawn(move || done.send(writer.push(&[0.5; 4])).unwrap());
    assert!(result.recv_timeout(Duration::from_millis(20)).is_err());
    output.stop();
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    worker.join().unwrap();
    output.start();
    assert!(output.channel(2).is_err());
    assert!(!device.render(&mut samples).unwrap());
    assert_eq!(samples, [0.; 4]);
    let channel = other.channel(1).unwrap();
    channel.observe_audio_pts(0).unwrap();
    channel.push(&[0.25; 4]).unwrap();
    other.configure_play(0, 0).unwrap();
    other.start();
    peer.render(&mut samples).unwrap();
    assert_eq!(samples, [0.25; 4]);
    assert_eq!(other.channel_generation(), 1);
}

#[test]
fn dropping_the_output_retires_a_retained_callback_driver() {
    let (output, mut device) = testing::manual(format(), 1, 4).unwrap();
    output.start();
    drop(output);
    let mut samples = [1.; 4];
    assert!(!device.render(&mut samples).unwrap());
    assert_eq!(samples, [0.; 4]);
}

#[test]
fn fractional_live_gaps_preserve_stereo_channel_boundaries() {
    let (output, mut device) = testing::manual(format(), 1, 32).unwrap();
    let channel = output.channel(1).unwrap();
    channel.observe_audio_pts(0).unwrap();
    channel.bridge_live_gap(100).unwrap();
    channel.push(&[0.25, -0.75]).unwrap();
    output.start();
    let mut samples = [0.; 12];
    device.render(&mut samples).unwrap();
    assert_eq!(&samples[..10], &[0.; 10]);
    assert_eq!(&samples[10..], &[0.25, -0.75]);
}

#[test]
fn empty_eos_does_not_turn_future_packet_metadata_into_elapsed_audio() {
    let (output, mut device) = testing::manual(format(), 1, 32).unwrap();
    let channel = output.channel(1).unwrap();
    output.configure_play(0, 0).unwrap();
    channel.observe_audio_pts(1_000).unwrap();
    channel.finish_decode().unwrap();
    channel.signal_eos().unwrap();
    output.start();
    let mut samples = [1.; 8];
    device.render(&mut samples).unwrap();
    assert_eq!(samples, [0.; 8]);
    assert_eq!(output.clock_pts(), Some(0));
    assert!(output.poll_drained().unwrap().is_ok());
}
