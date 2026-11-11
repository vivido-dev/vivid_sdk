//! Authenticated native audio workers, activation, drain and owner retirement.
#![cfg(all(feature = "presenter", feature = "vector-audio", feature = "testing"))]

// Rust guideline compliant 2026-10-07
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use vivid_protocol::{
    media,
    track::{
        AudioConfiguration, KindConfiguration, MILESTONE_BUFFERED_ENDED, MILESTONE_CLOCK_STARTED,
        MILESTONE_EOS_ACCEPTED, MILESTONE_OUTPUT_READY, MILESTONE_PRESENTED,
    },
};
use vivid_sdk::{
    presenter::{MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VirtualVivid},
    *,
};

fn fixture(test: bool) -> (Arc<VirtualVivid>, NativeVectorHost) {
    fixture_budget(test, MediaConfig::default().aggregate_retained_bytes)
}
fn fixture_budget(test: bool, bytes: u64) -> (Arc<VirtualVivid>, NativeVectorHost) {
    let p = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig {
                aggregate_retained_bytes: bytes,
                ..Default::default()
            }),
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(p.clone(), "").unwrap();
    host.enable_audio().unwrap();
    if test {
        host.use_test_audio_output().unwrap();
    }
    p.update_metrics(1, 32, 32, (1, 1));
    (p, host)
}

fn configuration(mode: TrackMode) -> TrackConfiguration {
    TrackConfiguration {
        direction: Default::default(),
        context_id: 1,
        surface_id: 1,
        track_id: 1,
        slot: SLOT_AUDIO,
        mode,
        lane: LaneClass::Realtime,
        maximum_record_body: media::audio_body_len(4096).unwrap(),
        maximum_rate_millihertz: 50_000,
        maximum_encoded_bits_per_second: 2_000_000,
        maximum_records_per_second: 50,
        maximum_inflight_body_bytes: 8192,
        kind: KindConfiguration::Audio(AudioConfiguration {
            codec: "pcm_s16le".into(),
            packetization: "pcm-packet-v1".into(),
            extradata: vec![],
            sample_rate: 48_000,
            channels: 2,
            channel_mask: 3,
            maximum_access_unit_bytes: 4096,
            codec_string: None,
        }),
        target_latency_us: 20_000,
        maximum_latency_us: 200_000,
        retained_pixel_charge: 0,
    }
}
struct Peer {
    session: Session,
    surface: Surface,
    track: Track,
    channel: Arc<TrackChannel>,
}
impl Peer {
    fn new(p: &VirtualVivid) -> Self {
        Self::with_configuration(p, configuration(TrackMode::Live))
    }
    fn with_configuration(p: &VirtualVivid, config: TrackConfiguration) -> Self {
        let secret = p.issue_pane_capability(1).unwrap();
        let mut session = Session::connect(ProducerConfig {
            endpoint_control: Some(p.endpoint()),
            authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
            ..Default::default()
        })
        .unwrap();
        let surface = session
            .create_surface(
                SurfaceDefinition {
                    context_id: 1,
                    surface_id: 1,
                    semantic_profile: GENERIC_CONTENT.into(),
                    coordinate_model: CoordinateModel::DesktopLogicalPixels,
                    logical_width: 16,
                    logical_height: 16,
                    scale_numerator: 1,
                    scale_denominator: 1,
                    rotation: 0,
                    descriptor: SurfaceDescriptor {
                        role: SurfaceRole::Figure,
                        title: String::new(),
                        semantic_content_revision: 1,
                        semantic_availability: 0,
                        locator_hint: String::new(),
                    },
                    policy: 0,
                    profile_parameters: vec![],
                },
                &Default::default(),
            )
            .unwrap();
        let track = session.create_track(config, &Default::default()).unwrap();
        let channel = Arc::new(session.open_track_channel(&track).unwrap());
        Self {
            session,
            surface,
            track,
            channel,
        }
    }
    fn send(&self, epoch: u32, id: u64, pts: i64) {
        let data = [0x00, 0x08].repeat(1920);
        self.channel
            .send_audio(media::AudioPacket {
                epoch,
                packet_id: id,
                pts_us: pts,
                dts_us: pts,
                duration_us: 20_000,
                trim_start_samples: 0,
                trim_end_samples: 0,
                data: &data,
            })
            .unwrap();
    }
    fn wait(&self, bits: u64) {
        self.session
            .wait_track(
                &self.track,
                TrackWaitCondition::MilestoneSet,
                Some(bits),
                5_000_000,
            )
            .unwrap();
    }
    fn activate(&mut self) {
        self.activate_required(MILESTONE_OUTPUT_READY);
    }
    fn activate_required(&mut self, required: u64) {
        self.session
            .activate_tracks(
                &self.surface,
                &[SlotBinding {
                    slot: SLOT_AUDIO,
                    track_id: 1,
                    expected_channel_generation: self.track.channel_generation(),
                    required_milestone: required,
                }],
                &Default::default(),
            )
            .unwrap();
    }
}

