//! Pane overlays: frameless vector windows drawn and hit tested by the host.
//!
//! Validation is shared with the Python and Node bindings through `bindings/overlay_model.rs`,
//! so a brush, stroke style, shadow, cursor, or semantic tree is refused for the same reasons in
//! every language. Text offsets reported to Lua are UTF-8 byte offsets, which is how Lua indexes a
//! string: `text:sub(first + 1, last)`.

use std::sync::{Arc, Weak};

use mlua::prelude::*;
use vivid_protocol::vector::Segment;
use vivid_sdk::overlay::{
    Brush, Canvas, Command, Path, PathBuilder, Point, Rect, RetainedImage, RetainedTextLayout,
    Scalar, StyledText, Text, TextGeometry, TextMeasurement,
};
use vivid_sdk::{OverlayLaneEvent, OverlaySession, OverlaySubmission, OverlayWindow};

use crate::convert::{Field, arg, check_keys, get, need, opt, options, sequence};
use crate::error::{IoResultExt, closed, invalid};
use crate::session::{LuaSession, producer_config};

// The shared model serves three bindings; Lua uses the parts that take its values directly.
#[allow(dead_code)]
#[path = "../../bindings/overlay_model.rs"]
mod model;

/// The shared model only validates, so everything it refuses was refused before any request.
fn model_error(error: std::io::Error) -> LuaError {
    invalid(error)
}

fn scene_error(error: impl std::fmt::Display) -> LuaError {
    invalid(error)
}

// ---------------------------------------------------------------------------
// Geometry tables
// ---------------------------------------------------------------------------

fn number(table: &LuaTable, name: &str) -> LuaResult<f64> {
    need::<f64>(table, name)
}

/// `{ x = .., y = .., width = .., height = .. }` as the model's flattened rectangle.
fn rect_values(value: LuaValue, name: &str) -> LuaResult<Vec<f64>> {
    let table = LuaTable::from_value(value, name)?;
    check_keys(&table, &["x", "y", "width", "height"], "rectangle")?;
    Ok(vec![
        number(&table, "x")?,
        number(&table, "y")?,
        number(&table, "width")?,
        number(&table, "height")?,
    ])
}

fn rect(value: LuaValue, name: &str) -> LuaResult<Rect> {
    model::rect(&rect_values(value, name)?).map_err(model_error)
}

fn point_values(value: LuaValue, name: &str) -> LuaResult<(f64, f64)> {
    let table = LuaTable::from_value(value, name)?;
    check_keys(&table, &["x", "y"], "point")?;
    Ok((number(&table, "x")?, number(&table, "y")?))
}

fn point(value: LuaValue, name: &str) -> LuaResult<Point> {
    let (x, y) = point_values(value, name)?;
    Point::new(x, y).map_err(scene_error)
}

fn rect_table(lua: &Lua, rect: Rect) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("x", rect.origin.x.get())?;
    table.set("y", rect.origin.y.get())?;
    table.set("width", rect.width.get())?;
    table.set("height", rect.height.get())?;
    Ok(table)
}

fn point_table(lua: &Lua, x: f64, y: f64) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("x", x)?;
    table.set("y", y)?;
    Ok(table)
}

fn viewport_table(lua: &Lua, viewport: vivid_sdk::overlay::Viewport) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("width", viewport.width.get())?;
    table.set("height", viewport.height.get())?;
    table.set("scale_numerator", viewport.scale_numerator)?;
    table.set("scale_denominator", viewport.scale_denominator)?;
    Ok(table)
}

// ---------------------------------------------------------------------------
// Path
// ---------------------------------------------------------------------------

/// A mutable path builder. Canvas commands snapshot the path when they are added. A coordinate
/// the wire cannot carry is remembered, and reported when the path is used.
pub struct LuaPath {
    builder: PathBuilder,
}

impl LuaPath {
    fn path(&self) -> LuaResult<Path> {
        self.builder.clone().build().map_err(scene_error)
    }

    /// A finished shape replayed into a builder, so a shape can be extended like any other path.
    fn from_path(path: Path) -> Self {
        let mut builder = Path::builder();
        if path.even_odd {
            builder = builder.even_odd();
        }
        for segment in path.segments {
            builder = match segment {
                Segment::Move(p) => builder.move_to(p.x.get(), p.y.get()),
                Segment::Line(p) => builder.line_to(p.x.get(), p.y.get()),
                Segment::Quad(c, p) => builder.quad_to(c.x.get(), c.y.get(), p.x.get(), p.y.get()),
                Segment::Cubic(a, b, p) => builder.cubic_to(
                    a.x.get(),
                    a.y.get(),
                    b.x.get(),
                    b.y.get(),
                    p.x.get(),
                    p.y.get(),
                ),
                Segment::Close => builder.close(),
            };
        }
        Self { builder }
    }
}

fn path_ref(value: LuaValue, name: &str) -> LuaResult<Path> {
    match value {
        LuaValue::UserData(ud) => ud
            .borrow::<LuaPath>()
            .map_err(|_| invalid(format!("{name} must be an overlay Path")))?
            .path(),
        _ => Err(invalid(format!("{name} must be an overlay Path"))),
    }
}

