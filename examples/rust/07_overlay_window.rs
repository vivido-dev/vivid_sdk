//! Draw an interactive overlay panel and pump its input lane until it is dismissed.
//!
//! Unlike examples 01-05 this presents a vector display list rather than a raster: the host
//! shapes the text and rasterizes the paths, so moving the window never reuploads anything.
use std::io;
use std::time::{Duration, Instant};

use vivid_sdk::overlay::{Brush, Canvas, Color, Command, HitRole, Path, Point, Rect, Text};
use vivid_sdk::overlay::{DismissReason, Event, WindowMode};
use vivid_sdk::{OverlayLaneEvent, OverlaySession, OverlayWindowOptions};

// Shared with the raster examples; this one drives its own event loop instead of blocking.
#[path = "support/hold.rs"]
#[allow(dead_code)]
mod hold;

const WIDTH: f64 = 320.;
const HEIGHT: f64 = 180.;
/// Application-chosen hit region ID. Nonzero and unique within one display list.
const PANEL: u64 = 1;

fn panel(label: &str) -> io::Result<Canvas> {
    let bounds = Rect::new(0., 0., WIDTH, HEIGHT).map_err(io::Error::other)?;
    let face = Path::rounded_rectangle(bounds, 12.).map_err(io::Error::other)?;
    let mut canvas = Canvas::new();
    canvas
        .fill(face.clone(), Brush::Solid(Color(0x203050ff)))
        .map_err(io::Error::other)?;
    canvas
        .stroke(face.clone(), Brush::Solid(Color(0x66ccffff)), 2.)
        .map_err(io::Error::other)?;
    canvas
        .push(Command::Text(Text {
            text: label.to_owned(),
            origin: Point::new(20., 24.).map_err(io::Error::other)?,
            size: vivid_sdk::overlay::Scalar::new(18.).map_err(io::Error::other)?,
            // An empty family asks the host for its default; custom font bytes are not supported.
            family: String::new(),
            weight: 400,
            italic: false,
            color: Color(0xffffffff),
            max_width: None,
        }))
        .map_err(io::Error::other)?;
    // Without a hit region the whole window would be input-transparent's opposite: the default
    // is the window rectangle. Declaring one lets the host report which part was pressed.
    canvas
        .push(Command::Hit {
            id: PANEL,
            path: face,
            role: HitRole::Input,
        })
        .map_err(io::Error::other)?;
    Ok(canvas)
}

fn main() -> io::Result<()> {
    let duration = hold::duration(std::env::args().skip(1))?;
    let overlays = OverlaySession::from_env()?;
    let window = overlays.create_window(OverlayWindowOptions::new(
        Rect::new(40., 40., WIDTH, HEIGHT).map_err(io::Error::other)?,
        WindowMode::Floating,
    ))?;

    window.present(panel("Click the panel, or press Escape.")?)?;
    window.center()?;
    window.request_focus()?;

    let deadline = duration.map(|duration| Instant::now() + duration);
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        let Some(event) = overlays.wait_event(Duration::from_millis(200))? else {
            continue;
        };
        match event {
            OverlayLaneEvent::ConnectionLost { diagnostic } => {
                eprintln!("overlay connection lost: {diagnostic}");
                break;
            }
            OverlayLaneEvent::Input(input) => {
                // One session can own many windows, so every event names the one it belongs to.
                if !overlays.event_targets(&input, &window)? {
                    continue;
                }
                match input.event {
                    Event::Pointer {
                        region: PANEL,
                        button: Some((_, true)),
                        ..
                    } => break,
                    Event::Dismissed(DismissReason::Escape) => break,
                    Event::Dismissed(reason) => {
                        eprintln!("overlay dismissed: {reason:?}");
                        break;
                    }
                    _ => {}
                }
            }
            // Submission outcomes and viewport snapshots share this lane; a drawing-only
            // example has nothing to do with them.
            OverlayLaneEvent::Outcome(_) | OverlayLaneEvent::Viewport(_) => {}
        }
    }

    // Always attempt graceful cleanup, including when presentation failed.
    let closed = window.close();
    overlays.close().and(closed)
}
