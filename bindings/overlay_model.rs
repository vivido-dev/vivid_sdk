//! Shared conversion helpers for the Python and Node overlay bindings. No language callbacks.
use std::io;
use vivid_protocol::vector::*;
use vivid_sdk::overlay::{OverlayWindowOptions, WindowMode};

pub fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn measurement_text(canvas: &Canvas) -> io::Result<Text> {
    let [Command::Text(text)] = canvas.commands() else {
        return Err(invalid("measurement requires one text specification"));
    };
    Ok(text.clone())
}

pub fn typography(
    overflow: &str,
    letter_spacing: f64,
    word_spacing: f64,
    line_height: Option<f64>,
    ligatures: bool,
    kerning: bool,
) -> io::Result<vivid_sdk::overlay::Typography> {
    use vivid_sdk::overlay::{TextOverflow, Typography};
    let result = Typography {
        overflow: match overflow {
            "clip" => TextOverflow::Clip,
            "ellipsis" => TextOverflow::Ellipsis,
            _ => return Err(invalid("unknown text overflow")),
        },
        letter_spacing: Scalar::new(letter_spacing).map_err(io::Error::other)?,
        word_spacing: Scalar::new(word_spacing).map_err(io::Error::other)?,
        line_height: line_height
            .map(Scalar::new)
            .transpose()
            .map_err(io::Error::other)?,
        ligatures,
        kerning,
    };
    result.validate().map_err(io::Error::other)?;
    Ok(result)
}

pub fn text_offset(offset: Option<u32>, source: &str, utf16: bool) -> io::Result<Option<u32>> {
    offset
        .map(|offset| {
            let prefix = source
                .get(..offset as usize)
                .ok_or_else(|| invalid("invalid truncation offset"))?;
            u32::try_from(if utf16 {
                prefix.encode_utf16().count()
            } else {
                prefix.chars().count()
            })
            .map_err(io::Error::other)
        })
        .transpose()
}

pub fn styled_text(
    canvas: &Canvas,
    decorations: &[u32],
    max_width: Option<f64>,
    alignment: &str,
    wrap: bool,
    max_lines: Option<u16>,
) -> io::Result<vivid_sdk::overlay::StyledText> {
    use vivid_sdk::overlay::{StyledText, TextAlignment, TextRun, TextStyle};
    if canvas.commands().len() != decorations.len() {
        return Err(invalid("text run decorations count mismatch"));
    }
    let runs = canvas
        .commands()
        .iter()
        .zip(decorations)
        .map(|(command, flags)| {
            let Command::Text(t) = command else {
                return Err(invalid("styled text requires text runs"));
            };
            if *flags > 3 {
                return Err(invalid("invalid text decorations"));
            }
            Ok(TextRun {
                text: t.text.clone(),
                style: TextStyle {
                    size: t.size,
                    family: t.family.clone(),
                    weight: t.weight,
                    italic: t.italic,
                    color: t.color,
                    underline: flags & 1 != 0,
                    strikethrough: flags & 2 != 0,
                },
            })
        })
        .collect::<io::Result<_>>()?;
    let text = StyledText {
        typography: Default::default(),
        runs,
        max_width: max_width
            .map(Scalar::new)
            .transpose()
            .map_err(io::Error::other)?,
        alignment: match alignment {
            "start" => TextAlignment::Start,
            "center" => TextAlignment::Center,
            "end" => TextAlignment::End,
            "justify" => TextAlignment::Justify,
            _ => return Err(invalid("unknown text alignment")),
        },
        wrap,
        max_lines,
    };
    text.validate().map_err(io::Error::other)?;
    Ok(text)
}