impl LuaUserData for LuaPath {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("length", |_, this| Ok(this.builder.len()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.overlay.Path(segments={})",
                this.builder.len()
            ))
        });
        fn step(
            ud: &LuaAnyUserData,
            apply: impl FnOnce(PathBuilder) -> PathBuilder,
        ) -> LuaResult<LuaAnyUserData> {
            {
                let mut path = ud.borrow_mut::<LuaPath>()?;
                let builder = std::mem::take(&mut path.builder);
                path.builder = apply(builder);
            }
            Ok(ud.clone())
        }
        methods.add_function(
            "move_to",
            |_, (ud, x, y): (LuaAnyUserData, LuaValue, LuaValue)| {
                let (x, y) = (f64::from_value(x, "x")?, f64::from_value(y, "y")?);
                step(&ud, |path| path.move_to(x, y))
            },
        );
        methods.add_function(
            "line_to",
            |_, (ud, x, y): (LuaAnyUserData, LuaValue, LuaValue)| {
                let (x, y) = (f64::from_value(x, "x")?, f64::from_value(y, "y")?);
                step(&ud, |path| path.line_to(x, y))
            },
        );
        methods.add_function(
            "quad_to",
            |_, (ud, cx, cy, x, y): (LuaAnyUserData, LuaValue, LuaValue, LuaValue, LuaValue)| {
                let values = [
                    f64::from_value(cx, "cx")?,
                    f64::from_value(cy, "cy")?,
                    f64::from_value(x, "x")?,
                    f64::from_value(y, "y")?,
                ];
                step(&ud, |path| {
                    path.quad_to(values[0], values[1], values[2], values[3])
                })
            },
        );
        methods.add_function(
            "cubic_to",
            |_, (ud, values): (LuaAnyUserData, LuaMultiValue)| {
                let values = values
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| {
                        f64::from_value(value, &format!("coordinate {}", index + 1))
                    })
                    .collect::<LuaResult<Vec<_>>>()?;
                let [ax, ay, bx, by, x, y] = values[..] else {
                    return Err(invalid("cubic_to takes six coordinates"));
                };
                step(&ud, |path| path.cubic_to(ax, ay, bx, by, x, y))
            },
        );
        methods.add_function("close", |_, ud: LuaAnyUserData| {
            step(&ud, PathBuilder::close)
        });
    }
}

fn path_new(_: &Lua, value: LuaValue) -> LuaResult<LuaPath> {
    let even_odd = match value {
        LuaValue::Nil => false,
        value => {
            let config = LuaTable::from_value(value, "path options")?;
            check_keys(&config, &["even_odd"], "path option")?;
            get(&config, "even_odd")?.unwrap_or(false)
        }
    };
    let builder = Path::builder();
    Ok(LuaPath {
        builder: if even_odd {
            builder.even_odd()
        } else {
            builder
        },
    })
}

fn shape(kind: &str, value: LuaValue, extra: LuaValue) -> LuaResult<LuaPath> {
    let bounds = rect(value, "bounds")?;
    let path = match kind {
        "rectangle" => Path::rectangle(bounds),
        "rounded" => Path::rounded_rectangle(bounds, f64::from_value(extra, "radius")?),
        "rounded-corners" => {
            let radii = Vec::<f64>::from_value(extra, "radii")?;
            let radii: [f64; 4] = radii
                .try_into()
                .map_err(|_| invalid("rounded corners require four radii"))?;
            Path::rounded_rectangle_corners(
                bounds,
                vivid_sdk::overlay::Corners::new(radii).map_err(scene_error)?,
            )
        }
        _ => Path::ellipse(bounds),
    }
    .map_err(scene_error)?;
    Ok(LuaPath::from_path(path))
}

// ---------------------------------------------------------------------------
// Brush
// ---------------------------------------------------------------------------

/// A paint, validated when it is made. Colors are straight-alpha sRGB `0xRRGGBBAA`.
pub struct LuaBrush {
    inner: Brush,
}

impl LuaUserData for LuaBrush {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.overlay.Brush({})",
                match this.inner {
                    Brush::Solid(_) => "solid",
                    Brush::Linear { .. } => "linear",
                    Brush::Radial { .. } => "radial",
                    Brush::Image { .. } => "image",
                }
            ))
        });
    }
}

fn brush_ref(value: LuaValue, name: &str) -> LuaResult<Brush> {
    match value {
        LuaValue::UserData(ud) => Ok(ud
            .borrow::<LuaBrush>()
            .map_err(|_| invalid(format!("{name} must be an overlay Brush")))?
            .inner
            .clone()),
        _ => Err(invalid(format!("{name} must be an overlay Brush"))),
    }
}

fn color(value: LuaValue, name: &str) -> LuaResult<f64> {
    Ok(u32::from_value(value, name)? as f64)
}

/// Gradient stops as the model's parallel color and offset lists.
fn stops(value: LuaValue) -> LuaResult<(Vec<f64>, Vec<f64>)> {
    let list = LuaTable::from_value(value, "stops")?;
    let mut colors = Vec::new();
    let mut offsets = Vec::new();
    for item in sequence(&list, "stops")? {
        let stop = LuaTable::from_value(item, "gradient stop")?;
        check_keys(&stop, &["offset", "color"], "gradient stop")?;
        offsets.push(need::<f64>(&stop, "offset")?);
        colors.push(color(stop.get("color")?, "color")?);
    }
    Ok((colors, offsets))
}

fn gradient(
    kind: &str,
    geometry: &[f64],
    stops_value: LuaValue,
    space: LuaValue,
) -> LuaResult<LuaBrush> {
    let (colors, offsets) = stops(stops_value)?;
    let space = opt::<String>(space, "space")?.unwrap_or_else(|| "srgb".into());
    let inner = if space == "srgb" {
        model::brush(kind, geometry, &colors, &offsets)
    } else {
        model::gradient_brush(kind, geometry, &colors, &offsets, &space)
    }
    .map_err(model_error)?;
    Ok(LuaBrush { inner })
}

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

/// A display list. Every method returns the canvas, so drawing chains.
pub struct LuaCanvas {
    inner: Canvas,
}

fn push(ud: &LuaAnyUserData, command: Command) -> LuaResult<LuaAnyUserData> {
    ud.borrow_mut::<LuaCanvas>()?
        .inner
        .push(command)
        .map_err(scene_error)?;
    Ok(ud.clone())
}

fn text_command(
    text: String,
    origin: Point,
    size: f64,
    color_value: u32,
    options: &LuaTable,
) -> LuaResult<Text> {
    check_keys(
        options,
        &["family", "weight", "italic", "max_width"],
        "text option",
    )?;
    if text.len() > vivid_protocol::vector::MAX_TEXT_BYTES {
        return Err(invalid("text limit exceeded"));
    }
    Ok(Text {
        text,
        origin,
        size: Scalar::new(size).map_err(scene_error)?,
        color: vivid_sdk::overlay::Color(color_value),
        family: get(options, "family")?.unwrap_or_default(),
        weight: get(options, "weight")?.unwrap_or(400),
        italic: get(options, "italic")?.unwrap_or(false),
        max_width: get::<f64>(options, "max_width")?
            .map(Scalar::new)
            .transpose()
            .map_err(scene_error)?,
    })
}

