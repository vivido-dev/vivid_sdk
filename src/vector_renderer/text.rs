//! Bounded, grapheme-safe end truncation. The measured and painted layout is the same object.
// Rust guideline compliant 2026-10-07
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

/// Styled source and the exact layout used for measurement and painting.
pub struct PreparedText {
    /// Final visible text, including any inserted direction mark and ellipsis.
    pub text: StyledText,
    /// The corresponding native shaped layout.
    pub layout: Layout<usize>,
    /// Original UTF-8 truncation boundary, if the paragraph was shortened.
    pub truncated_at: Option<usize>,
    /// Bytes of the host-inserted direction mark, excluded from public source offsets.
    pub prefix_bytes: usize,
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
    if text.typography.overflow != TextOverflow::Ellipsis || text.text().is_empty() {
        return Ok(PreparedText {
            text: text.clone(),
            layout,
            truncated_at: None,
            prefix_bytes: 0,
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
        });
    }
    let mut low = 0;
    let mut high = boundaries.len();
    // At most ceil(log2(4097)) probes. Keep only candidates whose complete layout fits.
    while low + 1 < high {
        let mid = low + (high - low) / 2;
        let candidate = prefix(text, boundaries[mid], direction);
        let shaped = system.shape_styled(&candidate)?;
        if fits(&shaped) {
            low = mid;
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
    let source = prepared.text.text();
    let mut measured = layout_geometry(
        &prepared.layout,
        &source,
        text.max_lines.map_or(usize::MAX, usize::from),
    )?;
    if let Some(cut) = prepared.truncated_at {
        measured
            .clusters
            .retain(|geometry| geometry.end as usize > prepared.prefix_bytes);
        for geometry in measured.lines.iter_mut().chain(&mut measured.clusters) {
            geometry.start = (geometry.start as usize)
                .saturating_sub(prepared.prefix_bytes)
                .min(cut) as u32;
            geometry.end = (geometry.end as usize)
                .saturating_sub(prepared.prefix_bytes)
                .min(cut) as u32;
        }
        measured.truncated_at = Some(cut as u32);
        measured
            .validate_text(&text.text())
            .map_err(|_| InvalidScene::new("invalid ellipsis source mapping"))?;
    }
    if let Some(width) = text.max_width {
        measured.width = width;
    }
    let painted = if retain {
        Some(super::text_layout::paint_prepared(
            &prepared.layout,
            &prepared.text,
            measured.width.get(),
            measured.height.get(),
        )?)
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

    #[test]
    fn maximum_source_can_paint_a_bounded_host_inserted_ellipsis() {
        let mut fonts = FontSystem::new("").unwrap();
        // Fewer than 1,024 grapheme clusters can still fill the 4,096-byte
        // source budget. The short ASCII suffix leaves room for the marker.
        let source = format!("{}WWWW", "🦀\u{301}".repeat(682));
        assert_eq!(source.len(), MAX_MEASURE_TEXT_BYTES);
        let mut text = StyledText::new(source, TextStyle::default());
        let original = fonts.shape_styled(&text).unwrap().full_width();
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
