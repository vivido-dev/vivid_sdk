//! Draw freeform paths: curves, an even-odd hole, and a stroke with caps, joins and dashes.
//!
//! Example 07 used the shape constructors — rectangles, rounded rectangles, ellipses — and those
//! cover a user interface. Everything else is a path you build: a chart, a map, a signature, a
//! gauge. The four segment kinds compose into all of it, and the even-odd rule is what puts a
//! hole in a shape rather than a second layer over it.
//!
//! The same builder exists in all three languages, with the same segment kinds and the same
//! options, so the geometry below is written the same way in each.
use std::io;
use std::time::{Duration, Instant};

use vivid_sdk::overlay::{
    Brush, Canvas, Cap, Color, Command, CursorShape, HitRole, Join, Path, PathBuilder, Point, Rect,
    Scalar, StrokeStyle,
};
use vivid_sdk::overlay::{DismissReason, Event, WindowMode};
use vivid_sdk::{OverlayLaneEvent, OverlaySession, OverlayWindowOptions};

// Shared with the raster examples; this one drives its own event loop instead of blocking.
#[path = "support/hold.rs"]
#[allow(dead_code)]
mod hold;

const WIDTH: f64 = 400.;
const HEIGHT: f64 = 220.;
/// Application-chosen hit region ID. Nonzero and unique within one display list.
const CANVAS: u64 = 1;

/// The circle constant, for the cubic approximation of a round shape.
const KAPPA: f64 = 0.552_284_749_830_793_6;

fn scalar(value: f64) -> io::Result<Scalar> {
    Scalar::new(value).map_err(io::Error::other)
}

fn point(x: f64, y: f64) -> io::Result<Point> {
    Point::new(x, y).map_err(io::Error::other)
}

/// A circle as four cubics, appended to a path that may already have subpaths.
///
/// A ring needs two of these in one builder, and a finished `Path` cannot be extended — which is
/// what a builder is for. `Path::ellipse` does the same arithmetic for a whole ellipse; this
/// writes it out because a ring is one path with two circles in it.
///
/// Nothing here can fail: a coordinate the wire cannot carry is remembered by the builder and
/// reported by `build()`, so a chain reads as geometry rather than as error handling.
fn circle(path: PathBuilder, cx: f64, cy: f64, radius: f64) -> PathBuilder {
    let k = radius * KAPPA;
    path.move_to(cx, cy - radius)
        .cubic_to(cx + k, cy - radius, cx + radius, cy - k, cx + radius, cy)
        .cubic_to(cx + radius, cy + k, cx + k, cy + radius, cx, cy + radius)
        .cubic_to(cx - k, cy + radius, cx - radius, cy + k, cx - radius, cy)
        .cubic_to(cx - radius, cy - k, cx - k, cy - radius, cx, cy - radius)
        .close()
}

/// A star, filled: ten corners alternating between two radii, walked with `line_to`.
fn star(cx: f64, cy: f64, radius: f64) -> io::Result<Path> {
    let mut path = Path::builder();
    for corner in 0..10 {
        let reach = if corner % 2 == 0 {
            radius
        } else {
            radius * 0.45
        };
        let angle = -std::f64::consts::FRAC_PI_2 + corner as f64 * std::f64::consts::PI / 5.;
        let (x, y) = (cx + reach * angle.cos(), cy + reach * angle.sin());
        path = if corner == 0 {
            path.move_to(x, y)
        } else {
            path.line_to(x, y)
        };
    }
    path.close().build().map_err(io::Error::other)
}