impl LuaUserData for LuaCanvas {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("length", |_, this| Ok(this.inner.commands().len()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.overlay.Canvas(commands={})",
                this.inner.commands().len()
            ))
        });
        methods.add_method("snapshot", |_, this, ()| {
            Ok(LuaCanvas {
                inner: this.inner.clone(),
            })
        });
        methods.add_method("validate", |_, this, ()| {
            this.inner.validate().map_err(scene_error)
        });
        methods.add_function(
            "fill",
            |_, (ud, path, brush): (LuaAnyUserData, LuaValue, LuaValue)| {
                let command = Command::Fill(path_ref(path, "path")?, brush_ref(brush, "brush")?);
                push(&ud, command)
            },
        );
        // A plain stroke. Caps, joins, and dashes are `stroke_styled`, which needs
        // overlay-paint-v1; this one does not.
        methods.add_function(
            "stroke",
            |_, (ud, path, brush, width): (LuaAnyUserData, LuaValue, LuaValue, LuaValue)| {
                let width = Scalar::new(f64::from_value(width, "width")?).map_err(scene_error)?;
                let command =
                    Command::Stroke(path_ref(path, "path")?, brush_ref(brush, "brush")?, width);
                push(&ud, command)
            },
        );
        methods.add_function(
            "stroke_styled",
            |_, (ud, path, brush, style): (LuaAnyUserData, LuaValue, LuaValue, LuaValue)| {
                let style = LuaTable::from_value(style, "stroke style")?;
                check_keys(
                    &style,
                    &[
                        "width",
                        "cap",
                        "join",
                        "miter_limit",
                        "dashes",
                        "dash_offset",
                    ],
                    "stroke style",
                )?;
                let style = model::stroke_style(
                    need(&style, "width")?,
                    &get::<String>(&style, "cap")?.unwrap_or_else(|| "butt".into()),
                    &get::<String>(&style, "join")?.unwrap_or_else(|| "miter".into()),
                    get(&style, "miter_limit")?.unwrap_or(4.0),
                    &get::<Vec<f64>>(&style, "dashes")?.unwrap_or_default(),
                    get(&style, "dash_offset")?.unwrap_or(0.0),
                )
                .map_err(model_error)?;
                let command = Command::StyledStroke(
                    path_ref(path, "path")?,
                    brush_ref(brush, "brush")?,
                    style,
                );
                push(&ud, command)
            },
        );
        // One blurred rounded rectangle, as CSS box-shadow defines it. Radii run clockwise from
        // the top left.
        methods.add_function("shadow", |_, (ud, shadow): (LuaAnyUserData, LuaValue)| {
            let shadow = LuaTable::from_value(shadow, "shadow")?;
            check_keys(
                &shadow,
                &[
                    "rect", "radii", "color", "offset", "blur", "spread", "inset",
                ],
                "shadow",
            )?;
            let mut values = rect_values(shadow.get("rect")?, "rect")?;
            values.extend(get::<Vec<f64>>(&shadow, "radii")?.unwrap_or_else(|| vec![0.0; 4]));
            let (x, y) = match shadow.get::<LuaValue>("offset")? {
                LuaValue::Nil => (0.0, 0.0),
                offset => point_values(offset, "offset")?,
            };
            values.extend([
                x,
                y,
                get(&shadow, "blur")?.unwrap_or(0.0),
                get(&shadow, "spread")?.unwrap_or(0.0),
                if get(&shadow, "inset")?.unwrap_or(false) {
                    1.0
                } else {
                    0.0
                },
            ]);
            let color = get::<u32>(&shadow, "color")?.unwrap_or(0x0000_00FF);
            push(&ud, model::shadow(&values, color).map_err(model_error)?)
        });
        methods.add_function("save", |_, ud: LuaAnyUserData| {
            push(&ud, model::state("save", &[]).map_err(model_error)?)
        });
        methods.add_function("restore", |_, ud: LuaAnyUserData| {
            push(&ud, model::state("restore", &[]).map_err(model_error)?)
        });
        methods.add_function("opacity", |_, (ud, value): (LuaAnyUserData, LuaValue)| {
            let value = f64::from_value(value, "opacity")?;
            push(&ud, model::state("opacity", &[value]).map_err(model_error)?)
        });
        methods.add_function(
            "transform",
            |_, (ud, values): (LuaAnyUserData, LuaMultiValue)| {
                let values = values
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| f64::from_value(value, &format!("term {}", index + 1)))
                    .collect::<LuaResult<Vec<_>>>()?;
                push(
                    &ud,
                    model::state("transform", &values).map_err(model_error)?,
                )
            },
        );
        methods.add_function("clip", |_, (ud, path): (LuaAnyUserData, LuaValue)| {
            push(&ud, Command::Clip(path_ref(path, "path")?))
        });
        methods.add_function(
            "text",
            |lua,
             (ud, text, origin, size, color_value, value): (
                LuaAnyUserData,
                LuaValue,
                LuaValue,
                LuaValue,
                LuaValue,
                LuaValue,
            )| {
                let options = options(lua, value, "text options")?;
                let text = text_command(
                    arg(text, "text")?,
                    point(origin, "origin")?,
                    arg(size, "size")?,
                    arg(color_value, "color")?,
                    &options,
                )?;
                push(&ud, Command::Text(text))
            },
        );
        // Resize edges: left = 1, top = 2, right = 4, bottom = 8. `cursor` is the shape shown while
        // the region is hovered; leaving it out leaves the host's own and needs no pointer profile.
        methods.add_function(
            "hit",
            |lua,
             (ud, id, path, role, value): (
                LuaAnyUserData,
                LuaValue,
                LuaValue,
                LuaValue,
                LuaValue,
            )| {
                let options = options(lua, value, "hit options")?;
                check_keys(&options, &["edges", "cursor"], "hit option")?;
                let role = opt::<String>(role, "role")?.unwrap_or_else(|| "input".into());
                let edges = get::<u8>(&options, "edges")?.unwrap_or(0);
                let command = Command::Hit {
                    id: arg(id, "application_id")?,
                    path: path_ref(path, "path")?,
                    role: model::hit_role(&role, f64::from(edges)).map_err(model_error)?,
                    cursor: model::cursor(&get::<String>(&options, "cursor")?.unwrap_or_default())
                        .map_err(model_error)?,
                };
                push(&ud, command)
            },
        );
    }
}

