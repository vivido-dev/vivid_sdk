//! Immutable, host-shaped glyph scenes. Painting these never invokes the shaper again.

// Rust guideline compliant 2026-10-07
use super::*;
use vivid_protocol::overlay::wire::text::styled::StyledText;

/// Text geometry with a token that keeps its measurement resources alive.
pub struct MeasuredLayout {
    /// Compiled vector drawing commands.
    pub scene: Scene,
    /// Width in this value's coordinate system.
    pub width: f64,
    /// Height in this value's coordinate system.
    pub height: f64,
    /// Shared lifetime token for measurement resources.
    pub token: Arc<()>,
}

/// Append shaped text to the supplied vector scene.
///
/// # Errors
///
/// Returns a vector error when text cannot be laid out or painted within the command contract.
pub fn paint(
    layout: &parley::Layout<usize>,
    text: &StyledText,
    width: f64,
    height: f64,
) -> Result<MeasuredLayout, InvalidScene> {
    text.validate()
        .map_err(|_| InvalidScene::new("invalid styled text"))?;
    paint_prepared(layout, text, width, height)
}

// Only shared preparation may exceed the wire source limit, by its bounded
// direction mark and ellipsis. Original producer paragraphs are validated first.
pub(super) fn paint_prepared(
    layout: &parley::Layout<usize>,
    text: &StyledText,
    width: f64,
    height: f64,
) -> Result<MeasuredLayout, InvalidScene> {
    if width < 0.0 || height < 0.0 {
        return Err(InvalidScene::new("negative shaped layout extent"));
    }
    check_bounds(Rect::new(0., 0., width, height), Affine::IDENTITY)?;
    let mut scene = Scene::new();
    // Clip truncation and non-wrapping overflow to the measured box.
    let clip_width = text.max_width.map_or(width.max(1.), |w| w.get());
    scene.push_layer(
        Fill::NonZero,
        Mix::Normal,
        1.,
        Affine::IDENTITY,
        &Rect::new(0., 0., clip_width, height),
    );
    for line in layout
        .lines()
        .take(text.max_lines.map_or(usize::MAX, usize::from))
    {
        for item in line.items() {
            let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let style = &text
                .runs
                .get(glyph_run.style().brush)
                .ok_or(InvalidScene::new(
                    "shaped layout references an absent text style",
                ))?
                .style;
            let start = glyph_run.offset();
            let baseline = glyph_run.baseline();
            let mut x = start;
            scene
                .draw_glyphs(run.font())
                .font_size(run.font_size())
                .normalized_coords(bytemuck::cast_slice(run.normalized_coords()))
                .brush(color(style.color))
                .hint(false)
                .draw(
                    Fill::NonZero,
                    glyph_run.glyphs().map(|g| {
                        let glyph = Glyph {
                            id: g.id,
                            x: x + g.x,
                            y: baseline - g.y,
                        };
                        x += g.advance;
                        glyph
                    }),
                );
            let metrics = run.font_metrics();
            for (enabled, offset, thickness) in [
                (
                    style.underline,
                    metrics.underline_offset,
                    metrics.underline_size,
                ),
                (
                    style.strikethrough,
                    metrics.strikethrough_offset,
                    metrics.strikethrough_size,
                ),
            ] {
                if enabled {
                    let y = f64::from(baseline - offset);
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        color(style.color),
                        None,
                        &Rect::new(
                            f64::from(start),
                            y,
                            f64::from(x),
                            y + f64::from(thickness.max(0.5)),
                        ),
                    );
                }
            }
        }
    }
    scene.pop_layer();
    Ok(MeasuredLayout {
        scene,
        width: clip_width,
        height,
        token: Arc::new(()),
    })
}

// Debug omits user content and native resources, and never acquires application locks.
impl std::fmt::Debug for MeasuredLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeasuredLayout").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::overlay::wire::text::styled::TextStyle;

    #[test]
    fn invalid_public_paint_inputs_fail_before_accessing_shaped_runs() {
        let text = StyledText::new("private source text", TextStyle::default());
        let layout = parley::Layout::<usize>::default();
        for (width, height) in [
            (-1., 10.),
            (10., -1.),
            (f64::NAN, 10.),
            (10., f64::INFINITY),
        ] {
            assert!(paint(&layout, &text, width, height).is_err());
        }
        let mut invalid = text;
        invalid.runs.clear();
        assert!(paint(&layout, &invalid, 10., 10.).is_err());
    }
}
