//! Shared conversion helpers for the Python, Node, and Lua overlay bindings. No language callbacks.
use std::io;
use vivid_protocol::overlay::{AccessibleAction, SemanticNode, SemanticRole, Semantics, Toggled};
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
    min_width: f64,
    min_height: f64,
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
        min_width: Scalar::new(min_width).map_err(io::Error::other)?,
        min_height: Scalar::new(min_height).map_err(io::Error::other)?,
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
    if kind == "rounded-corners" {
        let (bounds, radii) = values.split_at(4);
        let radii: Vec<f64> = radii.to_vec();
        let [tl, tr, br, bl]: [f64; 4] = radii
            .try_into()
            .map_err(|_| invalid("rounded corners require four radii"))?;
        let path = Path::rounded_rectangle_corners(
            rect(bounds)?,
            Corners::new([tl, tr, br, bl]).map_err(io::Error::other)?,
        )
        .map_err(io::Error::other)?;
        return flatten(&path);
    }
    let rect = rect(values)?;
    let path = match kind {
        "rectangle" => Path::rectangle(rect),
        "rounded" => Path::rounded_rectangle(rect, radius),
        "ellipse" => Path::ellipse(rect),
        _ => return Err(invalid("unknown shape")),
    }
    .map_err(io::Error::other)?;
    flatten(&path)
}

fn flatten(path: &Path) -> io::Result<Vec<Vec<f64>>> {
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
pub fn color_space(space: &str) -> io::Result<ColorSpace> {
    match space {
        "srgb" => Ok(ColorSpace::Srgb),
        "oklab" => Ok(ColorSpace::Oklab),
        _ => Err(invalid("gradient space is srgb or oklab")),
    }
}

/// An image brush by the asset identity a retained image handle owns. The identity crosses the
/// language boundary at full width because the caller reads it from the handle, not from a
/// floating-point array.
pub fn brush_image(asset: u64, transform: Option<Vec<f64>>, extend: &str) -> io::Result<Brush> {
    let transform = transform
        .map(|values| {
            let values: Result<Vec<_>, _> = values.iter().map(|v| Scalar::new(*v)).collect();
            let values = values.map_err(|_| invalid("image transform terms are out of range"))?;
            <[Scalar; 6]>::try_from(values)
                .map(Transform)
                .map_err(|_| invalid("image transform requires six terms"))
        })
        .transpose()?;
    let extend = match extend {
        "pad" => Extend::Pad,
        "repeat" => Extend::Repeat,
        "reflect" => Extend::Reflect,
        _ => return Err(invalid("image extend is pad, repeat, or reflect")),
    };
    Ok(Brush::Image {
        asset,
        transform,
        extend,
    })
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
            color_space: ColorSpace::Srgb,
        }),
        ("radial", [x, y, r]) => Ok(Brush::Radial {
            center: Point::new(*x, *y).map_err(io::Error::other)?,
            radius: Scalar::new(*r).map_err(io::Error::other)?,
            stops,
            color_space: ColorSpace::Srgb,
        }),
        _ => Err(invalid("invalid gradient geometry")),
    }
}

/// A gradient brush with an explicit interpolation space.
pub fn gradient_brush(
    kind: &str,
    geometry: &[f64],
    colors: &[f64],
    offsets: &[f64],
    space: &str,
) -> io::Result<Brush> {
    let space = color_space(space)?;
    match brush(kind, geometry, colors, offsets)? {
        Brush::Linear {
            start, end, stops, ..
        } => Ok(Brush::Linear {
            start,
            end,
            stops,
            color_space: space,
        }),
        Brush::Radial {
            center,
            radius,
            stops,
            ..
        } => Ok(Brush::Radial {
            center,
            radius,
            stops,
            color_space: space,
        }),
        _ => Err(invalid("gradient brush requires gradient geometry")),
    }
}

/// One blurred rounded rectangle, from the flattened language-side values:
/// `[x, y, width, height, tl, tr, br, bl, offset_x, offset_y, blur, spread, inset]`.
pub fn shadow(values: &[f64], color: u32) -> io::Result<Command> {
    let [
        x,
        y,
        width,
        height,
        tl,
        tr,
        br,
        bl,
        ox,
        oy,
        blur,
        spread,
        inset,
    ] = values
    else {
        return Err(invalid("shadow requires thirteen values"));
    };
    let command = Command::Shadow(Shadow {
        rect: rect(&[*x, *y, *width, *height])?,
        radii: Corners::new([*tl, *tr, *br, *bl]).map_err(io::Error::other)?,
        color: Color(color),
        offset: Point::new(*ox, *oy).map_err(io::Error::other)?,
        blur: Scalar::new(*blur).map_err(io::Error::other)?,
        spread: Scalar::new(*spread).map_err(io::Error::other)?,
        inset: *inset != 0.,
    });
    Ok(command)
}