fn canvas_value(value: LuaValue, name: &str) -> LuaResult<Canvas> {
    match value {
        LuaValue::UserData(ud) => Ok(ud
            .borrow::<LuaCanvas>()
            .map_err(|_| invalid(format!("{name} must be an overlay Canvas")))?
            .inner
            .clone()),
        _ => Err(invalid(format!("{name} must be an overlay Canvas"))),
    }
}

// ---------------------------------------------------------------------------
// Styled text and measurements
// ---------------------------------------------------------------------------

/// Styled text from its Lua description, validated by the shared model before any request.
fn styled_text(value: LuaValue) -> LuaResult<StyledText> {
    let spec = LuaTable::from_value(value, "styled text")?;
    check_keys(
        &spec,
        &[
            "runs",
            "max_width",
            "alignment",
            "wrap",
            "max_lines",
            "overflow",
            "letter_spacing",
            "word_spacing",
            "line_height",
            "ligatures",
            "kerning",
        ],
        "styled text",
    )?;
    let mut canvas = Canvas::new();
    let mut decorations = Vec::new();
    for item in sequence(&need::<LuaTable>(&spec, "runs")?, "runs")? {
        let run = LuaTable::from_value(item, "text run")?;
        check_keys(&run, &["text", "style"], "text run")?;
        let style = match run.get::<LuaValue>("style")? {
            LuaValue::Nil => None,
            value => Some(LuaTable::from_value(value, "text style")?),
        };
        let field = |name: &str| -> LuaResult<LuaValue> {
            match &style {
                Some(style) => style.get(name),
                None => Ok(LuaValue::Nil),
            }
        };
        if let Some(style) = &style {
            check_keys(
                style,
                &[
                    "size",
                    "family",
                    "weight",
                    "italic",
                    "color",
                    "underline",
                    "strikethrough",
                ],
                "text style",
            )?;
        }
        let text = need::<String>(&run, "text")?;
        if text.len() > vivid_protocol::vector::MAX_TEXT_BYTES {
            return Err(invalid("text limit exceeded"));
        }
        canvas
            .push(Command::Text(Text {
                text,
                origin: Point::new(0.0, 0.0).map_err(scene_error)?,
                size: Scalar::new(opt::<f64>(field("size")?, "size")?.unwrap_or(16.0))
                    .map_err(scene_error)?,
                color: vivid_sdk::overlay::Color(
                    opt::<u32>(field("color")?, "color")?.unwrap_or(0xFFFF_FFFF),
                ),
                family: opt(field("family")?, "family")?.unwrap_or_default(),
                weight: opt(field("weight")?, "weight")?.unwrap_or(400),
                italic: opt(field("italic")?, "italic")?.unwrap_or(false),
                max_width: None,
            }))
            .map_err(scene_error)?;
        let underline = opt::<bool>(field("underline")?, "underline")?.unwrap_or(false);
        let strikethrough = opt::<bool>(field("strikethrough")?, "strikethrough")?.unwrap_or(false);
        decorations.push(u32::from(underline) | (u32::from(strikethrough) << 1));
    }
    let mut text = model::styled_text(
        &canvas,
        &decorations,
        get(&spec, "max_width")?,
        &get::<String>(&spec, "alignment")?.unwrap_or_else(|| "start".into()),
        get(&spec, "wrap")?.unwrap_or(true),
        get(&spec, "max_lines")?,
    )
    .map_err(model_error)?;
    text.typography = model::typography(
        &get::<String>(&spec, "overflow")?.unwrap_or_else(|| "clip".into()),
        get(&spec, "letter_spacing")?.unwrap_or(0.0),
        get(&spec, "word_spacing")?.unwrap_or(0.0),
        get(&spec, "line_height")?,
        get(&spec, "ligatures")?.unwrap_or(true),
        get(&spec, "kerning")?.unwrap_or(true),
    )
    .map_err(model_error)?;
    text.validate().map_err(scene_error)?;
    Ok(text)
}

fn geometry_list(lua: &Lua, values: &[TextGeometry]) -> LuaResult<LuaTable> {
    let list = lua.create_table_with_capacity(values.len(), 0)?;
    for geometry in values {
        let entry = lua.create_table()?;
        // UTF-8 byte offsets into the measured text, first inclusive and last exclusive.
        entry.set("start", geometry.start)?;
        entry.set("end", geometry.end)?;
        let bounds = lua.create_table()?;
        bounds.set("x", geometry.x.get())?;
        bounds.set("y", geometry.y.get())?;
        bounds.set("width", geometry.width.get())?;
        bounds.set("height", geometry.height.get())?;
        entry.set("bounds", bounds)?;
        entry.set("baseline", geometry.baseline.get())?;
        entry.set("rtl", geometry.rtl)?;
        list.push(entry)?;
    }
    Ok(list)
}

fn measurement(lua: &Lua, measured: &TextMeasurement) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("width", measured.width.get())?;
    table.set("height", measured.height.get())?;
    table.set("lines", geometry_list(lua, &measured.lines)?)?;
    table.set("clusters", geometry_list(lua, &measured.clusters)?)?;
    table.set("truncated_at", measured.truncated_at)?;
    Ok(table)
}

/// An opaque, immutable host layout. Release it through the window that made it.
pub struct LuaTextLayout {
    inner: RetainedTextLayout,
}

impl LuaUserData for LuaTextLayout {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("measurement", |lua, this| {
            measurement(lua, this.inner.measurement())
        });
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| {
            Ok("vivid_sdk.overlay.TextLayout")
        });
    }
}

// ---------------------------------------------------------------------------
// Session and events
// ---------------------------------------------------------------------------

pub struct LuaOverlaySession {
    inner: Arc<OverlaySession>,
    closed: bool,
}

impl LuaOverlaySession {
    fn get(&self) -> LuaResult<&Arc<OverlaySession>> {
        if self.closed {
            return Err(closed("overlay session"));
        }
        Ok(&self.inner)
    }
}

/// `vivid.overlay.connect(options)`: a session offering the complete overlay profile bundle,
/// which the SDK owns. Options are `vivid.connect` options.
fn connect(lua: &Lua, value: LuaValue) -> LuaResult<LuaOverlaySession> {
    let config = producer_config(&options(lua, value, "connect options")?)?;
    Ok(LuaOverlaySession {
        inner: Arc::new(OverlaySession::connect(config).lua()?),
        closed: false,
    })
}

