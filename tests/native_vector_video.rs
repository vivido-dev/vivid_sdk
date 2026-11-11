//! Authenticated native video pixels, display receipts and owner isolation.
#![cfg(all(feature = "presenter", feature = "vector-video"))]

// Rust guideline compliant 2026-10-07
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use vivid_protocol::{
    cbor::Value,
    track::{
        KindConfiguration, MILESTONE_BUFFERED_ENDED, MILESTONE_OUTPUT_READY, MILESTONE_TRACK_LOST,
    },
};
use vivid_sdk::{
    presenter::{
        MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VectorFrame, VirtualVivid,
    },
    *,
};

#[path = "support/gated_media.rs"]
mod gated_media;

fn fixture() -> (Arc<VirtualVivid>, NativeVectorHost) {
    let presenter = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let host = NativeVectorHost::attach(presenter.clone(), "").unwrap();
    presenter.update_metrics(1, 32, 32, (1, 1));
    (presenter, host)
}

const STREAM: &[u8] = include_bytes!("../video/fixtures/red.h264");
fn send_reordered(channel: &TrackChannel, index: usize) {
    send_reordered_at(channel, index, 0, 0, 1);
}
fn send_reordered_at(channel: &TrackChannel, index: usize, epoch: u32, origin: i64, first_id: u64) {
    let stream = include_bytes!("../video/fixtures/reordered.h264");
    let offsets = [0, 891, 903, 915, 927, 945, 957];
    let pts = [0, 120_000, 40_000, 80_000, 200_000, 160_000];
    channel
        .send_video(vivid_protocol::media::VideoPacket {
            epoch,
            key: index == 0,
            packet_id: first_id + index as u64,
            pts_us: origin + pts[index],
            dts_us: origin + (index as i64 - 2) * 40_000,
            duration_us: 40_000,
            data: &stream[offsets[index]..offsets[index + 1]],
        })
        .unwrap();
}
fn send(channel: &TrackChannel, epoch: u32, id: u64, pts: i64, data: &[u8], key: bool) {
    channel
        .send_video(vivid_protocol::media::VideoPacket {
            epoch,
            key,
            packet_id: id,
            pts_us: pts,
            dts_us: pts,
            duration_us: 40_000,
            data,
        })
        .unwrap();
}
struct Peer {
    session: Session,
    surface: Surface,
    track: Track,
    channel: TrackChannel,
    node: SceneNode,
}
impl Peer {
    fn new(p: &VirtualVivid, x: i64) -> Self {
        Self::configured(p, x, 0)
    }
    fn configured(p: &VirtualVivid, x: i64, reorder: u8) -> Self {
        Self::mode(p, x, reorder, TrackMode::Live, 40_000)
    }
    fn timed(p: &VirtualVivid, x: i64, reorder: u8) -> Self {
        Self::mode(p, x, reorder, TrackMode::Timed, 40_000)
    }
    fn mode(p: &VirtualVivid, x: i64, reorder: u8, mode: TrackMode, first_duration: u64) -> Self {
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
        let track = session
            .create_track(
                TrackConfiguration {
                    direction: Default::default(),
                    context_id: 1,
                    surface_id: 1,
                    track_id: 1,
                    slot: 1,
                    mode,
                    lane: LaneClass::Realtime,
                    maximum_record_body: 4096,
                    maximum_rate_millihertz: 60_000,
                    maximum_encoded_bits_per_second: 1_000_000,
                    maximum_records_per_second: 60,
                    maximum_inflight_body_bytes: 4096,
                    kind: KindConfiguration::Video(VideoConfiguration {
                        codec: "h264".into(),
                        packetization: "h264-annexb-au-v1".into(),
                        extradata: Vec::new(),
                        coded_width: 16,
                        coded_height: 16,
                        profile: 0,
                        level: 4,
                        maximum_reorder_depth: reorder,
                        color_primaries: 1,
                        transfer: 2,
                        matrix: 2,
                        signal_range: 1,
                        aspect_numerator: 1,
                        aspect_denominator: 1,
                        maximum_access_unit_bytes: 1024,
                        codec_string: None,
                        decoder_configuration: None,
                    }),
                    target_latency_us: 0,
                    maximum_latency_us: 200_000,
                    retained_pixel_charge: 256,
                },
                &Default::default(),
            )
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        if reorder == 0 {
            channel
                .send_video(vivid_protocol::media::VideoPacket {
                    epoch: 0,
                    key: true,
                    packet_id: 1,
                    pts_us: 10_000,
                    dts_us: 10_000,
                    duration_us: first_duration,
                    data: &STREAM[..652],
                })
                .unwrap();
        } else {
            for index in 0..3 {
                send_reordered(&channel, index);
            }
        }
        session
            .wait_track(
                &track,
                TrackWaitCondition::MilestoneSet,
                Some(MILESTONE_OUTPUT_READY),
                5_000_000,
            )
            .unwrap();
        session
            .activate_tracks(
                &surface,
                &[SlotBinding {
                    slot: 1,
                    track_id: 1,
                    expected_channel_generation: track.channel_generation(),
                    required_milestone: MILESTONE_OUTPUT_READY,
                }],
                &Default::default(),
            )
            .unwrap();
        let node = SceneNode {
            owning_context_id: 1,
            node_id: 1,
            surface_context_id: 1,
            surface_id: 1,
            geometry: vec![
                (0, Value::Unsigned(1)),
                (1, Value::Unsigned((x << 32) as u64)),
                (2, Value::Unsigned(0)),
                (3, Value::Unsigned(16 << 32)),
                (4, Value::Unsigned(16 << 32)),
                (5, Value::Unsigned(1)),
            ],
            fit: Fit::Fill,
            linear_sampling: false,
            z_index: 0,
            visible: true,
            opacity: u16::MAX,
            clip: None,
        };
        session.create_node(&node, &Default::default()).unwrap();
        Self {
            session,
            surface,
            track,
            channel,
            node,
        }
    }
    fn decoded(&self, pts: i64) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = self.session.query_track(&self.track).unwrap();
            assert_eq!(
                status.milestones & MILESTONE_TRACK_LOST,
                0,
                "video lost: {status:?}"
            );
            if status.last_decoded_pts_us == pts && status.milestones & MILESTONE_OUTPUT_READY != 0
            {
                return;
            }
            assert!(Instant::now() < deadline, "video never decoded: {status:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn presented(&self) -> u64 {
        self.session
            .query_track(&self.track)
            .unwrap()
            .last_presentation_id
    }
}
fn prepare(host: &mut NativeVectorHost, count: usize) -> VectorFrame {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let frame = host.prepare(1, 1.).unwrap();
        if frame.rasters().len() == count {
            return frame;
        }
        assert!(Instant::now() < deadline, "raster node never became ready");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn playback(peer: &Peer) -> (u64, i64) {
    let status = peer.session.query_track(&peer.track).unwrap();
    let fields = status
        .playback_state
        .expect("native timed playback observation");
    let field = |key| {
        fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
            .unwrap()
    };
    (field(3).as_u64().unwrap(), field(4).as_i64().unwrap())
}

fn flush_overtakes_unread_video(mode: TrackMode) {
    use vivid_protocol::{media, messages, track::MILESTONE_EOS_ACCEPTED};
    let (listener, gate) = gated_media::Listener::new();
    let p = Arc::new(
        VirtualVivid::start_configured_eventless(
            listener,
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(p.clone(), "").unwrap();
    p.update_metrics(1, 32, 32, (1, 1));
    let mut a = Peer::mode(&p, 0, 0, mode, 40_000);
    let b = Peer::new(&p, 16);
    a.decoded(10_000);
    b.decoded(10_000);
    let old = prepare(&mut host, 2);
    gate.arm(messages::VIDEO_PACKET);
    send(&a.channel, 0, 2, 50_000, &STREAM[652..661], false);
    gate.wait();
    a.session.flush(&a.track, 1).unwrap();
    assert!(host.presented(&old).unwrap());
    assert_eq!(a.presented(), 0, "FLUSH retired the old display receipt");
    drop(old);
    send(&b.channel, 0, 2, 50_000, &STREAM[652..661], false);
    b.decoded(50_000);
    let peer = prepare(&mut host, 1);
    assert!(host.presented(&peer).unwrap());
    assert_eq!(b.presented(), 2);
    drop(peer);
    gate.release();
    let event = a.channel.wait_event(Duration::from_secs(3)).unwrap();
    let Some(ChannelEvent::Error(error)) = event else {
        panic!("expected stale-epoch rejection, got {event:?}");
    };
    assert_eq!(error.code, messages::ERROR_STALE_EPOCH);
    assert!(!error.fatal, "an old unread packet retired the new epoch");
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(status.media_epoch, 1);
    assert_eq!(status.last_media_id, 0, "rejected packet was accepted");
    assert_eq!(
        status.milestones
            & (MILESTONE_OUTPUT_READY | MILESTONE_TRACK_LOST | MILESTONE_EOS_ACCEPTED),
        0
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !a
        .channel
        .media_credit_available(media::video_body_len(652).unwrap())
    {
        assert!(
            Instant::now() < deadline,
            "stale packet leaked media credit"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    send(&a.channel, 1, 3, 90_000, &STREAM[..652], true);
    a.decoded(90_000);
    assert_eq!(a.presented(), 0, "decode alone is not a display receipt");
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    assert_eq!(
        a.session
            .query_track(&a.track)
            .unwrap()
            .last_presented_pts_us,
        90_000
    );
    assert_eq!(b.presented(), 2);
}

#[test]
fn live_flush_rejects_unread_old_media_without_retiring_the_new_epoch() {
    flush_overtakes_unread_video(TrackMode::Live);
}

#[test]
fn timed_flush_rejects_unread_old_media_without_retiring_the_new_epoch() {
    flush_overtakes_unread_video(TrackMode::Timed);
}

#[test]
fn timed_video_waits_for_pre_roll_eos_releases_short_input_and_pause_freezes_the_clock() {
    use vivid_protocol::track::MILESTONE_CLOCK_STARTED;
    let (p, mut host) = fixture();
    let mut a = Peer::timed(&p, 0, 0);
    let b = Peer::new(&p, 16);
    let preview = prepare(&mut host, 2);
    assert!(host.presented(&preview).unwrap());
    a.session.play(&a.track, 10_000, 150_000, 200_000).unwrap();
    assert_eq!(playback(&a), (1, 10_000));
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_CLOCK_STARTED,
        0
    );
    assert!(!host.presented(&preview).unwrap());
    assert_eq!(
        a.presented(),
        0,
        "preview receipt belongs to the previous playback generation"
    );
    drop(preview);
    send(&a.channel, 0, 2, 50_000, &STREAM[652..661], false);
    a.decoded(50_000);
    send(&a.channel, 0, 3, 90_000, &STREAM[661..], false);
    a.decoded(90_000);
    assert_eq!(playback(&a), (1, 10_000));
    send(&b.channel, 0, 2, 50_000, &STREAM[652..661], false);
    b.decoded(50_000);
    let peer_only = prepare(&mut host, 1);
    assert!(host.presented(&peer_only).unwrap());
    assert_eq!(b.presented(), 2);
    drop(peer_only);
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    assert_ne!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_CLOCK_STARTED,
        0
    );
    a.session.pause(&a.track).unwrap();
    let frozen = playback(&a);
    assert_eq!(frozen.0, 3);
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(playback(&a), frozen);
    let paused = host.prepare(1, 1.).unwrap();
    host.presented(&paused).unwrap();
    if a.presented() != 0 {
        assert!(
            a.session
                .query_track(&a.track)
                .unwrap()
                .last_presented_pts_us
                <= frozen.1
        );
    }
    a.session.play(&a.track, frozen.1, 0, 200_000).unwrap();
    host.presented(&paused).unwrap();
    assert_eq!(a.presented(), 0);
    drop(paused);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let frame = host.prepare(1, 1.).unwrap();
        host.presented(&frame).unwrap();
        let status = a.session.query_track(&a.track).unwrap();
        if status.milestones & MILESTONE_BUFFERED_ENDED != 0 {
            assert_eq!(status.last_presented_pts_us, 90_000);
            assert_eq!(playback(&a).0, 4);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed EOS did not finish: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn flushing_a_full_timed_pre_roll_queue_wakes_only_its_owner_and_keeps_the_channel() {
    let (p, mut host) = fixture();
    let mut a = Peer::timed(&p, 0, 0);
    let b = Peer::new(&p, 16);
    let old = prepare(&mut host, 2);
    for id in 2..=14 {
        send(
            &a.channel,
            0,
            id,
            10_000 + (id as i64 - 1) * 40_000,
            &STREAM[..652],
            true,
        );
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while a.session.query_track(&a.track).unwrap().last_media_id != 14 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    send(&b.channel, 0, 2, 50_000, &STREAM[652..661], false);
    b.decoded(50_000);
    a.session.flush(&a.track, 1).unwrap();
    send(&a.channel, 1, 15, 1_000_000, &STREAM[..652], true);
    a.decoded(1_000_000);
    assert_eq!(playback(&a).0, 0);
    assert!(host.presented(&old).unwrap());
    assert_eq!(a.presented(), 0);
    drop(old);
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    assert_eq!(
        a.session
            .query_track(&a.track)
            .unwrap()
            .last_presented_pts_us,
        1_000_000
    );
    assert_eq!(b.presented(), 2);
}

#[test]
fn timed_reordered_eos_preserves_future_pictures_and_requires_final_display() {
    let (p, mut host) = fixture();
    let mut a = Peer::timed(&p, 0, 2);
    let b = Peer::new(&p, 16);
    a.session.play(&a.track, 0, 200_000, 200_000).unwrap();
    for index in 3..6 {
        send_reordered(&a.channel, index);
    }
    a.decoded(120_000);
    assert_eq!(playback(&a), (1, 0));
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    a.decoded(200_000);
    assert_eq!(a.presented(), 0);
    // Decoder drain and elapsed clock time cannot establish physical display.
    std::thread::sleep(Duration::from_millis(260));
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_BUFFERED_ENDED,
        0
    );
    let final_frame = prepare(&mut host, 2);
    assert!(host.presented(&final_frame).unwrap());
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(status.last_presented_pts_us, 200_000);
    assert_ne!(status.milestones & MILESTONE_BUFFERED_ENDED, 0);
    assert_eq!(playback(&a), (4, 240_000));
    assert_eq!(b.presented(), 1);
}

#[test]
fn timed_resume_keeps_the_last_picture_and_its_remaining_duration() {
    let (p, mut host) = fixture();
    let mut a = Peer::mode(&p, 0, 0, TrackMode::Timed, 500_000);
    a.session.play(&a.track, 10_000, 0, 200_000).unwrap();
    let frame = prepare(&mut host, 1);
    host.presented(&frame).unwrap();
    a.session.pause(&a.track).unwrap();
    let frozen = playback(&a).1;
    let old_id = a.presented();
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    a.session.play(&a.track, frozen, 0, 200_000).unwrap();
    assert_eq!(a.presented(), 0);
    drop(frame);
    let resumed = prepare(&mut host, 1);
    assert!(host.presented(&resumed).unwrap());
    assert!(a.presented() > old_id);
    assert_eq!(
        a.session
            .query_track(&a.track)
            .unwrap()
            .last_presented_pts_us,
        10_000
    );
    std::thread::sleep(Duration::from_millis(550));
    let status = a.session.query_track(&a.track).unwrap();
    assert_ne!(status.milestones & MILESTONE_BUFFERED_ENDED, 0);
    assert!(
        playback(&a).1 >= frozen,
        "EOS did not rewind the paused clock"
    );
}

#[test]
fn new_play_rejects_an_unacknowledged_old_preview_and_preserves_a_peer() {
    let (p, mut host) = fixture();
    let mut a = Peer::timed(&p, 0, 0);
    let b = Peer::new(&p, 16);
    let old = prepare(&mut host, 2);
    a.session.play(&a.track, 10_000, 0, 200_000).unwrap();
    assert!(host.presented(&old).unwrap());
    assert_eq!((a.presented(), b.presented()), (0, 1));
    drop(old);
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    assert!(a.presented() > 0);
}

#[test]
fn timed_queue_budget_is_reserved_through_retired_images_and_invalid_play_is_atomic() {
    let mut config = PresenterConfig::terminal_with_overlay(MediaConfig::default());
    // ceil(60 Hz * 200 ms) + one queue boundary, two retained images and one decode image.
    config.media.aggregate_retained_bytes = 16 * 1024 + 64;
    let p = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            config,
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(p.clone(), "").unwrap();
    p.update_metrics(1, 32, 32, (1, 1));
    let mut a = Peer::timed(&p, 0, 0);
    assert!(a.session.play(&a.track, 10_000, 0, 200_001).is_err());
    assert_eq!(playback(&a).0, 0);
    let held = prepare(&mut host, 1);
    let mut probe = a.track.configuration().unwrap();
    probe.track_id = 0;
    assert!(!a.session.probe_track(&probe).unwrap().supported);
    a.session
        .destroy_track(&a.track, &Default::default())
        .unwrap();
    drop(prepare(&mut host, 0));
    assert!(!a.session.probe_track(&probe).unwrap().supported);
    drop(held);
    let support = a.session.probe_track(&probe).unwrap();
    assert!(support.supported);
    assert_eq!(support.selected_decoder, "native-ffmpeg-timed-srgb");
    probe.maximum_rate_millihertz = u64::MAX;
    probe.maximum_latency_us = u64::MAX;
    assert!(!a.session.probe_track(&probe).unwrap().supported);
}

#[test]
fn a_new_native_media_epoch_discards_delayed_decode_and_queued_timed_pictures() {
    let (p, mut host) = fixture();
    let a = Peer::timed(&p, 0, 2);
    let b = Peer::new(&p, 16);
    let old = prepare(&mut host, 2);
    // A greater epoch is independently ordered media, even without FLUSH.
    for index in 0..3 {
        send_reordered_at(&a.channel, index, 1, 1_000_000, 4);
    }
    a.decoded(1_000_000);
    assert_eq!(playback(&a).0, 0);
    assert!(host.presented(&old).unwrap());
    assert_eq!((a.presented(), b.presented()), (0, 1));
    drop(old);
    let fresh = prepare(&mut host, 2);
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        a.session
            .query_track(&a.track)
            .unwrap()
            .last_presented_pts_us,
        1_000_000
    );
    assert_eq!(b.presented(), 1);
}

#[test]
fn delayed_timed_output_keeps_the_surface_generation_that_admitted_its_packet() {
    let (p, mut host) = fixture();
    let mut a = Peer::timed(&p, 0, 2);
    let b = Peer::new(&p, 16);
    let old = prepare(&mut host, 2);
    let mut definition = a.surface.definition().unwrap();
    definition.logical_width = 32;
    a.session
        .update_surface(&a.surface, definition, &Default::default())
        .unwrap();
    send_reordered(&a.channel, 3);
    let deadline = Instant::now() + Duration::from_secs(3);
    while a.session.query_track(&a.track).unwrap().last_media_id != 4 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let peer_only = prepare(&mut host, 1);
    assert!(host.presented(&peer_only).unwrap());
    assert_eq!((a.presented(), b.presented()), (0, 1));
    drop(peer_only);
    // Packet four's new-generation picture becomes available on the next decode.
    // Packet three's delayed old-generation picture was not relabeled.
    send_reordered(&a.channel, 4);
    a.decoded(80_000);
    host.presented(&old).unwrap();
    assert_eq!(a.presented(), 0);
    drop(old);
    let fresh = prepare(&mut host, 2);
    assert!(host.presented(&fresh).unwrap());
    assert_eq!(
        a.session
            .query_track(&a.track)
            .unwrap()
            .last_presented_pts_us,
        80_000
    );
    assert_eq!(b.presented(), 1);
}

#[test]
fn displayed_video_uses_its_own_pts_even_after_a_new_picture_is_decoded() {
    let (p, mut host) = fixture();
    let a = Peer::new(&p, 0);
    let b = Peer::new(&p, 16);
    a.decoded(10_000);
    b.decoded(10_000);
    assert_eq!((a.presented(), b.presented()), (0, 0));
    let old = prepare(&mut host, 2);
    send(&a.channel, 0, 2, 50_000, &STREAM[652..661], false);
    a.decoded(50_000);
    assert!(host.presented(&old).unwrap());
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        (
            status.last_decoded_pts_us,
            status.last_presented_pts_us,
            status.last_presentation_id
        ),
        (50_000, 10_000, 1)
    );
    assert_eq!(b.presented(), 1);
    let current = prepare(&mut host, 2);
    assert!(!current.same_content(&old));
    assert!(current.same_input_context(&old));
    assert!(host.presented(&current).unwrap());
    assert_eq!(a.presented(), 2);
    drop((current, old));
    a.session.close().unwrap();
    let remaining = prepare(&mut host, 1);
    assert_eq!(
        remaining.rasters()[0]
            .surface
            .context()
            .session()
            .session_id(),
        b.session.info().session_id
    );
    assert!(host.presented(&remaining).unwrap());
    assert_eq!(b.presented(), 1);
}

#[test]
fn live_reordered_video_drains_to_its_latest_pts_and_only_display_ends_the_owner() {
    let (p, mut host) = fixture();
    let a = Peer::configured(&p, 0, 2);
    let b = Peer::configured(&p, 16, 2);
    a.decoded(0);
    b.decoded(0);
    let initial = prepare(&mut host, 2);
    assert!(host.presented(&initial).unwrap());
    drop(initial);
    for (index, pts) in [(3, 40_000), (4, 80_000), (5, 120_000)] {
        send_reordered(&a.channel, index);
        a.decoded(pts);
        let frame = prepare(&mut host, 2);
        assert!(host.presented(&frame).unwrap());
    }
    let old = prepare(&mut host, 2);
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    a.decoded(200_000);
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(status.last_presented_pts_us, 120_000);
    assert_eq!(status.milestones & MILESTONE_BUFFERED_ENDED, 0);
    assert!(host.presented(&old).unwrap());
    assert_eq!(
        a.presented(),
        4,
        "old display did not acknowledge delayed EOS output"
    );
    let final_frame = prepare(&mut host, 2);
    assert!(host.presented(&final_frame).unwrap());
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        (
            status.last_decoded_pts_us,
            status.last_presented_pts_us,
            status.last_presentation_id
        ),
        (200_000, 200_000, 5)
    );
    assert_ne!(status.milestones & MILESTONE_BUFFERED_ENDED, 0);
    assert_eq!(b.presented(), 1);
    assert_eq!(
        b.session.query_track(&b.track).unwrap().last_decoded_pts_us,
        0
    );
    drop((old, final_frame));
    send_reordered(&b.channel, 3);
    b.decoded(40_000);
    let remaining = prepare(&mut host, 2);
    assert!(host.presented(&remaining).unwrap());
    assert_eq!(b.presented(), 2);
}

#[test]
fn reordered_decode_reserves_the_whole_batch_through_retired_gui_receipts() {
    let mut config = PresenterConfig::terminal_with_overlay(MediaConfig::default());
    // Two retained images, a depth-two batch of three images, and one RGBA row.
    config.media.aggregate_retained_bytes = 5 * 1024 + 64;
    let p = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            config,
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(p.clone(), "").unwrap();
    p.update_metrics(1, 32, 32, (1, 1));
    let mut a = Peer::configured(&p, 0, 2);
    a.decoded(0);
    let held = prepare(&mut host, 1);
    let mut probe = a.track.configuration().unwrap();
    probe.track_id = 0;
    assert!(!a.session.probe_track(&probe).unwrap().supported);
    a.session
        .destroy_track(&a.track, &Default::default())
        .unwrap();
    drop(prepare(&mut host, 0));
    assert!(!a.session.probe_track(&probe).unwrap().supported);
    drop(held);
    assert!(a.session.probe_track(&probe).unwrap().supported);
    if let KindConfiguration::Video(video) = &mut probe.kind {
        video.maximum_reorder_depth = 3;
    }
    assert!(
        !a.session.probe_track(&probe).unwrap().supported,
        "the larger batch requires another complete frame"
    );
}

#[test]
fn replacing_a_reordered_channel_retires_delayed_output_without_touching_the_peer() {
    let (p, mut host) = fixture();
    let mut a = Peer::configured(&p, 0, 2);
    let b = Peer::configured(&p, 16, 2);
    let old = prepare(&mut host, 2);
    a.session
        .advance_channel(&a.track, 1, &Default::default())
        .unwrap();
    let replacement = a.session.open_track_channel(&a.track).unwrap();
    for index in 0..3 {
        send_reordered_at(&replacement, index, 1, 1_000_000, 4);
    }
    a.decoded(1_000_000);
    assert!(host.presented(&old).unwrap());
    assert_eq!((a.presented(), b.presented()), (0, 1));
    send_reordered(&b.channel, 3);
    b.decoded(40_000);
    drop(old);
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        (status.last_presented_pts_us, status.last_presentation_id),
        (1_000_000, 2)
    );
    assert_eq!(b.presented(), 2);
}
#[test]
fn native_video_replacement_fences_receipts_and_preserves_the_peer_with_reused_ids() {
    let (p, mut host) = fixture();
    let mut a = Peer::new(&p, 0);
    let peer = Peer::new(&p, 16);
    let first = prepare(&mut host, 2);
    assert!(host.presented(&first).unwrap());
    let pending = prepare(&mut host, 2);
    a.session
        .advance_channel(&a.track, 1, &Default::default())
        .unwrap();
    assert_eq!(a.presented(), 0);
    let replacement = a.session.open_track_channel(&a.track).unwrap();
    send(&replacement, 1, 2, 90_000, &STREAM[..652], true);
    a.decoded(90_000);
    assert!(host.presented(&pending).unwrap());
    assert_eq!(a.presented(), 0);
    assert_eq!(peer.presented(), 1);
    drop((first, pending));
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    assert_eq!(a.presented(), 2);
}
#[test]
fn clean_video_eos_retains_pixels_and_requires_a_real_display_receipt() {
    let (p, mut host) = fixture();
    let a = Peer::new(&p, 0);
    a.decoded(10_000);
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    assert_eq!(a.presented(), 0);
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_BUFFERED_ENDED,
        0
    );
    let frame = prepare(&mut host, 1);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(a.presented(), 1);
    assert_ne!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_BUFFERED_ENDED,
        0
    );
    let target = host.input_target(&frame).unwrap();
    assert_eq!(
        host.dispatch_input(&target, presenter::VectorInputEvent::Text("unowned".into()))
            .unwrap(),
        presenter::VectorInputResult::Rejected
    );
}
#[test]
fn invalid_video_fails_only_its_owner_while_the_peer_keeps_decoding() {
    let (p, mut host) = fixture();
    let a = Peer::new(&p, 0);
    let b = Peer::new(&p, 16);
    a.decoded(10_000);
    b.decoded(10_000);
    let first = prepare(&mut host, 2);
    assert!(host.presented(&first).unwrap());
    send(&a.channel, 0, 2, 50_000, &[0xff; 32], false);
    a.session
        .wait_track(
            &a.track,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_TRACK_LOST),
            3_000_000,
        )
        .unwrap();
    send(&b.channel, 0, 2, 50_000, &STREAM[652..661], false);
    b.decoded(50_000);
    drop(first);
    let remaining = prepare(&mut host, 1);
    assert!(host.presented(&remaining).unwrap());
    assert_eq!(b.presented(), 2);
}