fn playback(peer: &Peer) -> (u64, i64) {
    let fields = peer
        .session
        .query_track(&peer.track)
        .unwrap()
        .playback_state
        .expect("native audio playback observation");
    let field = |key| {
        fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
            .unwrap()
    };
    (field(3).as_u64().unwrap(), field(4).as_i64().unwrap())
}
fn until(peer: &Peer, ready: impl Fn(&TrackStatus) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = peer.session.query_track(&peer.track).unwrap();
        if ready(&status) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "audio observation timed out: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn native_codec_priming_anchors_on_the_first_audible_decoded_sample() {
    fn unhex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }
    for text in [
        include_str!("../audio/fixtures/opus.json"),
        include_str!("../audio/fixtures/aac.json"),
        include_str!("../audio/fixtures/flac.json"),
        include_str!("../audio/fixtures/vorbis.json"),
    ] {
        let (p, _host) = fixture(true);
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        let mut config = configuration(TrackMode::Live);
        let KindConfiguration::Audio(audio) = &mut config.kind else {
            unreachable!()
        };
        audio.codec = v["codec"].as_str().unwrap().into();
        audio.packetization = v["packetization"].as_str().unwrap().into();
        audio.extradata = unhex(v["extradata"].as_str().unwrap());
        let mut a = Peer::with_configuration(&p, config);
        let mut b = Peer::new(&p);
        b.send(0, 1, 90_000);
        b.wait(MILESTONE_OUTPUT_READY);
        for (id, packet) in v["packets"].as_array().unwrap().iter().enumerate() {
            let pts = packet["pts_us"].as_i64().unwrap();
            a.channel
                .send_audio(media::AudioPacket {
                    epoch: 0,
                    packet_id: id as u64 + 1,
                    pts_us: pts,
                    dts_us: pts,
                    // Decoded timing, rather than declared packet duration,
                    // must determine where sound is queued.
                    duration_us: 0,
                    trim_start_samples: packet["trim_start"].as_u64().unwrap() as u32,
                    trim_end_samples: packet["trim_end"].as_u64().unwrap() as u32,
                    data: &unhex(packet["data"].as_str().unwrap()),
                })
                .unwrap();
        }
        a.channel.eos().unwrap();
        a.wait(MILESTONE_EOS_ACCEPTED);
        a.wait(MILESTONE_OUTPUT_READY);
        b.activate();
        b.channel.eos().unwrap();
        b.wait(MILESTONE_BUFFERED_ENDED);
        a.activate();
        a.wait(MILESTONE_BUFFERED_ENDED);
        let status = a.session.query_track(&a.track).unwrap();
        let frames = v["reference_samples"].as_u64().unwrap() / 2;
        assert_eq!(status.last_presentation_id, frames, "{}", v["codec"]);
        assert_eq!(
            status.last_presented_pts_us,
            (frames * 1_000_000 / 48_000) as i64,
            "{}",
            v["codec"]
        );
        assert_eq!(
            status.maximum_media_records - status.cumulative_media_records,
            1
        );
    }
}