/// `vivid.overlay.from_session(session)`: adopt an established session that negotiated overlays.
fn from_session(_: &Lua, session: LuaUserDataRefMut<LuaSession>) -> LuaResult<LuaOverlaySession> {
    let mut session = session;
    let owned = session.take()?;
    Ok(LuaOverlaySession {
        inner: Arc::new(OverlaySession::from_session(owned).lua()?),
        closed: false,
    })
}

fn window_options(value: LuaValue) -> LuaResult<vivid_sdk::OverlayWindowOptions> {
    let config = LuaTable::from_value(value, "window options")?;
    check_keys(
        &config,
        &[
            "bounds",
            "mode",
            "title",
            "visible",
            "min_width",
            "min_height",
        ],
        "window option",
    )?;
    model::options(
        &rect_values(config.get("bounds")?, "bounds")?,
        &get::<String>(&config, "mode")?.unwrap_or_else(|| "floating".into()),
        get(&config, "title")?.unwrap_or_default(),
        get(&config, "visible")?.unwrap_or(true),
        get(&config, "min_width")?.unwrap_or(1.0),
        get(&config, "min_height")?.unwrap_or(1.0),
    )
    .map_err(model_error)
}

const SCROLL_PHASES: [&str; 5] = ["none", "began", "changed", "ended", "cancelled"];

/// One lane event as a table named by `kind`, with `event:targets(window)` telling which window of
/// this session it belongs to: one session may own many windows.
fn overlay_event(
    lua: &Lua,
    owner: Weak<OverlaySession>,
    event: OverlayLaneEvent,
) -> LuaResult<LuaTable> {
    let data = model::event_data(&event);
    let table = lua.create_table()?;
    let values = &data.values;
    let flag = |index: usize| values.get(index).is_some_and(|value| *value != 0.0);
    table.set("kind", data.kind)?;
    table.set("scene_revision", data.revision)?;
    match data.kind {
        "viewport" => {
            table.set("scene_revision", 0)?;
            table.set("revision", data.revision)?;
            let viewport = lua.create_table()?;
            viewport.set("width", values[0])?;
            viewport.set("height", values[1])?;
            viewport.set("scale_numerator", values[2] as u32)?;
            viewport.set("scale_denominator", values[3] as u32)?;
            table.set("viewport", viewport)?;
        }
        "submission-outcome" => table.set("outcome", data.text.as_str())?,
        "pointer" => {
            // [x, y, modifiers, clicks], an optional [button, down] pair, then the pressure,
            // negative when the device reported none.
            table.set("position", point_table(lua, values[0], values[1])?)?;
            table.set("application_id", data.region)?;
            table.set("modifiers", values[2] as u32)?;
            table.set("clicks", values[3] as u32)?;
            if values.len() > 5 {
                table.set("button", values[4] as u32)?;
                table.set("down", flag(5))?;
            }
            let pressure = values[values.len() - 1];
            if pressure >= 0.0 {
                table.set("pressure", pressure)?;
            }
        }
        "hover" => {
            table.set("application_id", data.region)?;
            table.set("entered", flag(0))?;
        }
        "accessibility" => {
            table.set("application_id", data.region)?;
            table.set("action", data.text.as_str())?;
        }
        "environment" => {
            // [font size, dark flag, reduced motion, refresh interval, revision]; a negative
            // reading is the host saying it cannot tell, which is not "false".
            let environment = lua.create_table()?;
            environment.set("font_family", data.text.as_str())?;
            environment.set("font_size", values[0])?;
            environment.set("appearance", if flag(1) { "dark" } else { "light" })?;
            if values[2] >= 0.0 {
                environment.set("reduced_motion", values[2] != 0.0)?;
            }
            if values[3] >= 0.0 {
                environment.set("refresh_interval_us", values[3])?;
            }
            table.set("environment", environment)?;
            table.set("revision", values[4] as u64)?;
        }
        "wheel" => {
            table.set("position", point_table(lua, values[0], values[1])?)?;
            table.set("dx", values[2])?;
            table.set("dy", values[3])?;
            table.set("modifiers", values[4] as u32)?;
            table.set("precise", flag(5))?;
            table.set(
                "phase",
                SCROLL_PHASES
                    .get(values[6] as usize)
                    .copied()
                    .unwrap_or("none"),
            )?;
        }
        "key" => {
            table.set("physical", values[0] as u32)?;
            table.set("down", flag(1))?;
            table.set("repeat", flag(2))?;
            table.set("modifiers", values[3] as u32)?;
        }
        "text" => table.set("text", data.text.as_str())?,
        "ime" => {
            table.set("preedit", data.text.as_str())?;
            if let [first, last] = values[..] {
                // UTF-8 byte offsets into `preedit`.
                let selection = lua.create_table()?;
                selection.set("start", first as u32)?;
                selection.set("end", last as u32)?;
                table.set("selection", selection)?;
            }
        }
        "geometry" => {
            let bounds = lua.create_table()?;
            bounds.set("x", values[0])?;
            bounds.set("y", values[1])?;
            bounds.set("width", values[2])?;
            bounds.set("height", values[3])?;
            table.set("bounds", bounds)?;
            table.set("settled", flag(4))?;
        }
        "focus" => table.set("focused", flag(0))?,
        "dismissed" => table.set("reason", data.text.as_str())?,
        "connection-lost" => table.set("diagnostic", data.text.as_str())?,
        _ => {}
    }
    let input = match event {
        OverlayLaneEvent::Input(input) => Some(input),
        _ => None,
    };
    table.set(
        "targets",
        lua.create_function(
            move |_, (_, window): (LuaValue, LuaUserDataRef<LuaOverlayWindow>)| match (
                &input,
                owner.upgrade(),
            ) {
                (Some(input), Some(owner)) => owner.event_targets(input, &window.inner).lua(),
                _ => Ok(false),
            },
        )?,
    )?;
    Ok(table)
}