#[test]
fn hidden_video_placement_cannot_acknowledge_an_old_receipt() {
    let (p, mut host) = fixture();
    let mut a = Peer::new(&p, 0);
    let b = Peer::new(&p, 16);
    a.decoded(10_000);
    b.decoded(10_000);
    let old = prepare(&mut host, 2);
    a.node.visible = false;
    a.session.update_node(&a.node, &Default::default()).unwrap();
    assert!(host.presented(&old).unwrap());
    assert_eq!((a.presented(), b.presented()), (0, 1));
    drop(old);
    assert_eq!(prepare(&mut host, 1).rasters().len(), 1);
    a.node.visible = true;
    a.node.opacity = 32768;
    a.session.update_node(&a.node, &Default::default()).unwrap();
    let shown = prepare(&mut host, 2);
    assert_eq!(shown.rasters()[0].opacity, 32768);
    assert!(host.presented(&shown).unwrap());
    assert_eq!((a.presented(), b.presented()), (1, 1));
}
#[test]
fn retired_video_images_hold_their_budget_until_every_receipt_retires() {
    let mut config = PresenterConfig::terminal_with_overlay(MediaConfig::default());
    config.media.aggregate_retained_bytes = 3 * 1024 + 64;
    let p = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            config,
        )
        .unwrap(),
    );
    let mut host = NativeVectorHost::attach(p.clone(), "").unwrap();
    p.update_metrics(1, 32, 32, (1, 1));
    let mut a = Peer::new(&p, 0);
    a.decoded(10_000);
    let held = prepare(&mut host, 1);
    let mut probe = a.track.configuration().unwrap();
    probe.track_id = 0;
    assert!(!a.session.probe_track(&probe).unwrap().supported);
    a.session
        .destroy_track(&a.track, &Default::default())
        .unwrap();
    drop(prepare(&mut host, 0));
    assert!(
        !a.session.probe_track(&probe).unwrap().supported,
        "GUI/GPU references still own retired pixels"
    );
    drop(held);
    assert!(a.session.probe_track(&probe).unwrap().supported);
    probe.mode = TrackMode::Timed;
    assert!(!a.session.probe_track(&probe).unwrap().supported);
    probe.mode = TrackMode::Live;
    if let KindConfiguration::Video(video) = &mut probe.kind {
        video.maximum_reorder_depth = 1;
    }
    assert!(!a.session.probe_track(&probe).unwrap().supported);
}
#[test]
fn dropping_the_native_host_retires_video_channels_without_reenabling_synthetic_display() {
    let (p, host) = fixture();
    let mut a = Peer::new(&p, 0);
    a.decoded(10_000);
    drop(host);
    a.session
        .wait_track(
            &a.track,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_TRACK_LOST),
            3_000_000,
        )
        .unwrap();
    assert_eq!(a.presented(), 0);
    let mut probe = a.track.configuration().unwrap();
    probe.track_id = 0;
    assert!(!a.session.probe_track(&probe).unwrap().supported);
}