#[test]
fn independent_live_audio_owners_require_activation_and_drain_actual_device_samples() {
    let (p, mut host) = fixture(true);
    let mut a = Peer::new(&p);
    let mut b = Peer::new(&p);
    a.send(0, 1, 10_000);
    b.send(0, 1, 90_000);
    a.wait(MILESTONE_OUTPUT_READY);
    b.wait(MILESTONE_OUTPUT_READY);
    std::thread::sleep(Duration::from_millis(30));
    host.poll();
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    assert_eq!(
        b.session.query_track(&b.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    a.activate();
    a.channel.eos().unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(
        b.session.query_track(&b.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    b.activate();
    b.channel.eos().unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    assert_ne!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    assert_ne!(
        b.session.query_track(&b.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    p.shutdown();
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.active_worker_count() != 0 {
        assert!(
            Instant::now() < deadline,
            "audio worker retirement did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn flush_replaces_only_one_owners_native_audio_timeline_on_the_same_channel() {
    let (p, _host) = fixture(true);
    let mut a = Peer::new(&p);
    let mut b = Peer::new(&p);
    a.send(0, 1, 10_000);
    b.send(0, 1, 90_000);
    a.wait(MILESTONE_OUTPUT_READY);
    b.wait(MILESTONE_OUTPUT_READY);
    a.session.flush(&a.track, 1).unwrap();
    let cleared = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        cleared.milestones & (MILESTONE_OUTPUT_READY | MILESTONE_BUFFERED_ENDED),
        0
    );
    a.send(1, 2, 7_000_000);
    a.wait(MILESTONE_OUTPUT_READY);
    a.activate();
    a.channel.eos().unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    b.activate();
    b.channel.eos().unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(
        a.session.query_track(&a.track).unwrap().last_decoded_pts_us,
        7_000_000
    );
    assert_eq!(
        b.session.query_track(&b.track).unwrap().last_decoded_pts_us,
        90_000
    );
}

#[test]
fn timed_pre_roll_waits_for_play_and_buffer_horizon_without_starting_a_peer() {
    let (p, _host) = fixture(true);
    let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let mut b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    a.send(0, 1, 0);
    b.send(0, 1, 90_000);
    a.wait(MILESTONE_OUTPUT_READY);
    b.wait(MILESTONE_OUTPUT_READY);
    a.activate();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones
            & (MILESTONE_CLOCK_STARTED | MILESTONE_PRESENTED),
        0
    );
    a.session.play(&a.track, 10_000, 40_000, 200_000).unwrap();
    a.send(0, 2, 20_000);
    until(&a, |s| s.last_decoded_pts_us == 20_000);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(playback(&a), (1, 10_000));
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_CLOCK_STARTED,
        0
    );
    a.session.pause(&a.track).unwrap();
    a.send(0, 3, 40_000);
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_BUFFERED_ENDED,
        0
    );
    a.session.play(&a.track, 10_000, 40_000, 200_000).unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        (status.last_presentation_id, status.last_presented_pts_us),
        (2400, 60_000)
    );
    assert_eq!(playback(&a), (4, 60_000));
    assert_eq!(
        b.session.query_track(&b.track).unwrap().milestones
            & (MILESTONE_PRESENTED | MILESTONE_CLOCK_STARTED),
        0
    );
    b.activate();
    b.channel.eos().unwrap();
    b.wait(MILESTONE_EOS_ACCEPTED);
    b.session.play(&b.track, 90_000, 200_000, 200_000).unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(
        b.session
            .query_track(&b.track)
            .unwrap()
            .last_presented_pts_us,
        110_000
    );
}

#[test]
fn zero_buffer_play_starts_before_media_and_late_pcm_does_not_fake_presentation() {
    let (p, _host) = fixture(true);
    let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let mut b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    a.activate_required(vivid_protocol::track::MILESTONE_CHANNEL_ACCEPTED);
    a.session.play(&a.track, 100_000, 0, 200_000).unwrap();
    a.wait(MILESTONE_CLOCK_STARTED);
    until(&a, |s| {
        s.playback_state.as_ref().is_some_and(|f| {
            f.iter()
                .any(|(k, v)| *k == 4 && v.as_i64().is_some_and(|p| p >= 130_000))
        })
    });
    let before = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        before.milestones & (MILESTONE_OUTPUT_READY | MILESTONE_PRESENTED),
        0
    );
    a.send(0, 1, 100_000);
    a.channel.eos().unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    let after = a.session.query_track(&a.track).unwrap();
    assert_eq!(after.last_presentation_id, 0);
    assert_eq!(after.milestones & MILESTONE_PRESENTED, 0);
    assert_eq!(
        b.session.query_track(&b.track).unwrap().milestones & MILESTONE_CLOCK_STARTED,
        0
    );
    b.send(0, 1, 0);
    b.wait(MILESTONE_OUTPUT_READY);
    b.channel.eos().unwrap();
    b.wait(MILESTONE_EOS_ACCEPTED);
    b.activate();
    b.session.play(&b.track, 0, 20_000, 200_000).unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(
        b.session
            .query_track(&b.track)
            .unwrap()
            .last_presentation_id,
        960
    );
}

#[test]
fn timed_pause_resume_drains_retained_pcm_without_new_packets_and_is_owner_scoped() {
    let (p, _host) = fixture(true);
    let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let mut b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    for id in 1..=10 {
        a.send(0, id, (id as i64 - 1) * 20_000);
    }
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    a.activate();
    a.session.play(&a.track, 0, 20_000, 200_000).unwrap();
    a.wait(MILESTONE_PRESENTED);
    a.session.pause(&a.track).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let paused = playback(&a);
    assert_eq!(paused.0, 3);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(playback(&a), paused);
    b.send(0, 1, 90_000);
    b.wait(MILESTONE_OUTPUT_READY);
    b.channel.eos().unwrap();
    b.wait(MILESTONE_EOS_ACCEPTED);
    b.activate();
    b.session.play(&b.track, 90_000, 0, 200_000).unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(playback(&a), paused);
    a.session.play(&a.track, paused.1, 20_000, 200_000).unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    let ended = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        (ended.last_presentation_id, ended.last_presented_pts_us),
        (9600, 200_000)
    );
}

#[test]
fn timed_leading_silence_and_eos_keep_exact_clock_without_counting_silence_as_pcm() {
    for play_first in [true, false] {
        let (p, _host) = fixture(true);
        let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
        let b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
        if play_first {
            a.session.play(&a.track, 0, 50_000, 200_000).unwrap();
        }
        a.send(0, 1, 50_000);
        a.wait(MILESTONE_OUTPUT_READY);
        a.channel.eos().unwrap();
        a.wait(MILESTONE_EOS_ACCEPTED);
        if !play_first {
            a.session.play(&a.track, 0, 50_000, 200_000).unwrap();
        }
        a.activate();
        a.wait(MILESTONE_BUFFERED_ENDED);
        let status = a.session.query_track(&a.track).unwrap();
        assert_eq!(
            (status.last_presentation_id, status.last_presented_pts_us),
            (960, 70_000)
        );
        assert_eq!(
            b.session.query_track(&b.track).unwrap().milestones & MILESTONE_CLOCK_STARTED,
            0
        );
    }
}

#[test]
fn timed_flush_retires_pre_roll_and_old_play_before_reusing_the_same_channel() {
    let (p, _host) = fixture(true);
    let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let mut b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    a.send(0, 1, 0);
    a.wait(MILESTONE_OUTPUT_READY);
    a.session.play(&a.track, 0, 100_000, 200_000).unwrap();
    a.activate();
    a.session.flush(&a.track, 1).unwrap();
    a.send(1, 2, 7_000_000);
    a.wait(MILESTONE_OUTPUT_READY);
    std::thread::sleep(Duration::from_millis(30));
    let replaced = a.session.query_track(&a.track).unwrap();
    assert_eq!(replaced.media_epoch, 1);
    assert_eq!(
        replaced.milestones & (MILESTONE_PRESENTED | MILESTONE_CLOCK_STARTED),
        0
    );
    assert_eq!(replaced.channel_generation.get(), 1);
    b.send(0, 1, 90_000);
    b.wait(MILESTONE_OUTPUT_READY);
    b.channel.eos().unwrap();
    b.wait(MILESTONE_EOS_ACCEPTED);
    b.activate();
    b.session.play(&b.track, 90_000, 0, 200_000).unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(
        a.session
            .query_track(&a.track)
            .unwrap()
            .last_presentation_id,
        0
    );
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    a.session
        .play(&a.track, 7_000_000, 20_000, 200_000)
        .unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    let ended = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        (ended.last_presentation_id, ended.last_presented_pts_us),
        (960, 7_020_000)
    );
    assert_eq!(
        ended.maximum_media_records - ended.cumulative_media_records,
        1
    );
}