impl LuaUserData for LuaOverlaySession {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("closed", |_, this| Ok(this.closed));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.overlay.OverlaySession(closed={})",
                this.closed
            ))
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| {
            if !this.closed {
                this.closed = true;
                this.inner.close().lua()?;
            }
            Ok(())
        });
        methods.add_method_mut("close", |_, this, ()| {
            if !this.closed {
                this.closed = true;
                this.inner.close().lua()?;
            }
            Ok(())
        });
        methods.add_method(
            "create_window",
            |_, this, (value, parent): (LuaValue, Option<LuaUserDataRef<LuaOverlayWindow>>)| {
                let options = window_options(value)?;
                let session = this.get()?;
                let window = match parent {
                    Some(parent) => session.create_child(&parent.inner, options),
                    None => session.create_window(options),
                }
                .lua()?;
                Ok(LuaOverlayWindow {
                    inner: Arc::new(window),
                    closed: false,
                })
            },
        );
        methods.add_method(
            "capture_pointer",
            |_, this, (window, capture): (LuaUserDataRef<LuaOverlayWindow>, LuaValue)| {
                let capture = opt::<bool>(capture, "capture")?.unwrap_or(true);
                this.get()?.capture_pointer(&window.inner, capture).lua()
            },
        );
        // Wait at most `timeout` seconds (0 to 60, default 0.25), independently of bulk traffic.
        methods.add_method("wait_event", |lua, this, value: LuaValue| {
            if this.closed {
                return Ok(LuaValue::Nil);
            }
            let wait = model::timeout(opt::<f64>(value, "timeout")?.unwrap_or(0.25))
                .map_err(model_error)?;
            match this.inner.wait_event(wait).lua()? {
                Some(event) => {
                    overlay_event(lua, Arc::downgrade(&this.inner), event).map(LuaValue::Table)
                }
                None => Ok(LuaValue::Nil),
            }
        });
        // Iterate events until the session closes or its connection is lost; a quiet `timeout`
        // does not end the iteration, since an interactive window is usually quiet.
        methods.add_function("events", |lua, (ud, value): (LuaAnyUserData, LuaValue)| {
            let wait = model::timeout(opt::<f64>(value, "timeout")?.unwrap_or(0.25))
                .map_err(model_error)?;
            ud.borrow::<LuaOverlaySession>()?;
            let mut lost = false;
            lua.create_function_mut(move |lua, _: LuaMultiValue| {
                if lost {
                    return Ok(LuaValue::Nil);
                }
                loop {
                    let session = ud.borrow::<LuaOverlaySession>()?;
                    if session.closed {
                        return Ok(LuaValue::Nil);
                    }
                    if let Some(event) = session.inner.wait_event(wait).lua()? {
                        lost = matches!(event, OverlayLaneEvent::ConnectionLost { .. });
                        return overlay_event(lua, Arc::downgrade(&session.inner), event)
                            .map(LuaValue::Table);
                    }
                }
            })
        });
    }
}

// ---------------------------------------------------------------------------
// Window, images, submissions
// ---------------------------------------------------------------------------

pub struct LuaOverlayWindow {
    inner: Arc<OverlayWindow>,
    closed: bool,
}

pub struct LuaOverlayImage {
    inner: RetainedImage,
}

impl LuaUserData for LuaOverlayImage {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        // The channel-qualified asset identity. Brushes take the image itself, so the identity
        // never has to survive a trip through a Lua number.
        fields.add_field_method_get("id", |_, this| Ok(this.inner.id()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| {
            Ok("vivid_sdk.overlay.RetainedImage")
        });
    }
}

/// A specific submission. A timeout leaves the receipt usable; lane loss raises.
pub struct LuaOverlaySubmission {
    inner: OverlaySubmission,
}

impl LuaUserData for LuaOverlaySubmission {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("revision", |_, this| Ok(this.inner.revision()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.overlay.Submission(revision={})",
                this.inner.revision()
            ))
        });
        methods.add_method("wait", |_, this, value: LuaValue| {
            let wait = model::timeout(opt::<f64>(value, "timeout")?.unwrap_or(0.25))
                .map_err(model_error)?;
            Ok(this.inner.wait(wait).lua()?.map(model::outcome))
        });
    }
}

impl LuaOverlayWindow {
    fn get(&self) -> LuaResult<&OverlayWindow> {
        if self.closed {
            return Err(closed("overlay window"));
        }
        Ok(&self.inner)
    }
}

fn image_ref(value: LuaValue, name: &str) -> LuaResult<RetainedImage> {
    match value {
        LuaValue::UserData(ud) => Ok(ud
            .borrow::<LuaOverlayImage>()
            .map_err(|_| invalid(format!("{name} must a retained overlay image")))?
            .inner
            .clone()),
        _ => Err(invalid(format!("{name} must be a retained overlay image"))),
    }
}

fn styled_list(value: LuaValue) -> LuaResult<Vec<StyledText>> {
    let list = LuaTable::from_value(value, "texts")?;
    sequence(&list, "texts")?
        .into_iter()
        .map(styled_text)
        .collect()
}

fn semantics(value: LuaValue) -> LuaResult<vivid_sdk::overlay::Semantics> {
    let spec = LuaTable::from_value(value, "semantics")?;
    check_keys(&spec, &["scene_revision", "nodes"], "semantics")?;
    let nodes = sequence(&need::<LuaTable>(&spec, "nodes")?, "nodes")?
        .into_iter()
        .map(|item| {
            let node = LuaTable::from_value(item, "semantic node")?;
            check_keys(
                &node,
                &[
                    "id", "role", "bounds", "label", "numeric", "level", "set", "toggled",
                    "disabled", "actions", "children",
                ],
                "semantic node",
            )?;
            Ok(model::SemanticNodeInput {
                id: need(&node, "id")?,
                role: need(&node, "role")?,
                bounds: rect_values(node.get("bounds")?, "bounds")?,
                label: get(&node, "label")?.unwrap_or_default(),
                numeric: get(&node, "numeric")?,
                level: get(&node, "level")?,
                set: get(&node, "set")?,
                toggled: get(&node, "toggled")?,
                disabled: get(&node, "disabled")?.unwrap_or(false),
                actions: get(&node, "actions")?.unwrap_or_default(),
                children: get(&node, "children")?.unwrap_or_default(),
            })
        })
        .collect::<LuaResult<Vec<_>>>()?;
    model::semantics(need(&spec, "scene_revision")?, nodes).map_err(model_error)
}

