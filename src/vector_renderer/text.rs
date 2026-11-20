//! Bounded, grapheme-safe end truncation. The measured and painted layout is the same object.
// Rust guideline compliant 2026-10-07
use super::plain::{MarkPlan, Repair, resolve};
use super::{TextShaper, text_layout::MeasuredLayout};
use parley::Layout;
use vivid_protocol::overlay::wire::text::{
    MAX_MEASURE_TEXT_BYTES, MAX_TEXT_GEOMETRY, TextGeometry, TextMeasurement,
};
use vivid_protocol::vector::{InvalidScene, Scalar, Text};
mod font;
#[doc(inline)]
pub use font::FontSystem;
use unicode_segmentation::UnicodeSegmentation;
use vivid_protocol::overlay::wire::text::styled::{StyledText, TextOverflow};

/// Measures plain text using the same fallback and wrapping as vector compilation.
///
/// # Errors
/// Rejects invalid text, oversized source and geometry outside the protocol bounds.
pub fn measure_plain(
    system: &mut impl TextShaper,
    text: &Text,
) -> Result<TextMeasurement, InvalidScene> {
    if text.text.len() > MAX_MEASURE_TEXT_BYTES {
        return Err(InvalidScene::new("text measurement source exceeds limit"));
    }
    let mut canvas = vivid_protocol::vector::Canvas::new();
    canvas.push(vivid_protocol::vector::Command::Text(text.clone()))?;
    super::plain::Plain::shape(system, text).measure(&text.text)
}

/// Styled source and the exact layout used for measurement and painting.
pub struct PreparedText {
    /// Final visible text, including any inserted direction mark and ellipsis.
    pub text: StyledText,
    /// Native layout with repaired advances. Its byte ranges index `layout_source`.
    pub layout: Layout<usize>,
    /// Original UTF-8 truncation boundary, if the paragraph was shortened.
    pub truncated_at: Option<usize>,
    /// Bytes of the host-inserted direction mark, excluded from public source offsets.
    pub prefix_bytes: usize,
    pub(super) marks: MarkPlan,
    adjusted_visible: Option<String>,
}

impl PreparedText {
    /// Returns the visible UTF-8 string indexed by native layout clusters.
    pub fn layout_source(&self) -> String {
        if self.truncated_at.is_none() && !self.marks.is_empty() {
            self.marks.adjusted.clone()
        } else {
            self.adjusted_visible
                .clone()
                .unwrap_or_else(|| self.text.text())
        }
    }

    /// Paints the fitted paragraph with its resolved marks and original styles.
    ///
    /// # Errors
    /// Rejects invalid drawing extents or references to absent styles.
    pub fn paint(&self, width: f64, height: f64) -> Result<MeasuredLayout, InvalidScene> {
        super::text_layout::paint_prepared(&self.layout, &self.text, width, height, Some(self))
    }
}

/// Supplies real host shaping for complete styled paragraphs.
pub trait StyledTextShaper {
    /// Shapes validated styled text with consistent paragraph and font policy.
    ///
    /// # Errors
    /// Returns an error for invalid text or unavailable host font resources.
    fn shape_styled(&mut self, text: &StyledText) -> Result<Layout<usize>, InvalidScene>;
}

