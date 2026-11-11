//! Real authenticated raster scene nodes beneath native vector overlays.
#![cfg(all(feature = "presenter", feature = "vector-render"))]

// Rust guideline compliant 2026-10-07
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use vivid_protocol::{
    cbor::Value,
    track::{KindConfiguration, MILESTONE_OUTPUT_READY, MILESTONE_PRESENTED},
};
use vivid_sdk::{
    presenter::{
        MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VectorFrame, VirtualVivid,
    },
    *,
};

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

#[test]
fn native_track_probes_match_compiled_codecs_and_renderer_dimensions() {
    let (presenter, _host) = fixture();
    let secret = presenter.issue_pane_capability(1).unwrap();
    let mut session = Session::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..Default::default()
    })
    .unwrap();
    let mut configuration = TrackConfiguration {
        direction: Default::default(),
        context_id: 1,
        surface_id: 1,
        track_id: 0,
        slot: 4,
        mode: TrackMode::Live,
        lane: LaneClass::Bulk,
        maximum_record_body: 65536,
        maximum_rate_millihertz: 60_000,
        maximum_encoded_bits_per_second: 1_000_000,
        maximum_records_per_second: 60,
        maximum_inflight_body_bytes: 65536,
        retained_pixel_charge: 4,
        kind: KindConfiguration::EncodedImage(ImageConfiguration {
            encoding: IMAGE_ENCODING_PNG,
            width: 2,
            height: 2,
            encoded_length: 24,
            sha256: None,
            cache_lookup: false,
        }),
        target_latency_us: 0,
        maximum_latency_us: 0,
    };
    assert_eq!(
        session.probe_track(&configuration).unwrap().supported,
        cfg!(feature = "vector-images")
    );
    configuration.slot = 1;
    configuration.kind = KindConfiguration::Raster(RasterConfiguration {
        width: 2,
        height: 2,
        alpha_mode: 1,
        delta_enabled: false,
        maximum_delta_operations: 1,
        zstd_enabled: false,
    });
    assert!(session.probe_track(&configuration).unwrap().supported);
    configuration.slot = 4;
    configuration.retained_pixel_charge = 8192 * 8192;
    configuration.kind = KindConfiguration::EncodedImage(ImageConfiguration {
        encoding: IMAGE_ENCODING_PNG,
        width: 8192,
        height: 8192,
        encoded_length: 24,
        sha256: None,
        cache_lookup: false,
    });
    assert!(
        !session.probe_track(&configuration).unwrap().supported,
        "native probe cannot promise an image beyond renderer allocation limits"
    );
}
struct Peer {
    session: Session,
    track: Track,
    channel: TrackChannel,
    node: SceneNode,
}
impl Peer {
    fn new(p: &VirtualVivid, x: i64, color: [u8; 4]) -> Self {
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
                    logical_width: 2,
                    logical_height: 2,
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
                    mode: TrackMode::Live,
                    lane: LaneClass::Bulk,
                    maximum_record_body: 4096,
                    maximum_rate_millihertz: 60_000,
                    maximum_encoded_bits_per_second: 1_000_000,
                    maximum_records_per_second: 60,
                    maximum_inflight_body_bytes: 4096,
                    kind: KindConfiguration::Raster(RasterConfiguration {
                        width: 2,
                        height: 2,
                        alpha_mode: 1,
                        delta_enabled: true,
                        maximum_delta_operations: 4,
                        zstd_enabled: false,
                    }),
                    target_latency_us: 0,
                    maximum_latency_us: 0,
                    retained_pixel_charge: 4,
                },
                &Default::default(),
            )
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        channel.send_raster(0, 1, &color.repeat(4), false).unwrap();
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
            track,
            channel,
            node,
        }
    }
    fn accepted(&self, id: u64) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.session.query_track(&self.track).unwrap().last_media_id != id {
            assert!(Instant::now() < deadline, "raster never accepted");
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

#[test]
fn raster_display_receipts_are_exact_and_peer_cleanup_keeps_reused_ids_independent() {
    let (presenter, mut host) = fixture();
    let a = Peer::new(&presenter, 0, [255, 0, 0, 255]);
    let b = Peer::new(&presenter, 16, [0, 0, 255, 255]);
    assert_eq!(a.presented(), 0);
    assert_eq!(b.presented(), 0);
    assert_eq!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    let old = prepare(&mut host, 2);
    assert_eq!(old.rasters()[1].bounds.origin.x.get(), 16.);
    a.channel
        .send_raster(0, 2, &[0, 255, 0, 255].repeat(4), false)
        .unwrap();
    a.accepted(2);
    assert!(host.presented(&old).unwrap());
    assert_eq!(
        a.presented(),
        0,
        "old pixels cannot mark the replacement frame presented"
    );
    assert_eq!(b.presented(), 1, "peer has independent presentation proof");
    let current = prepare(&mut host, 2);
    assert!(!old.same_content(&current));
    assert!(
        old.same_input_context(&current),
        "raster pixels do not become vector input authority"
    );
    assert!(host.presented(&current).unwrap());
    assert_eq!(a.presented(), 2);
    drop(old);
    drop(current);
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
fn raster_delta_and_node_visibility_change_content_without_reviving_old_placement() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0, [255, 0, 0, 255]);
    let before = prepare(&mut host, 1);
    a.channel
        .send_raster_delta(
            0,
            2,
            1,
            0,
            0,
            &[RasterDeltaOperation::Overwrite {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                rgba: &[0, 255, 0, 255],
            }],
            false,
        )
        .unwrap();
    a.accepted(2);
    let after = prepare(&mut host, 1);
    assert!(!before.same_content(&after));
    drop(before);
    a.node.visible = false;
    a.session.update_node(&a.node, &Default::default()).unwrap();
    assert!(host.presented(&after).unwrap());
    assert_eq!(
        a.presented(),
        0,
        "a hidden node cannot acknowledge its old display receipt"
    );
    drop(after);
    assert!(prepare(&mut host, 0).rasters().is_empty());
    a.node.visible = true;
    a.node.opacity = 32768;
    a.node.fit = Fit::Contain;
    a.session.update_node(&a.node, &Default::default()).unwrap();
    let shown = prepare(&mut host, 1);
    assert_eq!(shown.rasters()[0].opacity, 32768);
    assert!(host.presented(&shown).unwrap());
    assert_eq!(a.presented(), 2);
}