impl LuaUserData for LuaOverlayWindow {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("closed", |_, this| Ok(this.closed));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "vivid_sdk.overlay.OverlayWindow(closed={})",
                this.closed
            ))
        });
        #[cfg(any(feature = "lua54", feature = "lua55"))]
        methods.add_meta_method_mut(LuaMetaMethod::Close, |_, this, _: LuaMultiValue| {
            if !this.closed {
                this.inner.close().lua()?;
                this.closed = true;
            }
            Ok(())
        });
        methods.add_method_mut("close", |_, this, ()| {
            if !this.closed {
                this.inner.close().lua()?;
                this.closed = true;
            }
            Ok(())
        });
        // Submit an atomic snapshot; success does not acknowledge GPU presentation.
        methods.add_method("present", |_, this, canvas: LuaValue| {
            this.get()?.present(canvas_value(canvas, "canvas")?).lua()
        });
        methods.add_method("submit", |_, this, canvas: LuaValue| {
            Ok(LuaOverlaySubmission {
                inner: this.get()?.submit(canvas_value(canvas, "canvas")?).lua()?,
            })
        });
        // Prime and activate a fresh track; old retained images cannot appear in the canvas.
        methods.add_method("replace_track", |_, this, canvas: LuaValue| {
            Ok(LuaOverlaySubmission {
                inner: this
                    .get()?
                    .replace_track(canvas_value(canvas, "canvas")?)
                    .lua()?,
            })
        });
        methods.add_method("reconcile", |lua, this, ()| {
            let state = this.get()?.reconcile().lua()?;
            let table = lua.create_table()?;
            table.set("bounds", rect_table(lua, state.bounds)?)?;
            table.set("viewport", viewport_table(lua, state.viewport)?)?;
            table.set("viewport_revision", state.viewport_revision)?;
            table.set("window_revision", state.window_revision)?;
            table.set("presented_revision", state.presented_revision)?;
            table.set("accepted_revision", state.accepted_revision)?;
            table.set("active_revision", state.active_revision)?;
            table.set("focused", state.focused)?;
            Ok(table)
        });
        methods.add_method("set_bounds", |_, this, bounds: LuaValue| {
            this.get()?.set_bounds(rect(bounds, "bounds")?).lua()
        });
        methods.add_method("set_visible", |_, this, visible: LuaValue| {
            this.get()?.set_visible(arg(visible, "visible")?).lua()
        });
        methods.add_method("center", |_, this, ()| this.get()?.center().lua());
        methods.add_method("request_focus", |_, this, ()| {
            this.get()?.request_focus().lua()
        });
        methods.add_method("raise", |_, this, ()| this.get()?.raise().lua());
        methods.add_method("lower", |_, this, ()| this.get()?.lower().lua());
        methods.add_method("bounds", |lua, this, ()| {
            rect_table(lua, this.get()?.bounds().lua()?)
        });
        methods.add_method("viewport", |lua, this, ()| {
            viewport_table(lua, this.get()?.viewport().lua()?)
        });
        methods.add_method(
            "upload_rgba",
            |_, this, (width, height, rgba): (LuaValue, LuaValue, LuaString)| {
                let image = this
                    .get()?
                    .upload_rgba(
                        arg(width, "width")?,
                        arg(height, "height")?,
                        &rgba.as_bytes(),
                    )
                    .lua()?;
                Ok(LuaOverlayImage { inner: image })
            },
        );
        methods.add_method("release_image", |_, this, image: LuaValue| {
            this.get()?.release_image(&image_ref(image, "image")?).lua()
        });
        methods.add_method(
            "draw_image",
            |_, this, (canvas, image, bounds, opacity): (LuaAnyUserData, LuaValue, LuaValue, LuaValue)| {
                let image = image_ref(image, "image")?;
                let bounds = rect(bounds, "bounds")?;
                let opacity = opt::<f64>(opacity, "opacity")?.unwrap_or(1.0);
                let Command::Opacity(opacity) =
                    model::state("opacity", &[opacity]).map_err(model_error)?
                else {
                    unreachable!("the opacity state command is an opacity");
                };
                let window = this.get()?;
                let mut canvas = canvas
                    .borrow_mut::<LuaCanvas>()
                    .map_err(|_| invalid("canvas must be an overlay Canvas"))?;
                window
                    .draw_image(&mut canvas.inner, &image, bounds, opacity)
                    .lua()
            },
        );
        methods.add_method(
            "measure_text",
            |lua, this, (text, size, value): (LuaValue, LuaValue, LuaValue)| {
                let options = options(lua, value, "text options")?;
                let text = text_command(
                    arg(text, "text")?,
                    Point::new(0.0, 0.0).map_err(scene_error)?,
                    arg(size, "size")?,
                    0xFFFF_FFFF,
                    &options,
                )?;
                measurement(lua, &this.get()?.measure_text(&text).lua()?)
            },
        );
        methods.add_method("measure_text_batch", |lua, this, texts: LuaValue| {
            let texts = styled_list(texts)?;
            let measured = this.get()?.measure_text_batch(&texts).lua()?;
            let list = lua.create_table_with_capacity(measured.len(), 0)?;
            for measured in &measured {
                list.push(measurement(lua, measured)?)?;
            }
            Ok(list)
        });
        methods.add_method("layout_text_batch", |lua, this, texts: LuaValue| {
            let texts = styled_list(texts)?;
            let layouts = this.get()?.layout_text_batch(&texts).lua()?;
            let list = lua.create_table_with_capacity(layouts.len(), 0)?;
            for layout in layouts {
                list.push(LuaTextLayout { inner: layout })?;
            }
            Ok(list)
        });
        methods.add_method("layout_text", |_, this, text: LuaValue| {
            Ok(LuaTextLayout {
                inner: this.get()?.layout_text(&styled_text(text)?).lua()?,
            })
        });
        methods.add_method(
            "draw_text_layout",
            |_,
             this,
             (canvas, layout, origin): (
                LuaAnyUserData,
                LuaUserDataRef<LuaTextLayout>,
                LuaValue,
            )| {
                let origin = point(origin, "origin")?;
                let window = this.get()?;
                let mut canvas = canvas
                    .borrow_mut::<LuaCanvas>()
                    .map_err(|_| invalid("canvas must be an overlay Canvas"))?;
                window
                    .draw_text_layout(&mut canvas.inner, &layout.inner, origin)
                    .lua()
            },
        );
        methods.add_method(
            "release_text_layout",
            |_, this, layout: LuaUserDataRef<LuaTextLayout>| {
                this.get()?.release_text_layout(&layout.inner).lua()
            },
        );
        // Publish this window's accessibility tree for the scene revision it describes. The host
        // refuses a stale revision, so assistive technology never hears of a control not on screen.
        methods.add_method("set_semantics", |_, this, value: LuaValue| {
            this.get()?.set_semantics(&semantics(value)?).lua()
        });
        // Honored only for a focused window just after a key or pointer press delivered there.
        methods.add_method("set_clipboard", |_, this, text: LuaValue| {
            this.get()?
                .set_clipboard(&arg::<String>(text, "text")?)
                .lua()
        });
        methods.add_method(
            "set_editor_geometry",
            |_, this, (revision, caret): (LuaValue, LuaValue)| {
                let caret = match caret {
                    LuaValue::Nil => None,
                    caret => Some(rect(caret, "caret")?),
                };
                this.get()?
                    .set_editor_geometry(arg(revision, "scene_revision")?, caret)
                    .lua()
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Module table
// ---------------------------------------------------------------------------

fn constants(lua: &Lua, entries: &[(&str, u64)]) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    for (name, value) in entries {
        table.set(*name, *value)?;
    }
    Ok(table)
}

pub fn module(lua: &Lua) -> LuaResult<LuaTable> {
    use vivid_sdk::overlay::{buttons, keys, modifiers};
    let module = lua.create_table()?;
    module.set("connect", lua.create_function(connect)?)?;
    module.set("from_session", lua.create_function(from_session)?)?;

    let path = lua.create_table()?;
    path.set("new", lua.create_function(path_new)?)?;
    path.set(
        "rectangle",
        lua.create_function(|_, bounds: LuaValue| shape("rectangle", bounds, LuaValue::Nil))?,
    )?;
    path.set(
        "rounded_rectangle",
        lua.create_function(|_, (bounds, radius): (LuaValue, LuaValue)| {
            shape("rounded", bounds, radius)
        })?,
    )?;
    path.set(
        "rounded_rectangle_corners",
        lua.create_function(|_, (bounds, radii): (LuaValue, LuaValue)| {
            shape("rounded-corners", bounds, radii)
        })?,
    )?;
    path.set(
        "ellipse",
        lua.create_function(|_, bounds: LuaValue| shape("ellipse", bounds, LuaValue::Nil))?,
    )?;
    module.set("Path", path)?;

    let brush = lua.create_table()?;
    brush.set(
        "solid",
        lua.create_function(|_, value: LuaValue| {
            Ok(LuaBrush {
                inner: model::brush("solid", &[], &[color(value, "color")?], &[])
                    .map_err(model_error)?,
            })
        })?,
    )?;
    brush.set(
        "linear",
        lua.create_function(
            |_, (start, end, stops, space): (LuaValue, LuaValue, LuaValue, LuaValue)| {
                let (x, y) = point_values(start, "start")?;
                let (a, b) = point_values(end, "end")?;
                gradient("linear", &[x, y, a, b], stops, space)
            },
        )?,
    )?;
    brush.set(
        "radial",
        lua.create_function(
            |_, (center, radius, stops, space): (LuaValue, LuaValue, LuaValue, LuaValue)| {
                let (x, y) = point_values(center, "center")?;
                gradient(
                    "radial",
                    &[x, y, f64::from_value(radius, "radius")?],
                    stops,
                    space,
                )
            },
        )?,
    )?;
    // Fill with an uploaded image. `transform` is six affine terms; `extend` is pad, repeat, or
    // reflect.
    brush.set(
        "image",
        lua.create_function(
            |_, (image, transform, extend): (LuaValue, LuaValue, LuaValue)| {
                let image = image_ref(image, "image")?;
                Ok(LuaBrush {
                    inner: model::brush_image(
                        image.id(),
                        opt::<Vec<f64>>(transform, "transform")?,
                        &opt::<String>(extend, "extend")?.unwrap_or_else(|| "pad".into()),
                    )
                    .map_err(model_error)?,
                })
            },
        )?,
    )?;
    module.set("Brush", brush)?;

    let canvas = lua.create_table()?;
    canvas.set(
        "new",
        lua.create_function(|_, ()| {
            Ok(LuaCanvas {
                inner: Canvas::new(),
            })
        })?,
    )?;
    module.set("Canvas", canvas)?;

    // Plain-table constructors, for callers who prefer `rect(0, 0, 10, 10)` to a literal.
    module.set(
        "rect",
        lua.create_function(|lua, (x, y, w, h): (f64, f64, f64, f64)| {
            let table = lua.create_table()?;
            table.set("x", x)?;
            table.set("y", y)?;
            table.set("width", w)?;
            table.set("height", h)?;
            Ok(table)
        })?,
    )?;
    module.set(
        "point",
        lua.create_function(|lua, (x, y): (f64, f64)| point_table(lua, x, y))?,
    )?;

    // Normative overlay values, read from the protocol crate rather than copied.
    module.set(
        "Modifiers",
        constants(
            lua,
            &[
                ("SHIFT", modifiers::SHIFT.into()),
                ("CONTROL", modifiers::CONTROL.into()),
                ("ALT", modifiers::ALT.into()),
                ("SUPER", modifiers::SUPER.into()),
                ("CAPS_LOCK", modifiers::CAPS_LOCK.into()),
                ("NUM_LOCK", modifiers::NUM_LOCK.into()),
                ("KNOWN_MASK", modifiers::KNOWN_MASK.into()),
            ],
        )?,
    )?;
    module.set(
        "MouseButton",
        constants(
            lua,
            &[
                ("PRIMARY", buttons::PRIMARY.into()),
                ("AUXILIARY", buttons::AUXILIARY.into()),
                ("SECONDARY", buttons::SECONDARY.into()),
                ("BACK", buttons::BACK.into()),
                ("FORWARD", buttons::FORWARD.into()),
                ("MAXIMUM", buttons::MAXIMUM.into()),
            ],
        )?,
    )?;
    module.set(
        "Key",
        constants(
            lua,
            &[
                ("UNMAPPED", keys::UNMAPPED.into()),
                ("FIRST_USAGE", keys::FIRST_USAGE.into()),
                ("LAST_USAGE", keys::LAST_USAGE.into()),
            ],
        )?,
    )?;
    Ok(module)
}