#[test]
fn invalid_timed_play_is_atomic_and_does_not_create_a_native_worker() {
    let (p, _host) = fixture(true);
    let mut config = configuration(TrackMode::Timed);
    config.maximum_latency_us = 3_000_000;
    let mut a = Peer::with_configuration(&p, config);
    for (start, minimum, maximum) in [
        (i64::MIN, 0, 200_000),
        (0, 2_000_001, 3_000_000),
        (0, 0, 3_000_001),
    ] {
        assert!(a.session.play(&a.track, start, minimum, maximum).is_err());
    }
    assert!(
        a.session
            .play_with(
                &a.track,
                PlayOptions {
                    start_pts_us: 0,
                    minimum_buffer_us: 0,
                    maximum_latency_us: 200_000,
                    start_policy: StartPolicy::Synchronized,
                    hold_serial: None,
                }
            )
            .is_err()
    );
    let status = a.session.query_track(&a.track).unwrap();
    assert!(status.playback_state.is_none());
    assert_eq!(
        status.milestones & (MILESTONE_CLOCK_STARTED | MILESTONE_OUTPUT_READY),
        0
    );
    assert_eq!(p.active_worker_count(), p.active_connection_count());
    a.send(0, 1, 0);
    a.wait(MILESTONE_OUTPUT_READY);
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    a.activate();
    a.session.play(&a.track, 0, 0, 200_000).unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(
        a.session
            .query_track(&a.track)
            .unwrap()
            .last_presentation_id,
        960
    );
}

