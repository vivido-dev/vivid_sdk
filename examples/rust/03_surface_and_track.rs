//! surface and track: explicit Vivid 1.5 lifecycle.
use std::io;
use vivid_protocol::messages::LaneClass;
use vivid_sdk::{
    MILESTONE_OUTPUT_READY, ProducerConfig, RequestMetadata, SLOT_RASTER, Session, SlotBinding,
    SurfaceBuilder, SurfaceRole, TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH, TrackBuilder, TrackMode,
    TrackWaitCondition,
};
#[path = "support/hold.rs"]
mod hold;
// Four distinct RGBA8 pixels: red, green, blue, white.
const PIXELS: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
];

fn main() -> io::Result<()> {
    let duration = hold::duration(std::env::args().skip(1))?;
    let mut session = Session::connect(ProducerConfig::default())?;
    let result = run(&mut session, duration);
    let close = session.close();
    result.and(close)
}

fn run(session: &mut Session, duration: Option<std::time::Duration>) -> io::Result<()> {
    let metadata = RequestMetadata::default();
    let definition = SurfaceBuilder::new(session, 2, 2)?
        .titled(SurfaceRole::Figure, "SDK raster example")
        .build()?;
    let surface = session.create_surface(definition, &metadata)?;
    let node_id = session.allocate_id()?;
    // Rust geometry is signed 32.32 terminal cells; this occupies the top-left 16 x 8 cells.
    session.place_terminal_surface(
        &surface,
        node_id,
        0,
        0,
        16_i64 << 32,
        8_i64 << 32,
        TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH,
    )?;
    let result = (|| {
        let configuration =
            TrackBuilder::new(&surface, SLOT_RASTER, TrackMode::Live, LaneClass::Bulk)
                .raster(2, 2)?
                .build(&session.info().resource_contract, session.allocate_id()?)?;
        let track = session.create_track(configuration, &metadata)?;
        let channel = session.open_track_channel(&track)?;
        channel.send_raster(0, 1, &PIXELS, false)?;
        session.wait_track(
            &track,
            TrackWaitCondition::MilestoneSet,
            Some(MILESTONE_OUTPUT_READY),
            5_000_000,
        )?;
        session.activate_tracks(
            &surface,
            &[SlotBinding {
                slot: SLOT_RASTER,
                track_id: track.id(),
                expected_channel_generation: track.channel_generation(),
                required_milestone: MILESTONE_OUTPUT_READY,
            }],
            &metadata,
        )?;
        channel.eos()?;
        hold::wait(duration)
    })();
    // Evaluate every cleanup operation, preserving the original failure if there was one.
    let remove_node = session.delete_node(surface.context_id(), node_id, &metadata);
    let remove_surface = session.destroy_surface(&surface, &metadata);
    result.and(remove_node.map(|_| ())).and(remove_surface)
}
