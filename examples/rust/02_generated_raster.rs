//! Display a generated four-color RGBA image without image libraries.
use std::io;
use vivid_sdk::PaneSession;
#[path = "support/hold.rs"]
mod hold;
// Four distinct RGBA8 pixels: red, green, blue, white.
const PIXELS: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
];

fn main() -> io::Result<()> {
    let duration = hold::duration(std::env::args().skip(1))?;
    let mut pane = PaneSession::from_env()?;
    let result = pane
        .show_rgba(2, 2, &PIXELS)
        .and_then(|()| hold::wait(duration));
    // Always attempt graceful cleanup, including when presentation fails.
    let close = pane.close();
    result.and(close)
}