#[test]
fn drain_waits_for_actual_consumption_without_blocking_its_control_reader_or_a_peer() {
    let (p, _host) = fixture(true);
    let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let mut b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    for id in 1..=60 {
        a.send(0, id, (id as i64 - 1) * 20_000);
    }
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    a.activate();
    a.session.play(&a.track, 0, 20_000, 200_000).unwrap();
    let session = Arc::new(a.session);
    let waiter = session.clone();
    let track = a.track.clone();
    let (sent, received) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        sent.send(waiter.drain(&track)).unwrap();
    });
    assert!(
        received.recv_timeout(Duration::from_millis(30)).is_err(),
        "DRAIN acknowledged queued PCM before consumption"
    );
    session.query_session().unwrap();
    b.send(0, 1, 90_000);
    b.wait(MILESTONE_OUTPUT_READY);
    b.channel.eos().unwrap();
    b.wait(MILESTONE_EOS_ACCEPTED);
    b.activate();
    b.session.play(&b.track, 90_000, 0, 200_000).unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    received
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    let status = session.query_track(&a.track).unwrap();
    assert_ne!(status.milestones & MILESTONE_BUFFERED_ENDED, 0);
    assert_eq!(
        (status.last_presentation_id, status.last_presented_pts_us),
        (57_600, 1_200_000)
    );
}