/// A stroke with caps, joins, and dashes. `dashes` empty means a solid line.
pub fn stroke_style(
    width: f64,
    cap: &str,
    join: &str,
    miter_limit: f64,
    dashes: &[f64],
    dash_offset: f64,
) -> io::Result<StrokeStyle> {
    let cap = match cap {
        "butt" => Cap::Butt,
        "round" => Cap::Round,
        "square" => Cap::Square,
        _ => return Err(invalid("stroke cap is butt, round, or square")),
    };
    let join = match join {
        "miter" => Join::Miter,
        "bevel" => Join::Bevel,
        "round" => Join::Round,
        _ => return Err(invalid("stroke join is miter, bevel, or round")),
    };
    let dashes = dashes
        .iter()
        .map(|d| Scalar::new(*d))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("dash lengths are out of range"))?;
    Ok(StrokeStyle {
        width: Scalar::new(width).map_err(|_| invalid("stroke width is out of range"))?,
        cap,
        join,
        miter_limit: Scalar::new(miter_limit)
            .map_err(|_| invalid("miter limit is out of range"))?,
        dashes,
        dash_offset: Scalar::new(dash_offset)
            .map_err(|_| invalid("dash offset is out of range"))?,
    })
}
/// One semantic node as a language binding hands it over, before validation.
pub struct SemanticNodeInput {
    pub id: u64,
    pub role: String,
    pub bounds: Vec<f64>,
    pub label: String,
    pub numeric: Option<Vec<f64>>,
    pub level: Option<u8>,
    pub set: Option<Vec<u16>>,
    pub toggled: Option<String>,
    pub disabled: bool,
    pub actions: Vec<String>,
    pub children: Vec<u32>,
}

/// A complete semantic tree from the language-side nodes.
pub fn semantics(scene_revision: u64, nodes: Vec<SemanticNodeInput>) -> io::Result<Semantics> {
    let mut parsed = Vec::with_capacity(nodes.len());
    for node in nodes {
        let numeric = match node.numeric {
            None => None,
            Some(values) => {
                let [value, minimum, maximum] = values.as_slice() else {
                    return Err(invalid("numeric requires value, minimum and maximum"));
                };
                Some([
                    Scalar::new(*value).map_err(|_| invalid("numeric value is out of range"))?,
                    Scalar::new(*minimum)
                        .map_err(|_| invalid("numeric minimum is out of range"))?,
                    Scalar::new(*maximum)
                        .map_err(|_| invalid("numeric maximum is out of range"))?,
                ])
            }
        };
        let set = match node.set {
            None => None,
            Some(values) => {
                let [position, size] = values.as_slice() else {
                    return Err(invalid("a set requires a position and a size"));
                };
                Some([*position, *size])
            }
        };
        let toggled = match node.toggled.as_deref() {
            None => None,
            Some("off") => Some(Toggled::Off),
            Some("on") => Some(Toggled::On),
            Some("mixed") => Some(Toggled::Mixed),
            Some(_) => return Err(invalid("toggled is off, on, or mixed")),
        };
        let actions = node
            .actions
            .iter()
            .map(|action| accessible_action(action))
            .collect::<io::Result<Vec<_>>>()?;
        parsed.push(SemanticNode {
            id: node.id,
            role: semantic_role(&node.role)?,
            bounds: rect(&node.bounds)?,
            label: node.label,
            numeric,
            level: node.level,
            set,
            toggled,
            disabled: node.disabled,
            actions,
            children: node.children,
        });
    }
    let semantics = Semantics {
        scene_revision,
        nodes: parsed,
    };
    semantics.validate().map_err(io::Error::other)?;
    Ok(semantics)
}

/// The protocol's closed role set by name, so a binding cannot invent one.
fn semantic_role(role: &str) -> io::Result<SemanticRole> {
    Ok(match role {
        "generic" => SemanticRole::Generic,
        "application" => SemanticRole::Application,
        "group" => SemanticRole::Group,
        "heading" => SemanticRole::Heading,
        "text" => SemanticRole::Text,
        "button" => SemanticRole::Button,
        "switch" => SemanticRole::Switch,
        "checkbox" => SemanticRole::CheckBox,
        "radio-button" => SemanticRole::RadioButton,
        "text-input" => SemanticRole::TextInput,
        "slider" => SemanticRole::Slider,
        "spin-button" => SemanticRole::SpinButton,
        "progress-indicator" => SemanticRole::ProgressIndicator,
        "list" => SemanticRole::List,
        "list-item" => SemanticRole::ListItem,
        "image" => SemanticRole::Image,
        "link" => SemanticRole::Link,
        "dialog" => SemanticRole::Dialog,
        "tab" => SemanticRole::Tab,
        "separator" => SemanticRole::Separator,
        _ => return Err(invalid("unknown semantic role")),
    })
}