#[test]
fn saturated_output_does_not_block_control_or_a_peer_and_cannot_publish_after_replacement() {
    let (p, mut host) = fixture();
    let mut a = Peer::new(&p, 0);
    let b = Peer::new(&p, 16);
    a.decoded(10_000);
    b.decoded(10_000);
    let first = prepare(&mut host, 2);
    send(&a.channel, 0, 2, 50_000, &STREAM[652..661], false);
    a.decoded(50_000);
    let second = prepare(&mut host, 2);
    send(&a.channel, 0, 3, 90_000, &STREAM[661..], false);
    let deadline = Instant::now() + Duration::from_secs(3);
    while a.session.query_track(&a.track).unwrap().last_media_id != 3 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    send(&b.channel, 0, 2, 50_000, &STREAM[652..661], false);
    b.decoded(50_000);
    a.session
        .advance_channel(&a.track, 1, &Default::default())
        .unwrap();
    let replacement = a.session.open_track_channel(&a.track).unwrap();
    send(&replacement, 1, 4, 130_000, &STREAM[..652], true);
    assert!(host.presented(&second).unwrap());
    assert_eq!(a.presented(), 0);
    drop((first, second));
    a.decoded(130_000);
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    assert_eq!(
        a.presented(),
        3,
        "the cancelled output did not acquire a new source's receipt"
    );
    assert_eq!(b.presented(), 2);
}