#[test]
fn timed_underrun_advances_only_the_master_clock_and_not_the_last_presented_pcm() {
    let (p, _host) = fixture(true);
    let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    a.send(0, 1, 0);
    a.wait(MILESTONE_OUTPUT_READY);
    a.activate();
    a.session.play(&a.track, 0, 0, 200_000).unwrap();
    a.wait(MILESTONE_PRESENTED);
    until(&a, |s| {
        s.playback_state.as_ref().is_some_and(|f| {
            f.iter()
                .any(|(k, v)| *k == 4 && v.as_i64().is_some_and(|pts| pts >= 60_000))
        })
    });
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        (status.last_presentation_id, status.last_presented_pts_us),
        (960, 20_000)
    );
    assert!(playback(&a).1 >= 60_000);
    assert_eq!(
        b.session
            .query_track(&b.track)
            .unwrap()
            .last_presentation_id,
        0
    );
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    a.session.drain(&a.track).unwrap();
    let ended = a.session.query_track(&a.track).unwrap();
    assert_eq!(ended.last_presented_pts_us, 20_000);
    assert!(playback(&a).1 >= 60_000);
}

#[test]
#[cfg(feature = "vector-video")]
fn mixed_mode_av_activation_is_atomic_and_does_not_start_a_peer() {
    let (p, _host) = fixture(true);
    let mut a = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let b = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    let mut config = configuration(TrackMode::Timed);
    config.mode = TrackMode::Live;
    config.track_id = 2;
    config.slot = SLOT_PRIMARY_VIDEO;
    config.retained_pixel_charge = 256;
    config.kind = KindConfiguration::Video(VideoConfiguration {
        codec: "h264".into(),
        packetization: "h264-annexb-au-v1".into(),
        extradata: vec![],
        coded_width: 16,
        coded_height: 16,
        profile: 0,
        level: 4,
        maximum_reorder_depth: 0,
        color_primaries: 1,
        transfer: 2,
        matrix: 2,
        signal_range: 1,
        aspect_numerator: 1,
        aspect_denominator: 1,
        maximum_access_unit_bytes: 1024,
        codec_string: None,
        decoder_configuration: None,
    });
    let video = a.session.create_track(config, &Default::default()).unwrap();
    let _video_channel = a.session.open_track_channel(&video).unwrap();
    a.activate_required(vivid_protocol::track::MILESTONE_CHANNEL_ACCEPTED);
    assert!(
        a.session
            .activate_tracks(
                &a.surface,
                &[
                    SlotBinding {
                        slot: SLOT_AUDIO,
                        track_id: 1,
                        expected_channel_generation: a.track.channel_generation(),
                        required_milestone: vivid_protocol::track::MILESTONE_CHANNEL_ACCEPTED
                    },
                    SlotBinding {
                        slot: SLOT_PRIMARY_VIDEO,
                        track_id: 2,
                        expected_channel_generation: video.channel_generation(),
                        required_milestone: vivid_protocol::track::MILESTONE_CHANNEL_ACCEPTED
                    },
                ],
                &Default::default()
            )
            .is_err()
    );
    a.send(0, 1, 0);
    a.wait(MILESTONE_OUTPUT_READY);
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    a.session.play(&a.track, 0, 0, 200_000).unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(
        b.session.query_track(&b.track).unwrap().milestones & MILESTONE_CLOCK_STARTED,
        0
    );
}

#[test]
fn native_audio_reserves_resources_before_creating_a_device_worker() {
    let (p, _host) = fixture_budget(true, 40 * 1024 * 1024);
    let mut peer = Peer::new(&p);
    let mut config = configuration(TrackMode::Live);
    config.track_id = 2;
    let mut probe = config.clone();
    probe.track_id = 0;
    assert!(!peer.session.probe_track(&probe).unwrap().supported);
    assert!(
        peer.session
            .create_track(config.clone(), &Default::default())
            .is_err()
    );
    assert_eq!(p.active_worker_count(), p.active_connection_count());
    peer.session
        .destroy_track(&peer.track, &Default::default())
        .unwrap();
    assert!(peer.session.probe_track(&probe).unwrap().supported);
    assert!(
        peer.session
            .create_track(config, &Default::default())
            .is_ok()
    );
    assert_eq!(p.active_worker_count(), p.active_connection_count());
}

