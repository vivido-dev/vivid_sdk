//! Show a PNG/JPEG through the pane convenience API.
use std::io;
use vivid_sdk::PaneSession;
#[path = "support/hold.rs"]
mod hold;

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| io::Error::other("usage: sdk_01_show_image IMAGE [--duration SECONDS]"))?;
    let duration = hold::duration(args)?;
    // Read before connecting so missing files fail without creating a presentation.
    let encoded = std::fs::read(path)?;
    let mut pane = PaneSession::from_env()?;
    let result = pane
        .show_encoded_image(&encoded)
        .and_then(|()| hold::wait(duration));
    // Always attempt graceful cleanup, including when presentation fails.
    let close = pane.close();
    result.and(close)
}