#[test]
fn flush_retires_saturated_decode_and_recovers_on_the_same_channel_without_touching_a_peer() {
    let (p, mut host) = fixture();
    let mut a = Peer::new(&p, 0);
    let b = Peer::new(&p, 16);
    a.decoded(10_000);
    b.decoded(10_000);
    let first = prepare(&mut host, 2);
    send(&a.channel, 0, 2, 50_000, &STREAM[652..661], false);
    a.decoded(50_000);
    let second = prepare(&mut host, 2);
    send(&a.channel, 0, 3, 90_000, &STREAM[661..], false);
    let deadline = Instant::now() + Duration::from_secs(3);
    while a.session.query_track(&a.track).unwrap().last_media_id != 3 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let generation = a.track.channel_generation();
    a.session.flush(&a.track, 1).unwrap();
    assert_eq!(a.track.channel_generation(), generation);
    let flushed = a.session.query_track(&a.track).unwrap();
    assert_eq!(
        flushed.milestones & (MILESTONE_OUTPUT_READY | MILESTONE_BUFFERED_ENDED),
        0
    );
    assert!(host.presented(&second).unwrap());
    assert_eq!((a.presented(), b.presented()), (0, 1));
    send(&b.channel, 0, 2, 50_000, &STREAM[652..661], false);
    b.decoded(50_000);
    drop((first, second));
    send(&a.channel, 1, 4, 130_000, &STREAM[..652], true);
    a.decoded(130_000);
    let recovered = prepare(&mut host, 2);
    assert!(host.presented(&recovered).unwrap());
    let status = a.session.query_track(&a.track).unwrap();
    assert_eq!(status.last_presentation_id, 3);
    assert_eq!(status.last_presented_pts_us, 130_000);
    assert_eq!(b.presented(), 2);
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    assert_ne!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_BUFFERED_ENDED,
        0
    );
}