#[test]
fn clean_eos_transport_closure_keeps_admitted_audio_until_activation_and_drain() {
    let (p, _host) = fixture(true);
    let mut peer = Peer::new(&p);
    peer.send(0, 1, 10_000);
    peer.wait(MILESTONE_OUTPUT_READY);
    peer.channel.eos().unwrap();
    peer.wait(MILESTONE_EOS_ACCEPTED);
    peer.channel.close().unwrap();
    peer.activate();
    peer.wait(MILESTONE_BUFFERED_ENDED);
    assert_ne!(
        peer.session.query_track(&peer.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
}

#[test]
fn empty_eos_drains_without_claiming_any_audio_was_presented() {
    let (p, _host) = fixture(false);
    let peer = Peer::new(&p);
    peer.channel.eos().unwrap();
    peer.wait(MILESTONE_EOS_ACCEPTED);
    peer.wait(MILESTONE_BUFFERED_ENDED);
    assert_eq!(p.active_worker_count(), p.active_connection_count());
    assert_eq!(
        peer.session.query_track(&peer.track).unwrap().milestones
            & (MILESTONE_PRESENTED | MILESTONE_OUTPUT_READY),
        0
    );
}

#[test]
fn decoded_gaps_and_overlaps_preserve_the_queue_clock_and_an_independent_owner() {
    let (p, _host) = fixture(true);
    let mut a = Peer::new(&p);
    let mut b = Peer::new(&p);
    a.send(0, 1, 10_000);
    b.send(0, 1, 90_000);
    a.wait(MILESTONE_OUTPUT_READY);
    b.wait(MILESTONE_OUTPUT_READY);
    a.send(0, 2, 50_000); // 20 ms silence follows the already queued first packet.
    a.send(0, 3, 60_000); // Only the new suffix after 70 ms may be queued.
    a.send(0, 4, 0); // Entirely old PCM cannot replace admitted audio or its anchor.
    a.channel.eos().unwrap();
    a.wait(MILESTONE_EOS_ACCEPTED);
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    b.activate();
    b.channel.eos().unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    a.activate();
    a.wait(MILESTONE_BUFFERED_ENDED);
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(status.last_presentation_id, 3360);
    assert_eq!(status.last_presented_pts_us, 80_000);
    assert_eq!(
        status.maximum_media_records - status.cumulative_media_records,
        1
    );
    assert_eq!(
        status.maximum_body_bytes - status.cumulative_body_bytes,
        u64::from(media::audio_body_len(4096).unwrap())
    );
    assert_eq!(
        b.session.query_track(&b.track).unwrap().last_decoded_pts_us,
        90_000
    );
}

#[test]
fn old_channel_cleanup_cannot_retire_replacement_audio_or_another_owner() {
    let (p, _host) = fixture(true);
    let mut a = Peer::new(&p);
    let mut b = Peer::new(&p);
    a.send(0, 1, 10_000);
    b.send(0, 1, 90_000);
    a.wait(MILESTONE_OUTPUT_READY);
    b.wait(MILESTONE_OUTPUT_READY);
    a.session
        .advance_channel(
            &a.track,
            vivid_protocol::registry::channel_advance_reason::TIMELINE_DISCONTINUITY,
            &Default::default(),
        )
        .unwrap();
    a.session.flush(&a.track, 1).unwrap();
    let replacement = Arc::new(a.session.open_track_channel(&a.track).unwrap());
    let old = std::mem::replace(&mut a.channel, replacement);
    a.send(1, 2, 7_000_000);
    a.wait(MILESTONE_OUTPUT_READY);
    old.close().unwrap();
    a.activate();
    a.channel.eos().unwrap();
    a.wait(MILESTONE_BUFFERED_ENDED);
    b.activate();
    b.channel.eos().unwrap();
    b.wait(MILESTONE_BUFFERED_ENDED);
    let current = a.session.query_track(&a.track).unwrap();
    assert_eq!(current.channel_generation.get(), 2);
    assert_eq!(current.last_decoded_pts_us, 7_000_000);
    assert_eq!(
        b.session
            .query_track(&b.track)
            .unwrap()
            .channel_generation
            .get(),
        1
    );
}

#[test]
fn flushing_a_full_inactive_audio_ring_releases_ingress_and_returns_exact_credit() {
    for gap in [false, true] {
        let last = if gap { 3 } else { 102 };
        use std::sync::mpsc;
        let (p, _host) = fixture(true);
        let mut a = Peer::new(&p);
        let mut b = Peer::new(&p);
        let channel = a.channel.clone();
        let (sent, received) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let data = [0x00, 0x08].repeat(1920);
            for id in 1..=last {
                channel
                    .send_audio(media::AudioPacket {
                        epoch: 0,
                        packet_id: id,
                        pts_us: if gap && id >= 2 {
                            7_000_000 + (id as i64 - 2) * 20_000
                        } else {
                            (id as i64 - 1) * 20_000
                        },
                        dts_us: if gap && id >= 2 {
                            7_000_000 + (id as i64 - 2) * 20_000
                        } else {
                            (id as i64 - 1) * 20_000
                        },
                        duration_us: 20_000,
                        trim_start_samples: 0,
                        trim_end_samples: 0,
                        data: &data,
                    })
                    .unwrap();
                sent.send(id).unwrap();
            }
        });
        for id in 1..last {
            assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), id);
        }
        assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
        b.send(0, 1, 90_000);
        b.wait(MILESTONE_OUTPUT_READY);
        b.activate();
        b.channel.eos().unwrap();
        b.wait(MILESTONE_BUFFERED_ENDED);
        a.session.flush(&a.track, 1).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(3)).unwrap(), last);
        writer.join().unwrap();
        a.send(1, last + 1, 7_000_000);
        a.wait(MILESTONE_OUTPUT_READY);
        a.activate();
        a.channel.eos().unwrap();
        a.wait(MILESTONE_BUFFERED_ENDED);
        let status = a.session.query_track(&a.track).unwrap();
        assert_eq!(status.cumulative_media_records, last + 1);
        assert_eq!(
            status.maximum_media_records - status.cumulative_media_records,
            1
        );
        assert_eq!(
            status.maximum_body_bytes - status.cumulative_body_bytes,
            u64::from(media::audio_body_len(4096).unwrap())
        );
        let peer = b.session.query_track(&b.track).unwrap();
        assert_eq!(peer.cumulative_media_records, 1);
        assert_eq!(
            peer.maximum_media_records - peer.cumulative_media_records,
            1
        );
    }
}