/// Fits styled text using grapheme-safe ellipsis and the host's real shaping.
///
/// # Errors
/// Rejects invalid paragraphs or host shaping failures.
pub fn prepare_styled(
    system: &mut impl StyledTextShaper,
    text: &StyledText,
) -> Result<PreparedText, InvalidScene> {
    text.validate()
        .map_err(|_| InvalidScene::new("invalid styled text"))?;
    let layout = system.shape_styled(text)?;
    let source = text.text();
    if source.is_ascii() {
        return fit_styled(system, text, layout, MarkPlan::default());
    }
    let mut end = 0;
    let ends: Vec<_> = text
        .runs
        .iter()
        .map(|r| {
            end += r.text.len();
            end
        })
        .collect();
    let mut failure = None;
    let plan = MarkPlan::detect(
        &source,
        &layout,
        |start, len, _| {
            let index = ends.partition_point(|end| *end <= start);
            let mut spans = Vec::new();
            for (index, &end) in ends.iter().enumerate().skip(index) {
                let relative = end.min(start + len) - start;
                if spans.last().is_none_or(|&(prev, _)| prev != relative) {
                    spans.push((relative, index));
                }
                if end >= start + len {
                    break;
                }
            }
            Some(spans)
        },
        |spans, face, value| {
            let base_end = value.chars().next()?.len_utf8();
            let mut result = Repair::default();
            let mut start = base_end;
            for &(end, index) in spans {
                if end <= start {
                    continue;
                }
                let style = &text.runs[index].style;
                // Each style shapes its missing marks against the same anchor.
                let fragment = format!("{}{}", &value[..base_end], &value[start..end]);
                let part = resolve(face, &fragment, &style.family, |source, family| {
                    let mut probe = StyledText::new(source, style.clone());
                    probe.runs[0].style.family = family.into();
                    match system.shape_styled(&probe) {
                        Ok(layout) => layout,
                        Err(error) => {
                            failure = Some(error);
                            Layout::default()
                        }
                    }
                });
                if let Some(part) = part {
                    result.append(part, start - base_end, super::color(style.color));
                }
                start = end;
            }
            result.has_marks().then_some(result)
        },
    );
    if let Some(error) = failure {
        return Err(error);
    }
    let layout = if plan.is_empty() {
        layout
    } else {
        system.shape_styled(&adjust_for_layout(text, &plan, source.len(), 0))?
    };
    fit_styled(system, text, layout, plan)
}

// Replace only producer bytes. Host-inserted direction and ellipsis bytes
// retain their styles even when their run consists solely of a removed mark.
fn adjust_for_layout(text: &StyledText, plan: &MarkPlan, cut: usize, prefix: usize) -> StyledText {
    let mut adjusted = text.clone();
    let mut start = 0;
    for (index, run) in adjusted.runs.iter_mut().enumerate() {
        let leading = if index == 0 { prefix } else { 0 };
        let count = (run.text.len() - leading).min(cut - start);
        let end = start + count;
        run.text.replace_range(
            leading..leading + count,
            &plan.adjusted[plan.shortened_boundary(start)..plan.shortened_boundary(end)],
        );
        start = end;
    }
    adjusted
}

fn fit_styled(
    system: &mut impl StyledTextShaper,
    text: &StyledText,
    layout: Layout<usize>,
    plan: MarkPlan,
) -> Result<PreparedText, InvalidScene> {
    if text.typography.overflow != TextOverflow::Ellipsis || text.text().is_empty() {
        return Ok(PreparedText {
            text: text.clone(),
            layout,
            truncated_at: None,
            prefix_bytes: 0,
            adjusted_visible: None,
            marks: plan,
        });
    }
    let width = text.max_width.map_or(f64::MAX, |w| w.get());
    let lines = usize::from(text.max_lines.unwrap_or(1));
    let fits = |layout: &Layout<usize>| {
        layout.len() <= lines
            && layout
                .lines()
                .all(|line| f64::from(line.metrics().advance) <= width + 0.001)
    };
    if fits(&layout) {
        return Ok(PreparedText {
            text: text.clone(),
            layout,
            truncated_at: None,
            prefix_bytes: 0,
            adjusted_visible: None,
            marks: plan,
        });
    }
    let source = text.text();
    let boundaries: Vec<_> = source
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .chain(std::iter::once(source.len()))
        .collect();
    let direction = if layout.is_rtl() {
        '\u{200f}'
    } else {
        '\u{200e}'
    };
    let mut best_text = prefix(text, 0, direction);
    let mut best_layout = system.shape_styled(&best_text)?;
    if !fits(&best_layout) {
        // Never draw a clipped fragment of the marker itself.
        best_text.runs.truncate(1);
        best_text.runs[0].text.clear();
        best_layout = system.shape_styled(&best_text)?;
        return Ok(PreparedText {
            text: best_text,
            layout: best_layout,
            truncated_at: Some(0),
            prefix_bytes: 0,
            adjusted_visible: None,
            marks: plan,
        });
    }
    let mut adjusted_visible = None;
    let mut low = 0;
    let mut high = boundaries.len();
    // At most ceil(log2(4097)) probes. Keep only candidates whose complete layout fits.
    while low + 1 < high {
        let mid = low + (high - low) / 2;
        let candidate = prefix(text, boundaries[mid], direction);
        let adjusted = (!plan.is_empty())
            .then(|| adjust_for_layout(&candidate, &plan, boundaries[mid], direction.len_utf8()));
        let shaped = system.shape_styled(adjusted.as_ref().unwrap_or(&candidate))?;
        if fits(&shaped) {
            low = mid;
            adjusted_visible = adjusted.map(|paragraph| paragraph.text());
            best_text = candidate;
            best_layout = shaped;
        } else {
            high = mid;
        }
    }
    Ok(PreparedText {
        text: best_text,
        layout: best_layout,
        truncated_at: Some(boundaries[low]),
        prefix_bytes: direction.len_utf8(),
        adjusted_visible,
        marks: plan,
    })
}

