//! Encoded-image decoding and exact display receipts over authenticated sockets.
#![cfg(all(feature = "presenter", feature = "vector-images"))]

// Rust guideline compliant 2026-10-07
use image::ImageEncoder as _;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use vivid_protocol::track::{
    KindConfiguration, MILESTONE_FIRST_MEDIA, MILESTONE_OUTPUT_READY, MILESTONE_PRESENTED,
};
use vivid_sdk::{
    presenter::{
        MediaConfig, NativeVectorHost, PresenterConfig, SocketListener, VectorFrame, VirtualVivid,
    },
    *,
};

fn fixture() -> (Arc<VirtualVivid>, NativeVectorHost) {
    let p = Arc::new(
        VirtualVivid::start_configured_eventless(
            SocketListener::bind("tcp:127.0.0.1:0").unwrap(),
            PresenterConfig::terminal_with_overlay(MediaConfig::default()),
        )
        .unwrap(),
    );
    let host = NativeVectorHost::attach(p.clone(), "").unwrap();
    p.update_metrics(1, 32, 32, (1, 1));
    (p, host)
}
fn config(p: &VirtualVivid) -> ProducerConfig {
    let secret = p.issue_pane_capability(1).unwrap();
    ProducerConfig {
        endpoint_control: Some(p.endpoint()),
        endpoint_bulk: Some(p.endpoint()),
        endpoint_interactive: Some(p.endpoint()),
        authentication: ProducerAuthentication::root_hex(&secret).unwrap(),
        ..Default::default()
    }
}
fn encoded(jpeg: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if jpeg {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .write_image(&[0, 255, 0].repeat(4), 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
    } else {
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &[0, 0, 255, 255].repeat(4),
                2,
                2,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
    }
    bytes
}
struct Peer {
    session: Session,
    surface: Surface,
    track: Track,
    _channel: TrackChannel,
}
impl Peer {
    fn new(p: &VirtualVivid, body: &[u8], image: ImageConfiguration, x: i64) -> Self {
        let mut peer = Self::staged(p, body, image);
        peer.activate(x, MILESTONE_FIRST_MEDIA);
        peer
    }
    fn staged(p: &VirtualVivid, body: &[u8], image: ImageConfiguration) -> Self {
        let mut session = Session::connect(config(p)).unwrap();
        let surface = session
            .create_surface(
                SurfaceDefinition {
                    context_id: 1,
                    surface_id: 1,
                    semantic_profile: GENERIC_CONTENT.into(),
                    coordinate_model: CoordinateModel::DesktopLogicalPixels,
                    logical_width: u64::from(image.width),
                    logical_height: u64::from(image.height),
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
        let length = image.encoded_length;
        let track = session
            .create_track(
                TrackConfiguration {
                    direction: Default::default(),
                    context_id: 1,
                    surface_id: 1,
                    track_id: 1,
                    slot: 4,
                    mode: TrackMode::Live,
                    lane: LaneClass::Bulk,
                    maximum_record_body: length,
                    maximum_rate_millihertz: 1000,
                    maximum_encoded_bits_per_second: u64::from(length) * 8,
                    maximum_records_per_second: 1,
                    maximum_inflight_body_bytes: u64::from(length),
                    retained_pixel_charge: u64::from(image.width) * u64::from(image.height),
                    kind: KindConfiguration::EncodedImage(image),
                    target_latency_us: 0,
                    maximum_latency_us: 0,
                },
                &Default::default(),
            )
            .unwrap();
        let channel = session.open_track_channel(&track).unwrap();
        channel.send_image(body).unwrap();
        session
            .wait_track(
                &track,
                TrackWaitCondition::MilestoneSet,
                Some(MILESTONE_FIRST_MEDIA),
                3_000_000,
            )
            .unwrap();
        Self {
            session,
            surface,
            track,
            _channel: channel,
        }
    }
    fn activate(&mut self, x: i64, milestone: u64) {
        self.session
            .activate_tracks(
                &self.surface,
                &[SlotBinding {
                    slot: 4,
                    track_id: self.track.id(),
                    expected_channel_generation: self.track.channel_generation(),
                    required_milestone: milestone,
                }],
                &Default::default(),
            )
            .unwrap();
        self.session
            .place_terminal_surface(&self.surface, 1, x << 32, 0, 16 << 32, 16 << 32, 1)
            .unwrap();
    }
    fn status(&self) -> TrackStatus {
        self.session.query_track(&self.track).unwrap()
    }
}
#[test]
fn inactive_unplaced_images_prime_before_atomic_activation_and_fail_without_false_readiness() {
    let (p, mut host) = fixture();
    let png = encoded(false);
    let jpeg = encoded(true);
    let mut a = Peer::staged(&p, &png, probe_encoded_image(&png).unwrap());
    let mut b = Peer::staged(&p, &jpeg, probe_encoded_image(&jpeg).unwrap());
    assert_eq!(a.status().milestones & MILESTONE_OUTPUT_READY, 0);
    assert_eq!(b.status().milestones & MILESTONE_OUTPUT_READY, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while a.status().milestones & MILESTONE_OUTPUT_READY == 0
        || b.status().milestones & MILESTONE_OUTPUT_READY == 0
    {
        assert!(host.prepare(1, 1.).unwrap().rasters().is_empty());
        assert!(Instant::now() < deadline, "inactive images did not prime");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(a.status().last_presentation_id, 0);
    a.activate(0, MILESTONE_OUTPUT_READY);
    b.activate(16, MILESTONE_OUTPUT_READY);
    let frame = ready(&mut host, 2, 0);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(a.status().last_presentation_id, 1);
    let mut truncated = png;
    truncated.truncate(24);
    let bad = Peer::staged(&p, &truncated, probe_encoded_image(&truncated).unwrap());
    while bad.status().lifecycle != 6 {
        host.prepare(1, 1.).unwrap();
        assert!(
            Instant::now() < deadline,
            "inactive invalid image did not fail"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(bad.status().milestones & MILESTONE_OUTPUT_READY, 0);
    assert_eq!(bad.status().last_presentation_id, 0);
    assert_eq!(ready(&mut host, 2, 0).rasters().len(), 2);
}
fn ready(host: &mut NativeVectorHost, images: usize, windows: usize) -> VectorFrame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = host.prepare(1, 1.).unwrap();
        if frame.rasters().len() == images && frame.drawings().len() == windows {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "encoded image did not become ready"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn png_and_jpeg_wait_for_decode_and_actual_display_and_reuse_immutable_pixels() {
    let (p, mut host) = fixture();
    let png = encoded(false);
    let jpeg = encoded(true);
    let a = Peer::new(&p, &png, probe_encoded_image(&png).unwrap(), 0);
    let b = Peer::new(&p, &jpeg, probe_encoded_image(&jpeg).unwrap(), 16);
    let first = host.prepare(1, 1.).unwrap();
    assert!(
        first.rasters().is_empty(),
        "preparing queues decoding without waiting for it"
    );
    assert_eq!(a.status().last_presentation_id, 0);
    assert_eq!(a.status().milestones & MILESTONE_OUTPUT_READY, 0);
    assert_eq!(b.status().milestones & MILESTONE_PRESENTED, 0);
    drop(first);
    let decoded = ready(&mut host, 2, 0);
    assert_ne!(a.status().milestones & MILESTONE_OUTPUT_READY, 0);
    assert_eq!(
        a.status().last_presentation_id,
        0,
        "decode is not display proof"
    );
    assert!(host.presented(&decoded).unwrap());
    assert_eq!(a.status().last_presentation_id, 1);
    assert_eq!(b.status().last_presentation_id, 1);
    let again = ready(&mut host, 2, 0);
    assert!(decoded.same_content(&again));
    assert!(Arc::ptr_eq(
        &decoded.rasters()[0].image,
        &again.rasters()[0].image
    ));
    a.session.close().unwrap();
    drop(decoded);
    drop(again);
    let peer = ready(&mut host, 1, 0);
    assert_eq!(
        peer.rasters()[0].surface.context().session().session_id(),
        b.session.info().session_id
    );
    assert!(host.presented(&peer).unwrap());
}
#[test]
fn malformed_image_fails_only_its_owner_while_a_peer_keeps_rendering_and_receiving_input() {
    let (p, mut host) = fixture();
    let mut truncated = encoded(false);
    truncated.truncate(24);
    let bad = Peer::new(&p, &truncated, probe_encoded_image(&truncated).unwrap(), 0);
    let png = encoded(false);
    let good = Peer::new(&p, &png, probe_encoded_image(&png).unwrap(), 16);
    let overlays = OverlaySession::connect(config(&p)).unwrap();
    let window = overlays
        .create_window(OverlayWindowOptions::new(
            overlay::Rect::new(0., 0., 8., 8.).unwrap(),
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
    window.submit(canvas).unwrap();
    window.request_focus().unwrap();
    let frame = ready(&mut host, 1, 1);
    let deadline = Instant::now() + Duration::from_secs(3);
    while bad.status().lifecycle != 6 {
        host.prepare(1, 1.).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(bad.status().last_presentation_id, 0);
    assert!(host.presented(&frame).unwrap());
    assert_eq!(good.status().last_presentation_id, 1);
    let target = host.input_target(&frame).unwrap();
    assert_eq!(
        host.dispatch_input(
            &target,
            presenter::VectorInputEvent::Key {
                usage: 4,
                down: true,
                repeat: false,
                modifiers: 0,
                text: Some("peer".into())
            }
        )
        .unwrap(),
        presenter::VectorInputResult::Delivered { text: true }
    );
    loop {
        if let Some(OverlayLaneEvent::Input(input)) =
            overlays.wait_event(Duration::from_millis(10)).unwrap()
            && matches!(input.event, overlay::Event::Text(ref text) if text == "peer")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "peer input was blocked by another owner's image"
        );
    }
}
#[test]
fn a_late_decode_cannot_target_an_owner_reusing_local_ids_after_cleanup() {
    let (p, mut host) = fixture();
    let png = encoded(false);
    let a = Peer::new(&p, &png, probe_encoded_image(&png).unwrap(), 0);
    let initial = host.prepare(1, 1.).unwrap();
    assert!(initial.rasters().is_empty());
    a.session.close().unwrap();
    let replacement = Peer::new(&p, &png, probe_encoded_image(&png).unwrap(), 0);
    assert!(host.presented(&initial).unwrap());
    assert_eq!(replacement.status().last_presentation_id, 0);
    drop(initial);
    let frame = ready(&mut host, 1, 0);
    assert_eq!(
        frame.rasters()[0].surface.context().session().session_id(),
        replacement.session.info().session_id
    );
    assert!(host.presented(&frame).unwrap());
    assert_eq!(replacement.status().last_presentation_id, 1);
}
#[test]
fn decoder_dimensions_must_match_the_immutable_track_configuration() {
    let (p, mut host) = fixture();
    let png = encoded(false);
    let mut wrong = probe_encoded_image(&png).unwrap();
    wrong.width = 1;
    wrong.height = 4;
    let bad = Peer::new(&p, &png, wrong, 0);
    let deadline = Instant::now() + Duration::from_secs(3);
    while bad.status().lifecycle != 6 {
        assert!(host.prepare(1, 1.).unwrap().rasters().is_empty());
        assert!(
            Instant::now() < deadline,
            "inconsistent image dimensions were not rejected"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(bad.status().last_presentation_id, 0);
}

#[test]
fn animated_and_multi_picture_sources_fail_without_affecting_a_static_peer() {
    let (p, mut host) = fixture();
    let png = encoded(false);
    let jpeg = encoded(true);
    let mut apng = png.clone();
    let mut chunk = 8u32.to_be_bytes().to_vec();
    chunk.extend_from_slice(b"acTL");
    chunk.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 0]);
    chunk.extend_from_slice(&crc32fast::hash(&chunk[4..]).to_be_bytes());
    apng.splice(33..33, chunk);
    let mut mpo = jpeg.clone();
    mpo.splice(2..2, [0xff, 0xe2, 0, 6, b'M', b'P', b'F', 0]);
    let mut concatenated = jpeg.clone();
    concatenated.extend_from_slice(&jpeg);
    let good = Peer::new(&p, &png, probe_encoded_image(&png).unwrap(), 16);
    for body in [apng, mpo, concatenated] {
        let bad = Peer::new(&p, &body, probe_encoded_image(&body).unwrap(), 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        while bad.status().lifecycle != 6 {
            host.prepare(1, 1.).unwrap();
            assert!(
                Instant::now() < deadline,
                "multi-picture source was not rejected"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let frame = ready(&mut host, 1, 0);
        assert_eq!(
            frame.rasters()[0].surface.context().session().session_id(),
            good.session.info().session_id
        );
        assert!(host.presented(&frame).unwrap());
        assert_eq!(bad.status().last_presentation_id, 0);
        assert_eq!(good.status().last_presentation_id, 1);
        bad.session.close().unwrap();
    }
}
