//! Native CoreAudio endpoint loss with an application-private aggregate.
#![cfg(all(target_os = "macos", feature = "testing"))]

// Rust guideline compliant 2026-10-07
use cpal::traits::{DeviceTrait, HostTrait};
use std::{
    io, thread,
    time::{Duration, Instant},
};
use vivid_audio_output::{Limits, Output, testing};

use vivid_coreaudio_fixture::Aggregate;

fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "timed out waiting for {what}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn play_silence(output: &Output, channel: &vivid_audio_output::Channel) {
    let format = output.format();
    let samples = usize::from(format.channels) * format.sample_rate as usize / 2;
    let before = output.control().rendered_frames();
    channel.push(&vec![0.0; samples]).unwrap();
    output.start();
    until("native callback progress", || {
        output.control().rendered_frames() > before
    });
}

#[test]
#[ignore = "requires a native macOS output and private CoreAudio aggregate devices"]
fn private_endpoint_removal_and_same_uid_return_preserve_peer() {
    let host = cpal::default_host();
    let default = host.default_output_device().unwrap().id().unwrap();
    let uid = format!("org.vivido.audio-output.test.{}", std::process::id());
    let id = cpal::DeviceId::new(default.host(), &uid);
    assert!(host.device_by_id(&id).is_none());
    let mut endpoint = Aggregate::create(&uid, default.id());
    let open = || testing::open_device(&id, 1, Limits::default());
    until("aggregate output format", || {
        host.device_by_id(&id)
            .is_some_and(|d| d.default_output_config().is_ok())
    });
    let first = open().unwrap();
    let old = first.channel(1).unwrap();
    let peer = Output::open(1).unwrap();
    let peer_channel = peer.channel(1).unwrap();
    play_silence(&first, &old);
    play_silence(&peer, &peer_channel);

    endpoint.remove();
    until("endpoint disappearance", || {
        host.device_by_id(&id).is_none()
    });
    assert_eq!(open().unwrap_err().kind(), io::ErrorKind::NotFound);
    until("native output error", || {
        first
            .poll_drained()
            .is_some_and(|r| r.is_err_and(|e| e.kind() == io::ErrorKind::NotConnected))
    });
    play_silence(&peer, &peer_channel);

    let mut replacement = Aggregate::create(&uid, default.id());
    until("returned output format", || {
        host.device_by_id(&id)
            .is_some_and(|d| d.default_output_config().is_ok())
    });
    let fresh = open().unwrap();
    let fresh_channel = fresh.channel(1).unwrap();
    play_silence(&fresh, &fresh_channel);
    assert!(
        old.try_push(&vec![0.; usize::from(first.format().channels)])
            .is_err()
    );
    first.stop();
    play_silence(&fresh, &fresh_channel);
    play_silence(&peer, &peer_channel);
    assert_eq!(host.default_output_device().unwrap().id().unwrap(), default);
    eprintln!(
        "native frames: retired={}, replacement={}, peer={}",
        first.control().rendered_frames(),
        fresh.control().rendered_frames(),
        peer.control().rendered_frames()
    );
    drop((fresh_channel, fresh));
    replacement.remove();
    until("fixture cleanup", || host.device_by_id(&id).is_none());
}