fn accessible_action(action: &str) -> io::Result<AccessibleAction> {
    Ok(match action {
        "default" => AccessibleAction::Default,
        "focus" => AccessibleAction::Focus,
        "click" => AccessibleAction::Click,
        "increment" => AccessibleAction::Increment,
        "decrement" => AccessibleAction::Decrement,
        "expand" => AccessibleAction::Expand,
        "collapse" => AccessibleAction::Collapse,
        _ => return Err(invalid("unknown accessible action")),
    })
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
/// The protocol's cursor set by name, so a language binding cannot invent a shape.
pub fn cursor(shape: &str) -> io::Result<Option<CursorShape>> {
    let shape = match shape {
        "" => return Ok(None),
        "default" => CursorShape::Default,
        "pointer" => CursorShape::Pointer,
        "text" => CursorShape::Text,
        "move" => CursorShape::Move,
        "crosshair" => CursorShape::Crosshair,
        "not-allowed" => CursorShape::NotAllowed,
        "grab" => CursorShape::Grab,
        "grabbing" => CursorShape::Grabbing,
        "wait" => CursorShape::Wait,
        "progress" => CursorShape::Progress,
        "resize-left" => CursorShape::ResizeLeft,
        "resize-right" => CursorShape::ResizeRight,
        "resize-up" => CursorShape::ResizeUp,
        "resize-down" => CursorShape::ResizeDown,
        "resize-up-left" => CursorShape::ResizeUpLeft,
        "resize-up-right" => CursorShape::ResizeUpRight,
        "resize-down-left" => CursorShape::ResizeDownLeft,
        "resize-down-right" => CursorShape::ResizeDownRight,
        "resize-left-right" => CursorShape::ResizeLeftRight,
        "resize-up-down" => CursorShape::ResizeUpDown,
        _ => return Err(invalid("unknown cursor shape")),
    };
    Ok(Some(shape))
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

/// The language-facing name of an accessible action.
fn accessible_action_name(action: vivid_sdk::overlay::AccessibleAction) -> &'static str {
    use vivid_sdk::overlay::AccessibleAction as Action;
    match action {
        Action::Default => "default",
        Action::Focus => "focus",
        Action::Click => "click",
        Action::Increment => "increment",
        Action::Decrement => "decrement",
        Action::Expand => "expand",
        Action::Collapse => "collapse",
    }
}

/// The language-facing scroll phase index, matching the protocol's wire order.
fn scroll_phase(phase: vivid_sdk::overlay::ScrollPhase) -> u8 {
    use vivid_sdk::overlay::ScrollPhase;
    match phase {
        ScrollPhase::None => 0,
        ScrollPhase::Began => 1,
        ScrollPhase::Changed => 2,
        ScrollPhase::Ended => 3,
        ScrollPhase::Cancelled => 4,
    }
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
            vivid_sdk::OverlayLaneEvent::Accessibility { node, action, .. } => {
                data.kind = "accessibility";
                data.region = *node;
                data.text = accessible_action_name(*action).to_owned();
            }
            vivid_sdk::OverlayLaneEvent::Environment(update) => {
                data.kind = "environment";
                data.revision = update.revision;
                data.text = update.environment.font_family.clone();
                let env = &update.environment;
                data.values = vec![
                    env.font_size.get(),
                    // Appearance and the two optional readings are flattened; a negative value
                    // is absence, which the languages turn back into None.
                    if env.appearance == vivid_sdk::overlay::Appearance::Dark {
                        1.
                    } else {
                        0.
                    },
                    env.reduced_motion
                        .map_or(-1., |on| if on { 1. } else { 0. }),
                    env.refresh_interval_us.map_or(-1., |us| us as f64),
                    update.revision as f64,
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
            clicks,
            pressure,
        } => {
            data.kind = "pointer";
            data.region = *region;
            data.values = vec![
                position.x.get(),
                position.y.get(),
                f64::from(*modifiers),
                f64::from(*clicks),
            ];
            if let Some((button, down)) = button {
                data.values
                    .extend([f64::from(*button), u8::from(*down).into()]);
            }
            // Absent pressure is reported as a negative sentinel, which no real value can be.
            data.values.push(pressure.map_or(-1., |p| p.get()));
        }
        Event::Accessibility { node, action } => {
            data.kind = "accessibility";
            data.region = *node;
            data.text = accessible_action_name(*action).to_owned();
        }
        Event::Hover { region, entered } => {
            data.kind = "hover";
            data.region = *region;
            data.values = vec![u8::from(*entered).into()];
        }
        Event::Wheel {
            position,
            dx,
            dy,
            modifiers,
            precise,
            phase,
        } => {
            data.kind = "wheel";
            data.values = vec![
                position.x.get(),
                position.y.get(),
                dx.get(),
                dy.get(),
                f64::from(*modifiers),
                u8::from(*precise).into(),
                f64::from(scroll_phase(*phase)),
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
