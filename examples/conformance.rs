//! Print this SDK's canonical conformance report.
//!
//! Every binding runs the same scenarios and prints the same report, so the comparison is
//! between languages rather than between expectations: a value that drifts in one of them shows
//! up as a diff rather than as a subtly different wire value at runtime.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example conformance --features presenter
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use vivid_protocol::messages::LaneClass;
use vivid_sdk::presenter::{
    CaptureContent, MediaConfig, PresenterListener, SocketListener, VirtualVivid,
};
use vivid_sdk::{
    ConstantValue, GENERIC_CONTENT, MILESTONE_OUTPUT_READY, ProducerAuthentication, ProducerConfig,
    RequestMetadata, SLOT_RASTER, Session, SlotBinding, SurfaceBuilder, SurfaceRole, TrackBuilder,
    TrackMode, TrackWaitCondition, constant_table,
};

/// The frame every binding presents and reads back.
const PIXELS: [u8; 16] = [
    200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255, 200, 0, 0, 255,
];
const PANE: u64 = 1;

fn main() {
    let mut report: BTreeMap<&str, String> = BTreeMap::new();
    report.insert("constants", constants());
    report.insert("raster", raster());
    let body = report
        .iter()
        .map(|(key, value)| format!("  {key:?}: {value}"))
        .collect::<Vec<_>>()
        .join(",\n");
    println!("{{\n{body}\n}}");
}

/// Every constant the SDK exposes, as name to value, in a stable order.
fn constants() -> String {
    let entries = constant_table()
        .iter()
        .map(|(name, value)| {
            let rendered = match value {
                ConstantValue::Text(text) => format!("{text:?}"),
                ConstantValue::Number(number) => number.to_string(),
            };
            format!("{name:?}: {rendered}")
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{entries}}}")
}

/// Present one frame and read it back, reporting what the presenter retained.
fn raster() -> String {
    let listener = SocketListener::bind("tcp:127.0.0.1:0").expect("bind a loopback presenter");
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start(listener, MediaConfig::default()).expect("start");
    let capability = presenter.issue_pane_capability(PANE).expect("capability");
    presenter.update_metrics(PANE, 80, 24, (8, 16));

    let mut session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&capability).expect("root auth"),
        ..ProducerConfig::default()
    })
    .expect("connect");

    let surface = session
        .create_surface(
            SurfaceBuilder::new(&session, 2, 2)
                .expect("surface builder")
                .titled(SurfaceRole::Figure, "conformance")
                .semantic(
                    GENERIC_CONTENT,
                    vivid_sdk::CoordinateModel::DesktopLogicalPixels,
                )
                .build()
                .expect("surface"),
            &RequestMetadata::default(),
        )
        .expect("create surface");

    let contract = session.info().resource_contract.clone();
    let configuration = TrackBuilder::detached(
        surface.context_id(),
        surface.id(),
        SLOT_RASTER,
        TrackMode::Live,
        LaneClass::Bulk,
    )
    .raster(2, 2)
    .expect("raster builder")
    .build(&contract, session.allocate_id().expect("track id"))
    .expect("track configuration");
    let track = session
        .create_track(configuration, &RequestMetadata::default())
        .expect("create track");

    // Place the node before sending: activation composites the node, and a surface with no
    // node has nothing a presenter could present.
    session
        .place_terminal_surface(
            &surface,
            1,
            0,
            0,
            2_i64 << 32,
            2_i64 << 32,
            vivid_sdk::TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH,
        )
        .expect("place the node");

    let channel = session.open_track_channel(&track).expect("channel");
    channel
        .send_raster(0, 1, &PIXELS, false)
        .expect("send the frame");
    session
        .wait_track(
            &track,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_OUTPUT_READY),
            30_000_000,
        )
        .expect("output ready");
    session
        .activate_tracks(
            &surface,
            &[SlotBinding {
                slot: SLOT_RASTER,
                track_id: track.id(),
                expected_channel_generation: track.channel_generation(),
                required_milestone: MILESTONE_OUTPUT_READY,
            }],
            &RequestMetadata::default(),
        )
        .expect("activate");

    let retained = presenter.wait_for_retained_media(PANE, Duration::from_secs(5));
    let capture = presenter.capture_pane(PANE, 0);
    let (kind, pixels) = match capture.layers.first().map(|layer| &layer.content) {
        Some(CaptureContent::Raster(raster)) => ("raster", raster.pixels.to_vec()),
        Some(CaptureContent::EncodedImage(_)) => ("encodedImage", Vec::new()),
        None => ("none", Vec::new()),
    };

    let _ = channel.eos();
    let _ = session.close();

    format!(
        "{{\"retained\":{retained},\"layers\":{},\"skipped\":{},\"contentKind\":{kind:?},\"pixels\":[{}]}}",
        capture.layers.len(),
        capture.skipped.len(),
        pixels
            .iter()
            .map(|byte| byte.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}