#[test]
#[ignore = "opens the actual default audio output device"]
fn native_audio_presenter_drains_real_default_device_pcm() {
    let (p, _host) = fixture(false);
    let mut peer = Peer::new(&p);
    peer.send(0, 1, 0);
    peer.wait(MILESTONE_OUTPUT_READY);
    peer.activate();
    peer.channel.eos().unwrap();
    peer.wait(MILESTONE_BUFFERED_ENDED);
}

#[test]
#[ignore = "opens the actual default audio output device"]
fn native_timed_audio_presenter_drains_real_default_device_pcm() {
    let (p, _host) = fixture(false);
    let mut peer = Peer::with_configuration(&p, configuration(TrackMode::Timed));
    for id in 1..=3 {
        peer.send(0, id, (id as i64 - 1) * 20_000);
    }
    peer.wait(MILESTONE_OUTPUT_READY);
    peer.channel.eos().unwrap();
    peer.wait(MILESTONE_EOS_ACCEPTED);
    peer.session
        .play(&peer.track, 10_000, 20_000, 200_000)
        .unwrap();
    peer.activate();
    peer.session.drain(&peer.track).unwrap();
    let status = peer.session.query_track(&peer.track).unwrap();
    assert_ne!(status.milestones & MILESTONE_BUFFERED_ENDED, 0);
    assert!(status.last_presentation_id != 0);
    // Whole-frame placement can differ by less than one frame at the lowest
    // supported device rate; the selected native format need not be 48 kHz.
    assert!(status.last_presented_pts_us.abs_diff(60_000) < 125);
}