fn prefix(text: &StyledText, cut: usize, direction: char) -> StyledText {
    let mut result = text.clone();
    result.runs.clear();
    let mut offset = 0;
    for run in &text.runs {
        if offset >= cut {
            break;
        }
        let mut kept = run.clone();
        let count = (cut - offset).min(run.text.len());
        kept.text.truncate(count);
        result.runs.push(kept);
        offset += run.text.len();
    }
    if result.runs.is_empty() {
        let mut run = text.runs[0].clone();
        run.text.clear();
        result.runs.push(run);
    }
    result.runs[0].text.insert(0, direction);
    if let Some(last) = result.runs.last_mut() {
        last.text.push('…');
    }
    result
}

/// Converts native line and cluster geometry into bounded protocol measurements.
///
/// # Errors
/// Rejects excessive source/geometry, invalid UTF-8 ranges or unbounded coordinates.
pub fn layout_geometry<B: parley::Brush>(
    layout: &parley::Layout<B>,
    source: &str,
    max_lines: usize,
) -> Result<TextMeasurement, InvalidScene> {
    // Ellipsis may add a three-byte direction mark and three-byte marker to
    // an already bounded source. These bytes never become public source offsets.
    if source.len() > MAX_MEASURE_TEXT_BYTES + 6 {
        return Err(InvalidScene::new("text measurement source exceeds limit"));
    }
    let scalar = |v: f32| {
        Scalar::new(f64::from(v)).map_err(|_| InvalidScene::new("text geometry out of range"))
    };
    let mut result = TextMeasurement {
        truncated_at: None,
        width: Scalar::ZERO,
        height: Scalar::ZERO,
        lines: Vec::new(),
        clusters: Vec::new(),
    };
    for line in layout.lines().take(max_lines) {
        if result.lines.len() >= MAX_TEXT_GEOMETRY {
            return Err(InvalidScene::new("too many text lines"));
        }
        let m = line.metrics();
        let range = line.text_range();
        let geometry = TextGeometry {
            start: range.start as u32,
            end: range.end.min(source.len()) as u32,
            x: scalar(m.offset)?,
            y: scalar(m.block_min_coord)?,
            width: if source.is_empty() {
                Scalar::ZERO
            } else {
                scalar(m.advance)?
            },
            height: scalar(m.block_max_coord - m.block_min_coord)?,
            baseline: scalar(m.baseline)?,
            rtl: false,
        };
        result.lines.push(geometry.clone());
        result.width = result.width.max(if source.is_empty() {
            Scalar::ZERO
        } else {
            scalar(m.offset + m.advance)?
        });
        result.height = scalar(m.block_max_coord)?;
        let mut x = m.offset;
        for run in line.runs() {
            for cluster in run.visual_clusters() {
                if result.clusters.len() >= MAX_TEXT_GEOMETRY {
                    return Err(InvalidScene::new("too many text clusters"));
                }
                let range = cluster.text_range();
                result.clusters.push(TextGeometry {
                    start: range.start as u32,
                    end: range.end as u32,
                    x: scalar(x)?,
                    width: scalar(cluster.advance())?,
                    baseline: Scalar::ZERO,
                    rtl: cluster.is_rtl(),
                    ..geometry.clone()
                });
                x += cluster.advance();
            }
        }
    }
    result
        .validate_text(source)
        .map_err(|_| InvalidScene::new("invalid text ranges"))?;
    Ok(result)
}