/// The whole scene: a filled star, a curved stroke, a ring with a hole in it, and a dashed line.
///
/// A builder reports a coordinate the wire cannot carry as an error rather than panicking part
/// way through an expression, so `build()` is where a path that went wrong is found — and it
/// finds the *first* thing that went wrong, not whichever was checked last.
fn scene(label: &str, font: Option<&(String, f64)>) -> io::Result<Canvas> {
    let mut canvas = Canvas::new();
    let frame = Rect::new(0., 0., WIDTH, HEIGHT).map_err(io::Error::other)?;
    let face = Path::rounded_rectangle(frame, 10.).map_err(io::Error::other)?;
    canvas
        .fill(face.clone(), Brush::Solid(Color(0x181828ff)))
        .map_err(io::Error::other)?;

    // A filled star: ten corners, alternating radii, one `line_to` each.
    let badge = star(58., 72., 34.)?;
    canvas
        .fill(badge.clone(), Brush::Solid(Color(0xe0b050ff)))
        .map_err(io::Error::other)?;

    // One cubic through two control points, stroked with round caps and a round join. The caps
    // are why the ends are not cut off square.
    let curve = Path::builder()
        .move_to(108., 96.)
        .cubic_to(150., 20., 220., 130., 262., 46.)
        .build()
        .map_err(io::Error::other)?;
    let mut round = StrokeStyle::new(5.).map_err(io::Error::other)?;
    round.cap = Cap::Round;
    round.join = Join::Round;
    canvas
        .stroke_styled(curve, Brush::Solid(Color(0x8ecbffff)), round)
        .map_err(io::Error::other)?;

    // A ring: two circles in one path, with the even-odd rule. The inner one is a hole rather
    // than a second disc, so the background shows through it — and the host hit tests the rule
    // it fills by, so the hole is not part of the region either.
    let ring = circle(Path::builder().even_odd(), 316., 72., 34.);
    let ring = circle(ring, 316., 72., 16.)
        .build()
        .map_err(io::Error::other)?;
    canvas
        .fill(ring, Brush::Solid(Color(0x70d090ff)))
        .map_err(io::Error::other)?;

    // A dashed line: the dashes belong to the stroke, not to the path, so the path is two points.
    let rule = Path::builder()
        .move_to(24., 158.)
        .line_to(WIDTH - 24., 158.)
        .build()
        .map_err(io::Error::other)?;
    let mut dashed = StrokeStyle::new(3.).map_err(io::Error::other)?;
    dashed.cap = Cap::Butt;
    dashed.dashes = vec![scalar(10.)?, scalar(6.)?];
    canvas
        .stroke_styled(rule, Brush::Solid(Color(0xff8ea0ff)), dashed)
        .map_err(io::Error::other)?;

    canvas
        .push(Command::Text(vivid_sdk::overlay::Text {
            text: label.to_owned(),
            origin: point(24., 180.)?,
            size: scalar(font.map_or(15., |(_, size)| *size))?,
            // An empty family asks the host for its default; custom font bytes are not supported.
            family: font.map_or_else(String::new, |(family, _)| family.clone()),
            weight: 400,
            italic: false,
            color: Color(0xc0c0d0ff),
            max_width: None,
        }))
        .map_err(io::Error::other)?;

    // Without a hit region the whole window would be input-transparent's opposite: the default
    // is the window rectangle. Declaring one lets the host report which part was pressed.
    canvas
        .push(Command::Hit {
            id: CANVAS,
            path: face,
            role: HitRole::Input,
            cursor: Some(CursorShape::Pointer),
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

    let mut font: Option<(String, f64)> = None;
    window.present(scene(
        "star, curve, ring, dashes — click or press \u{1b}",
        font.as_ref(),
    )?)?;
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
                        region: CANVAS,
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
            // The host says which font it draws plain text in, so the scene adopts it rather
            // than guessing and looking foreign in the pane.
            OverlayLaneEvent::Environment(update) => {
                let env = &update.environment;
                if !env.font_family.is_empty() {
                    font = Some((env.font_family.clone(), env.font_size.get()));
                    window.present(scene(
                        "star, curve, ring, dashes — click or press \u{1b}",
                        font.as_ref(),
                    )?)?;
                }
            }
            // Submission outcomes and viewport snapshots share this lane; a drawing-only
            // example has nothing to do with them.
            OverlayLaneEvent::Outcome(_) | OverlayLaneEvent::Viewport(_) => {}
            // An action can only come back for a semantic tree this window published, and a
            // drawing-only example publishes none.
            OverlayLaneEvent::Accessibility { .. } => {}
        }
    }

    // Always attempt graceful cleanup, including when presentation failed.
    let closed = window.close();
    overlays.close().and(closed)
}