#[test]
fn raster_eos_keeps_retained_pixels_after_its_channel_detaches() {
    let (presenter, mut host) = fixture();
    let a = Peer::new(&presenter, 0, [0, 0, 255, 255]);
    a.channel.eos().unwrap();
    a.channel.close().unwrap();
    a.session
        .wait_track(&a.track, TrackWaitCondition::ChannelClosed, None, 3_000_000)
        .unwrap();
    let frame = prepare(&mut host, 1);
    assert_eq!(a.presented(), 0);
    assert!(frame.drawings().is_empty());
    assert!(host.presented(&frame).unwrap());
    assert_eq!(a.presented(), 1);
    let target = host.input_target(&frame).unwrap();
    assert_eq!(
        host.dispatch_input(&target, presenter::VectorInputEvent::Text("unowned".into()))
            .unwrap(),
        presenter::VectorInputResult::Rejected
    );
}

#[test]
fn raster_channel_replacement_retires_old_presentation_proof_without_touching_peer() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0, [255, 0, 0, 255]);
    let peer = Peer::new(&presenter, 16, [0, 255, 0, 255]);
    let old = prepare(&mut host, 2);
    assert!(host.presented(&old).unwrap());
    assert_eq!(a.presented(), 1);
    let pending = prepare(&mut host, 2);
    a.session
        .advance_channel(&a.track, 1, &Default::default())
        .unwrap();
    assert_eq!(a.presented(), 0, "a new channel has no display proof");
    let replacement = a.session.open_track_channel(&a.track).unwrap();
    replacement
        .send_raster(1, 2, &[0, 0, 255, 255].repeat(4), false)
        .unwrap();
    a.accepted(2);
    assert!(host.presented(&pending).unwrap());
    assert_eq!(
        a.presented(),
        0,
        "old receipt cannot acknowledge the replacement channel"
    );
    assert_eq!(peer.presented(), 1);
    drop(old);
    drop(pending);
    let current = prepare(&mut host, 2);
    assert!(host.presented(&current).unwrap());
    assert_eq!(a.presented(), 2);
    assert_ne!(
        a.session.query_track(&a.track).unwrap().milestones & MILESTONE_PRESENTED,
        0
    );
    assert_eq!(peer.presented(), 1);
}