/// Shapes, measures and optionally paints one paragraph from the same layout.
///
/// # Errors
/// Rejects invalid text, shaping, geometry, source mapping or painting.
pub fn measure_styled(
    system: &mut impl StyledTextShaper,
    text: &StyledText,
    retain: bool,
) -> Result<(TextMeasurement, Option<MeasuredLayout>), InvalidScene> {
    let prepared = prepare_styled(system, text)?;
    let source = prepared.layout_source();
    let mut measured = layout_geometry(
        &prepared.layout,
        &source,
        text.max_lines.map_or(usize::MAX, usize::from),
    )?;
    if prepared.truncated_at.is_some() || !prepared.marks.is_empty() {
        measured.clusters.retain(|geometry| {
            geometry.end as usize > prepared.prefix_bytes || prepared.prefix_bytes == 0
        });
        let restore = |offset: u32| {
            let short = (offset as usize).saturating_sub(prepared.prefix_bytes);
            prepared
                .marks
                .original_offset(short)
                .min(prepared.truncated_at.unwrap_or(usize::MAX)) as u32
        };
        for geometry in measured.lines.iter_mut().chain(&mut measured.clusters) {
            geometry.start = restore(geometry.start);
            geometry.end = restore(geometry.end);
        }
        measured.truncated_at = prepared.truncated_at.map(|cut| cut as u32);
        measured
            .validate_text(&text.text())
            .map_err(|_| InvalidScene::new("invalid styled source mapping"))?;
    }
    if let Some(width) = text.max_width {
        measured.width = width;
    }
    let painted = if retain {
        Some(prepared.paint(measured.width.get(), measured.height.get())?)
    } else {
        None
    };
    Ok((measured, painted))
}