#[test]
fn an_old_clean_eos_receipt_cannot_acknowledge_a_recreated_surface_with_identical_ids() {
    let (p, mut host) = fixture();
    let mut a = Peer::new(&p, 0);
    let b = Peer::new(&p, 16);
    a.decoded(10_000);
    b.decoded(10_000);
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    let old = prepare(&mut host, 2);
    let definition = a.surface.definition().unwrap();
    let configuration = a.track.configuration().unwrap();
    a.session
        .destroy_surface(&a.surface, &Default::default())
        .unwrap();
    let surface = a
        .session
        .create_surface(definition, &Default::default())
        .unwrap();
    let track = a
        .session
        .create_track(configuration, &Default::default())
        .unwrap();
    let channel = a.session.open_track_channel(&track).unwrap();
    send(&channel, 0, 1, 90_000, &STREAM[..652], true);
    a.session
        .wait_track(
            &track,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_OUTPUT_READY),
            3_000_000,
        )
        .unwrap();
    a.session
        .activate_tracks(
            &surface,
            &[SlotBinding {
                slot: 1,
                track_id: 1,
                expected_channel_generation: track.channel_generation(),
                required_milestone: MILESTONE_OUTPUT_READY,
            }],
            &Default::default(),
        )
        .unwrap();
    a.session.create_node(&a.node, &Default::default()).unwrap();
    channel.eos().unwrap();
    channel.close().unwrap();
    a.session
        .wait_track(&track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    assert!(host.presented(&old).unwrap());
    assert_eq!(
        a.session.query_track(&track).unwrap().last_presentation_id,
        0,
        "an old detached image cannot acknowledge a different source lifetime"
    );
    assert_eq!(b.presented(), 1);
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    assert_eq!(
        a.session.query_track(&track).unwrap().last_presented_pts_us,
        90_000
    );
}