#[cfg(feature = "vector-offscreen")]
fn pixels(
    gpu: &mut vector_renderer::compositor::offscreen::Offscreen,
    frame: &VectorFrame,
) -> vector_renderer::compositor::offscreen::Pixels {
    let (width, height) = frame.dimensions();
    assert!(
        gpu.render_mixed(frame.drawings(), frame.rasters(), width, height)
            .unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pixels) = gpu.poll().unwrap() {
            return pixels;
        }
        assert!(Instant::now() < deadline, "mixed GPU readback timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(feature = "vector-offscreen")]
#[test]
#[ignore = "requires a real native Metal/Vulkan/DX12 adapter"]
fn native_gpu_composes_raster_deltas_fit_opacity_clipping_and_vector_windows() {
    let (presenter, mut host) = fixture();
    let mut a = Peer::new(&presenter, 0, [0, 0, 255, 255]);
    let b = Peer::new(&presenter, 16, [0, 255, 0, 255]);
    let secret = presenter.issue_pane_capability(1).unwrap();
    let overlays = OverlaySession::connect(ProducerConfig {
        endpoint_control: Some(presenter.endpoint()),
        endpoint_interactive: Some(presenter.endpoint()),
        endpoint_bulk: Some(presenter.endpoint()),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..Default::default()
    })
    .unwrap();
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            overlay::Rect::new(4., 4., 8., 8.).unwrap(),
            overlay::WindowMode::Floating,
        ))
        .unwrap();
    let mut canvas = overlay::Canvas::new();
    canvas
        .fill(
            overlay::Path::rectangle(overlay::Rect::new(0., 0., 8., 8.).unwrap()).unwrap(),
            overlay::Brush::Solid(overlay::Color(0xff0000ff)),
        )
        .unwrap();
    let submitted = window.submit(canvas).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while window.reconcile().unwrap().accepted_revision != submitted.revision() {
        assert!(Instant::now() < deadline, "vector scene not accepted");
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut gpu =
        pollster::block_on(vector_renderer::compositor::offscreen::Offscreen::new()).unwrap();
    let frame = prepare(&mut host, 2);
    let image = pixels(&mut gpu, &frame);
    let sample =
        |image: &vector_renderer::compositor::offscreen::Pixels, x: usize, y: usize| -> [u8; 4] {
            image.rgba[((y * 32 + x) * 4)..((y * 32 + x) * 4 + 4)]
                .try_into()
                .unwrap()
        };
    assert_eq!(sample(&image, 1, 1), [0, 0, 255, 255]);
    assert_eq!(
        sample(&image, 6, 6),
        [255, 0, 0, 255],
        "vector window is above raster pixels"
    );
    assert_eq!(sample(&image, 18, 1), [0, 255, 0, 255]);
    assert_eq!(sample(&image, 31, 31), [0; 4]);
    let mut invalid = frame.rasters().to_vec();
    invalid[0].scale = f64::NAN;
    assert!(
        gpu.render_mixed(frame.drawings(), &invalid, 32, 32)
            .is_err()
    );
    assert!(
        !gpu.render_mixed(frame.drawings(), frame.rasters(), 32, 32)
            .unwrap(),
        "a rejected raster cannot mutate the GPU cache"
    );
    assert!(host.presented(&frame).unwrap());
    a.channel
        .send_raster_delta(
            0,
            2,
            1,
            0,
            0,
            &[RasterDeltaOperation::Overwrite {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                rgba: &[0, 255, 255, 255],
            }],
            false,
        )
        .unwrap();
    a.accepted(2);
    let updated = prepare(&mut host, 2);
    let image = pixels(&mut gpu, &updated);
    assert_eq!(sample(&image, 1, 1), [0, 255, 255, 255]);
    assert_eq!(
        a.presented(),
        1,
        "GPU submission alone cannot acknowledge the delta"
    );
    assert!(host.presented(&updated).unwrap());
    assert_eq!(a.presented(), 2);
    drop(frame);
    drop(updated);
    a.node.geometry[4].1 = Value::Unsigned(8 << 32);
    a.node.fit = Fit::Contain;
    a.node.opacity = 32768;
    a.node.clip = Some(vec![
        (0, Value::Unsigned(6 << 32)),
        (1, Value::Unsigned(0)),
        (2, Value::Unsigned(2 << 32)),
        (3, Value::Unsigned(8 << 32)),
    ]);
    a.session.update_node(&a.node, &Default::default()).unwrap();
    let clipped = prepare(&mut host, 2);
    let image = pixels(&mut gpu, &clipped);
    assert_eq!(
        sample(&image, 1, 1),
        [0; 4],
        "contain-fit and clip leave transparent margins"
    );
    let cyan = sample(&image, 6, 1);
    assert_eq!(&cyan[..3], &[0, 255, 255]);
    assert!(
        cyan[3].abs_diff(128) <= 1,
        "node opacity was not applied: {cyan:?}"
    );
    assert_eq!(
        sample(&image, 18, 1),
        [0, 255, 0, 255],
        "peer remains unchanged"
    );
    assert_eq!(b.presented(), 1);
}
