//! Verify a real producer/presenter round trip without a terminal or external credentials.
use std::{io, time::Duration};
use vivid_sdk::presenter::{
    CaptureContent, MediaConfig, PresenterListener, SocketListener, VirtualVivid,
};
use vivid_sdk::{PaneSession, ProducerAuthentication, ProducerConfig, Session};
// Four distinct RGBA8 pixels: red, green, blue, white.
const PIXELS: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
];

fn main() -> io::Result<()> {
    let listener = SocketListener::bind("tcp:127.0.0.1:0")?;
    let endpoint = listener.endpoint();
    let presenter = VirtualVivid::start(listener, MediaConfig::default())?;
    presenter.update_metrics(1, 80, 24, (8, 16));
    // Keep this capability in memory; never print it or pass it through argv.
    let capability = presenter.issue_pane_capability(1)?;
    let session = Session::connect(ProducerConfig {
        endpoint_control: Some(endpoint),
        authentication: ProducerAuthentication::root_hex(&capability)?,
        ..ProducerConfig::default()
    })?;
    let mut pane = PaneSession::from_session(session)?;
    let result = (|| {
        pane.show_rgba(2, 2, &PIXELS)?;
        if !presenter.wait_for_retained_media(1, Duration::from_secs(5)) {
            return Err(io::Error::other("timed out waiting for retained pixels"));
        }
        let capture = presenter.capture_pane(1, 0);
        let Some(CaptureContent::Raster(frame)) =
            capture.layers.first().map(|layer| &layer.content)
        else {
            return Err(io::Error::other("expected one raster layer"));
        };
        if capture.layers.len() != 1
            || !capture.skipped.is_empty()
            || frame.width != 2
            || frame.height != 2
            || frame.pixels.as_ref() != PIXELS
        {
            return Err(io::Error::other(
                "captured dimensions or RGBA pixels differ",
            ));
        }
        println!("Verified one 2 x 2 raster with exact red, green, blue, white pixels.");
        Ok(())
    })();
    let close = pane.close();
    // Dropping the presenter shuts down its listener and workers.
    drop(presenter);
    result.and(close)
}