pub fn text_geometry(
    values: &[vivid_sdk::overlay::TextGeometry],
    text: &str,
    utf16: bool,
) -> io::Result<Vec<Vec<f64>>> {
    values
        .iter()
        .map(|g| {
            let index = |offset: u32| -> io::Result<f64> {
                let prefix = text
                    .get(..offset as usize)
                    .ok_or_else(|| invalid("invalid text geometry offset"))?;
                Ok(if utf16 {
                    prefix.encode_utf16().count()
                } else {
                    prefix.chars().count()
                } as f64)
            };
            Ok(vec![
                index(g.start)?,
                index(g.end)?,
                g.x.get(),
                g.y.get(),
                g.width.get(),
                g.height.get(),
                g.baseline.get(),
                f64::from(g.rtl),
            ])
        })
        .collect()
}
pub fn number(value: f64, maximum: u64) -> io::Result<u64> {
    if !value.is_finite() || value < 0. || value.fract() != 0. || value > maximum as f64 {
        return Err(invalid("integer is outside its supported range"));
    }
    Ok(value as u64)
}
pub fn rect(values: &[f64]) -> io::Result<Rect> {
    let [x, y, w, h] = values else {
        return Err(invalid("bounds require x, y, width, height"));
    };
    Rect::new(*x, *y, *w, *h).map_err(io::Error::other)
}
pub fn bounds(rect: Rect) -> Vec<f64> {
    vec![
        rect.origin.x.get(),
        rect.origin.y.get(),
        rect.width.get(),
        rect.height.get(),
    ]
}
pub fn options(
    values: &[f64],
    mode: &str,
    title: String,
    visible: bool,
) -> io::Result<OverlayWindowOptions> {
    let mode = match mode {
        "floating" => WindowMode::Floating,
        "popup" => WindowMode::Popup,
        "modal" => WindowMode::Modal,
        _ => return Err(invalid("unknown window mode")),
    };
    Ok(OverlayWindowOptions {
        bounds: rect(values)?,
        mode,
        title,
        visible,
    })
}
pub fn path(segments: &[Vec<f64>], even_odd: bool) -> io::Result<Path> {
    if segments.len() > MAX_PATH_SEGMENTS {
        return Err(invalid("path segment limit exceeded"));
    }
    let p = |x, y| Point::new(x, y).map_err(io::Error::other);
    let segments = segments
        .iter()
        .map(|v| {
            Ok(match v.as_slice() {
                [0., x, y] => Segment::Move(p(*x, *y)?),
                [1., x, y] => Segment::Line(p(*x, *y)?),
                [2., x, y, a, b] => Segment::Quad(p(*x, *y)?, p(*a, *b)?),
                [3., x, y, a, b, c, d] => Segment::Cubic(p(*x, *y)?, p(*a, *b)?, p(*c, *d)?),
                [4.] => Segment::Close,
                _ => return Err(invalid("invalid path segment")),
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let path = Path { segments, even_odd };
    path.validate().map_err(io::Error::other)?;
    Ok(path)
}
pub fn shape(kind: &str, values: &[f64], radius: f64) -> io::Result<Vec<Vec<f64>>> {
    let rect = rect(values)?;
    let path = match kind {
        "rectangle" => Path::rectangle(rect),
        "rounded" => Path::rounded_rectangle(rect, radius),
        "ellipse" => Path::ellipse(rect),
        _ => return Err(invalid("unknown shape")),
    }
    .map_err(io::Error::other)?;
    Ok(path
        .segments
        .iter()
        .map(|segment| match segment {
            Segment::Move(p) => vec![0., p.x.get(), p.y.get()],
            Segment::Line(p) => vec![1., p.x.get(), p.y.get()],
            Segment::Quad(a, b) => vec![2., a.x.get(), a.y.get(), b.x.get(), b.y.get()],
            Segment::Cubic(a, b, c) => vec![
                3.,
                a.x.get(),
                a.y.get(),
                b.x.get(),
                b.y.get(),
                c.x.get(),
                c.y.get(),
            ],
            Segment::Close => vec![4.],
        })
        .collect())
}
pub fn brush(kind: &str, geometry: &[f64], colors: &[f64], offsets: &[f64]) -> io::Result<Brush> {
    if colors.len() > MAX_GRADIENT_STOPS {
        return Err(invalid("gradient stop limit exceeded"));
    }
    let colors = colors
        .iter()
        .map(|c| number(*c, u64::from(u32::MAX)).map(|c| Color(c as u32)))
        .collect::<io::Result<Vec<_>>>()?;
    if kind == "solid" {
        return match colors.as_slice() {
            [color] => Ok(Brush::Solid(*color)),
            _ => Err(invalid("solid brush requires one color")),
        };
    }
    if colors.len() != offsets.len() {
        return Err(invalid("gradient colors and offsets differ"));
    }
    let stops = colors
        .into_iter()
        .zip(offsets)
        .map(|(color, offset)| {
            if !offset.is_finite() || !(0. ..=1.).contains(offset) {
                return Err(invalid("gradient offset must be between zero and one"));
            }
            Ok(GradientStop {
                offset: (*offset * f64::from(u16::MAX)).round() as u16,
                color,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    match (kind, geometry) {
        ("linear", [x, y, a, b]) => Ok(Brush::Linear {
            start: Point::new(*x, *y).map_err(io::Error::other)?,
            end: Point::new(*a, *b).map_err(io::Error::other)?,
            stops,
        }),
        ("radial", [x, y, r]) => Ok(Brush::Radial {
            center: Point::new(*x, *y).map_err(io::Error::other)?,
            radius: Scalar::new(*r).map_err(io::Error::other)?,
            stops,
        }),
        _ => Err(invalid("invalid gradient geometry")),
    }
}
pub fn state(kind: &str, values: &[f64]) -> io::Result<Command> {
    match (kind, values) {
        ("save", []) => Ok(Command::Save),
        ("restore", []) => Ok(Command::Restore),
        ("opacity", [v]) if v.is_finite() && (0. ..=1.).contains(v) => {
            Ok(Command::Opacity((*v * f64::from(u16::MAX)).round() as u16))
        }
        ("transform", [a, b, c, d, e, f]) => Ok(Command::Transform(
            Transform::new([*a, *b, *c, *d, *e, *f]).map_err(io::Error::other)?,
        )),
        _ => Err(invalid("invalid canvas state command")),
    }
}
pub fn hit_role(role: &str, edges: f64) -> io::Result<HitRole> {
    match role {
        "input" => Ok(HitRole::Input),
        "drag" => Ok(HitRole::Drag),
        "transparent" => Ok(HitRole::Transparent),
        "resize" => Ok(HitRole::Resize(number(edges, 15)? as u8)),
        _ => Err(invalid("unknown hit role")),
    }
}
pub fn timeout(seconds: f64) -> io::Result<std::time::Duration> {
    if !seconds.is_finite() || !(0. ..=60.).contains(&seconds) {
        return Err(invalid("event timeout must be between zero and 60 seconds"));
    }
    std::time::Duration::try_from_secs_f64(seconds).map_err(io::Error::other)
}

pub struct EventData {
    pub kind: &'static str,
    pub revision: u64,
    pub region: u64,
    pub values: Vec<f64>,
    pub text: String,
}
pub fn event_data(event: &vivid_sdk::OverlayLaneEvent) -> EventData {
    use vivid_sdk::overlay::{DismissReason, Event};
    let mut data = EventData {
        kind: "connection-lost",
        revision: 0,
        region: 0,
        values: vec![],
        text: String::new(),
    };
    let vivid_sdk::OverlayLaneEvent::Input(input) = event else {
        match event {
            vivid_sdk::OverlayLaneEvent::ConnectionLost { diagnostic } => {
                data.text = diagnostic.clone()
            }
            vivid_sdk::OverlayLaneEvent::Viewport(update) => {
                data.kind = "viewport";
                data.revision = update.revision;
                let v = update.viewport;
                data.values = vec![
                    v.width.get(),
                    v.height.get(),
                    f64::from(v.scale_numerator),
                    f64::from(v.scale_denominator),
                ];
            }
            vivid_sdk::OverlayLaneEvent::Outcome(result) => {
                data.kind = "submission-outcome";
                data.revision = result.submission.revision;
                data.text = outcome(result.outcome).into();
            }
            vivid_sdk::OverlayLaneEvent::Input(_) => unreachable!(),
        }
        return data;
    };
    data.revision = input.scene_revision;
    match &input.event {
        Event::Focus(focused) => {
            data.kind = "focus";
            data.values = vec![u8::from(*focused).into()];
        }
        Event::Pointer {
            position,
            region,
            button,
            modifiers,
        } => {
            data.kind = "pointer";
            data.region = *region;
            data.values = vec![position.x.get(), position.y.get(), f64::from(*modifiers)];
            if let Some((button, down)) = button {
                data.values
                    .extend([f64::from(*button), u8::from(*down).into()]);
            }
        }
        Event::Wheel {
            position,
            dx,
            dy,
            modifiers,
        } => {
            data.kind = "wheel";
            data.values = vec![
                position.x.get(),
                position.y.get(),
                dx.get(),
                dy.get(),
                f64::from(*modifiers),
            ];
        }
        Event::Key {
            physical,
            down,
            repeat,
            modifiers,
        } => {
            data.kind = "key";
            data.values = vec![
                f64::from(*physical),
                u8::from(*down).into(),
                u8::from(*repeat).into(),
                f64::from(*modifiers),
            ];
        }
        Event::Text(text) => {
            data.kind = "text";
            data.text = text.clone();
        }
        Event::Ime { preedit, selection } => {
            data.kind = "ime";
            data.text = preedit.clone();
            if let Some((a, b)) = selection {
                data.values = vec![f64::from(*a), f64::from(*b)];
            }
        }
        Event::Geometry {
            bounds: rect,
            settled,
        } => {
            data.kind = "geometry";
            data.values = bounds(*rect);
            data.values.push(u8::from(*settled).into());
        }
        Event::Dismissed(reason) => {
            data.kind = "dismissed";
            data.text = match reason {
                DismissReason::Escape => "escape",
                DismissReason::OutsidePress => "outside-press",
                DismissReason::Closed => "closed",
                DismissReason::OwnerLost => "owner-lost",
                DismissReason::ParentClosed => "parent-closed",
            }
            .into();
        }
        Event::Cancel => {
            data.kind = "cancel";
        }
    }
    data
}

pub fn outcome(value: vivid_sdk::overlay::PresentationOutcome) -> &'static str {
    match value {
        vivid_sdk::overlay::PresentationOutcome::Presented => "presented",
        vivid_sdk::overlay::PresentationOutcome::Superseded => "superseded",
    }
}