impl std::fmt::Debug for PreparedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedText").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::overlay::wire::text::styled::TextStyle;

    #[test]
    fn native_fonts_measure_proportional_glyphs_and_preserve_unicode_ranges() {
        let mut fonts = FontSystem::new("").unwrap();
        let narrow = StyledText::new("iiii", TextStyle::default());
        let wide = StyledText::new("WWWW", TextStyle::default());
        let (a, _) = measure_styled(&mut fonts, &narrow, false).unwrap();
        let (b, _) = measure_styled(&mut fonts, &wide, false).unwrap();
        assert!(b.width.get() > a.width.get() * 1.5);
        for source in [
            "e\u{301}e\u{301} long words",
            "👨‍👩‍👧‍👦🇺🇸 many words",
            "אבג דהו זחט יכל מנס",
        ] {
            let mut text = StyledText::new(source, TextStyle::default());
            text.max_width = Some(Scalar::new(45.).unwrap());
            text.max_lines = Some(1);
            text.typography.overflow = TextOverflow::Ellipsis;
            let (metrics, painted) = measure_styled(&mut fonts, &text, true).unwrap();
            let cut = metrics.truncated_at.unwrap() as usize;
            assert!(
                source
                    .grapheme_indices(true)
                    .any(|(offset, _)| offset == cut)
            );
            metrics.validate_text(source).unwrap();
            assert_eq!(painted.unwrap().width, metrics.width.get());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn styled_marks_keep_source_ranges_wrap_and_ellipsis_style() {
        use vivid_protocol::overlay::wire::text::styled::TextRun;
        let mut fonts = FontSystem::new("").unwrap();
        let mut marked = StyledText::new("🦇", TextStyle::default());
        let accent = TextStyle {
            size: Scalar::new(22.).unwrap(),
            ..TextStyle::default()
        };
        marked.runs.push(TextRun {
            text: "़́".into(),
            style: accent.clone(),
        });
        marked.runs.push(TextRun {
            text: "X אב 🦇़́X tail".into(),
            style: TextStyle::default(),
        });
        let mut bare = marked.clone();
        bare.runs[1].text.clear();
        bare.runs[2].text = "X אב 🦇X tail".into();
        for width in [None, Some(Scalar::new(43.).unwrap())] {
            bare.max_width = width;
            marked.max_width = width;
            let (a, _) = measure_styled(&mut fonts, &bare, false).unwrap();
            let (b, retained) = measure_styled(&mut fonts, &marked, true).unwrap();
            assert_eq!(a.width, b.width);
            assert_eq!(a.lines.len(), b.lines.len());
            assert_eq!(a.clusters.len(), b.clusters.len());
            for (left, right) in a.clusters.iter().zip(&b.clusters) {
                assert_eq!(
                    (left.x, left.width, left.rtl),
                    (right.x, right.width, right.rtl)
                );
                let source =
                    marked.text()[right.start as usize..right.end as usize].replace(['́', '़'], "");
                assert_eq!(source, bare.text()[left.start as usize..left.end as usize]);
            }
            assert!(retained.is_some());
        }
        // Choose the exact width of the base and an ellipsis in the accent's
        // larger font. That style survives even when its mark bytes disappear.
        let mut expected = StyledText::new("🦇", TextStyle::default());
        expected.runs.push(TextRun {
            text: "…".into(),
            style: accent,
        });
        let width = measure_styled(&mut fonts, &expected, false)
            .unwrap()
            .0
            .width;
        marked.max_width = Some(width);
        marked.wrap = false;
        marked.typography.overflow = TextOverflow::Ellipsis;
        let p = prepare_styled(&mut fonts, &marked).unwrap();
        assert_eq!(p.truncated_at, Some("🦇़́".len()));
        assert_eq!(p.text.runs.last().unwrap().text, "़́…");
        assert_eq!(p.layout_source(), "\u{200e}🦇…");
        assert_eq!(p.layout.full_width(), width.get() as f32);
        let (measured, _) = measure_styled(&mut fonts, &marked, true).unwrap();
        let last = measured.clusters.last().unwrap();
        assert_eq!(
            (last.start, last.end),
            ("🦇़́".len() as u32, "🦇़́".len() as u32)
        );
        measured.validate_text(&marked.text()).unwrap();
    }

    #[test]
    fn maximum_source_can_paint_a_bounded_host_inserted_ellipsis() {
        let mut fonts = FontSystem::new("").unwrap();
        // Fewer than 1,024 grapheme clusters can still fill the 4,096-byte
        // source budget. The short ASCII suffix leaves room for the marker.
        let source = format!("{}WWWW", "🦀\u{301}".repeat(682));
        assert_eq!(source.len(), MAX_MEASURE_TEXT_BYTES);
        let mut text = StyledText::new(source, TextStyle::default());
        let original = prepare_styled(&mut fonts, &text)
            .unwrap()
            .layout
            .full_width();
        text.max_width = Some(Scalar::new(f64::from(original - 1.)).unwrap());
        text.max_lines = Some(1);
        text.typography.overflow = TextOverflow::Ellipsis;
        text.wrap = false;
        let prepared = prepare_styled(&mut fonts, &text).unwrap();
        assert!(
            prepared.text.text().len() > MAX_MEASURE_TEXT_BYTES,
            "length {}, cut {:?}, original width {}, final width {}",
            prepared.text.text().len(),
            prepared.truncated_at,
            original,
            prepared.layout.full_width()
        );
        assert!(prepared.text.text().len() <= MAX_MEASURE_TEXT_BYTES + 6);
        let (metrics, painted) = measure_styled(&mut fonts, &text, true).unwrap();
        metrics.validate_text(&text.text()).unwrap();
        assert!(painted.is_some());
        text.runs[0].text.push('i');
        assert!(measure_styled(&mut fonts, &text, true).is_err());
    }
}
