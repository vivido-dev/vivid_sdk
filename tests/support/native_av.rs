//! Shared authenticated native A/V producers for baseline and private conformance tests.

// Rust guideline compliant 2026-10-07
use super::sdk::{
    presenter::{MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VirtualVivid},
    *,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use vivid_protocol::{cbor::Value, media, track::MILESTONE_CHANNEL_ACCEPTED};

const STREAM: &[u8] = include_bytes!("../../video/fixtures/red.h264");

pub(crate) fn fixture() -> (Arc<VirtualVivid>, NativeVectorHost) {
    fixture_device(true)
}
pub(crate) fn fixture_device(test: bool) -> (Arc<VirtualVivid>, NativeVectorHost) {
    let p = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
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

pub(crate) struct Peer {
    pub(crate) session: Session,
    pub(crate) audio: Track,
    pub(crate) video: Track,
    pub(crate) ac: TrackChannel,
    pub(crate) vc: TrackChannel,
}
impl Peer {
    pub(crate) fn new(p: &VirtualVivid, x: u32) -> Self {
        Self::mode(p, x, TrackMode::Timed)
    }
    pub(crate) fn live(p: &VirtualVivid, x: u32) -> Self {
        Self::mode(p, x, TrackMode::Live)
    }
    pub(crate) fn mode(p: &VirtualVivid, x: u32, mode: TrackMode) -> Self {
        Self::pane(p, 1, x, mode)
    }
    pub(crate) fn pane(p: &VirtualVivid, pane: u64, x: u32, mode: TrackMode) -> Self {
        let secret = p.issue_pane_capability(pane).unwrap();
        Self::configured(
            ProducerConfig {
                endpoint_control: Some(p.endpoint()),
                authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
                ..Default::default()
            },
            x,
            mode,
        )
    }
    pub(crate) fn configured(config: ProducerConfig, x: u32, mode: TrackMode) -> Self {
        let mut session = Session::connect(config).unwrap();
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
        let mut config = TrackConfiguration {
            direction: Default::default(),
            context_id: 1,
            surface_id: 1,
            track_id: 1,
            slot: SLOT_AUDIO,
            mode,
            lane: LaneClass::Realtime,
            maximum_record_body: 4096,
            maximum_rate_millihertz: 50_000,
            maximum_encoded_bits_per_second: 2_000_000,
            maximum_records_per_second: 50,
            maximum_inflight_body_bytes: 8192,
            target_latency_us: 20_000,
            maximum_latency_us: 2_000_000,
            retained_pixel_charge: 0,
            kind: KindConfiguration::Audio(AudioConfiguration {
                codec: "pcm_s16le".into(),
                packetization: "pcm-packet-v1".into(),
                extradata: vec![],
                sample_rate: 48_000,
                channels: 2,
                channel_mask: 3,
                maximum_access_unit_bytes: 3840,
                codec_string: None,
            }),
        };
        let audio = session
            .create_track(config.clone(), &Default::default())
            .unwrap();
        config.track_id = 2;
        config.slot = SLOT_PRIMARY_VIDEO;
        config.maximum_rate_millihertz = 25_000;
        config.maximum_latency_us = 250_000;
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
        let video = session.create_track(config, &Default::default()).unwrap();
        let ac = session.open_track_channel(&audio).unwrap();
        let vc = session.open_track_channel(&video).unwrap();
        session
            .activate_tracks(
                &surface,
                &[(&audio, SLOT_AUDIO), (&video, SLOT_PRIMARY_VIDEO)].map(|(t, slot)| {
                    SlotBinding {
                        slot,
                        track_id: t.configuration().unwrap().track_id,
                        expected_channel_generation: t.channel_generation(),
                        required_milestone: MILESTONE_CHANNEL_ACCEPTED,
                    }
                }),
                &Default::default(),
            )
            .unwrap();
        session
            .create_node(
                &SceneNode {
                    owning_context_id: 1,
                    node_id: 1,
                    surface_context_id: 1,
                    surface_id: 1,
                    geometry: vec![
                        (0, Value::Unsigned(1)),
                        (1, Value::Unsigned(u64::from(x) << 32)),
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
                },
                &Default::default(),
            )
            .unwrap();
        Self {
            session,
            audio,
            video,
            ac,
            vc,
        }
    }
    pub(crate) fn audio(&self, epoch: u32, first: u64, origin: i64, count: u32) {
        let data = [0, 8].repeat(1920);
        for n in 0..count {
            self.ac
                .send_audio(media::AudioPacket {
                    epoch,
                    packet_id: first + u64::from(n),
                    pts_us: origin + i64::from(n) * 20_000,
                    dts_us: origin + i64::from(n) * 20_000,
                    duration_us: 0,
                    trim_start_samples: 0,
                    trim_end_samples: 0,
                    data: &data,
                })
                .unwrap();
        }
    }
    pub(crate) fn video(&self, epoch: u32, id: u64, pts: i64) {
        self.vc
            .send_video(media::VideoPacket {
                epoch,
                packet_id: id,
                key: true,
                pts_us: pts,
                dts_us: pts,
                duration_us: 40_000,
                data: &STREAM[..652],
            })
            .unwrap();
    }
    pub(crate) fn status(&self, t: &Track) -> TrackStatus {
        self.session.query_track(t).unwrap()
    }
    pub(crate) fn wait(&self, t: &Track, bits: u64) {
        self.session
            .wait_track(t, TrackWaitCondition::MilestoneSet, Some(bits), 5_000_000)
            .unwrap();
    }
}

pub(crate) fn clock(p: &Peer, t: &Track) -> (u64, i64) {
    let s = p.status(t);
    let f = s.playback_state.expect("playback clock");
    let field = |k| &f.iter().find(|(key, _)| *key == k).unwrap().1;
    (field(3).as_u64().unwrap(), field(4).as_i64().unwrap())
}
pub(crate) fn until(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < until, "A/V condition timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
pub(crate) fn display(host: &mut NativeVectorHost) {
    let _ = host.poll();
    let f = host.prepare(1, 1.).unwrap();
    host.presented(&f).unwrap();
}
