//! Paint missing combining marks from fallback fonts around a shaped base glyph.
// Rust guideline compliant 2026-10-07

use icu_properties::props::{DefaultIgnorableCodePoint, GeneralCategory};
use icu_properties::{CodePointMapData, CodePointSetData};
use parley::{FontData, Layout, PositionedLayoutItem};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use std::borrow::Borrow;
use vello::kurbo::Affine;
use vello::peniko::{Color, Fill};
use vello::{Glyph, Scene};

/// Positioned marks from one font, relative to the center of their base.
#[derive(Debug)]
pub struct MarkLayout {
    font: FontData,
    size: f32,
    coords: Vec<i16>,
    glyphs: Vec<Glyph>,
}

/// Resolves all missing marks in a cluster using the caller's font policy.
pub fn fallback<B: parley::Brush, L: Borrow<Layout<B>>>(
    source: &str,
    original: &FontData,
    missing: usize,
    mut shape: impl FnMut(String) -> L,
) -> Option<Vec<MarkLayout>> {
    let font = FontRef::from_index(original.data.as_ref(), original.index).ok()?;
    let cmap = font.charmap();
    let mut chars = source.chars();
    cmap.map(chars.next()?).filter(|g| g.to_u32() != 0)?;
    let categories = CodePointMapData::<GeneralCategory>::new();
    let ignored = CodePointSetData::new::<DefaultIgnorableCodePoint>();
    let mut marks = String::new();
    let mut count = 0;
    for ch in chars {
        // Selectors and joiners remain in the already-shaped base sequence.
        if ignored.contains(ch) || cmap.map(ch).is_some_and(|g| g.to_u32() != 0) {
            continue;
        }
        if !matches!(
            categories.get(ch),
            GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
        ) {
            return None;
        }
        marks.push(ch);
        count += 1;
    }
    // Normalization and GSUB may have substituted a glyph. Only suppress
    // .notdef when each missing mark accounts for one observed missing glyph.
    if count != missing {
        return None;
    }

    if let Some(layout) = shape_marks(&marks, &mut shape) {
        return Some(vec![layout]);
    }

    if count == 1 {
        return None;
    }

    // Keep adjacent marks together when one face can shape them, so its
    // mark-to-mark positioning still applies within a mixed-font cell.
    let mut groups = Vec::new();
    let mut start = 0;
    let mut face = None;
    for (offset, mark) in marks.char_indices() {
        let end = offset + mark.len_utf8();
        let single = shape_marks(&marks[offset..end], &mut shape)?;
        if face.as_ref().is_some_and(|prev| prev != &single.font) {
            groups.push(shape_marks(&marks[start..offset], &mut shape)?);
            start = offset;
        }
        face = Some(single.font);
    }
    if start == 0 {
        return None;
    }
    groups.push(shape_marks(&marks[start..], &mut shape)?);
    Some(groups)
}

fn shape_marks<B: parley::Brush, L: Borrow<Layout<B>>>(
    marks: &str,
    shape: &mut impl FnMut(String) -> L,
) -> Option<MarkLayout> {
    let mut anchored = String::from("◌");
    anchored.push_str(marks);
    let layout = shape(anchored);
    let line = layout.borrow().lines().next()?;
    let mut items = line.items();
    let PositionedLayoutItem::GlyphRun(run) = items.next()? else {
        return None;
    };
    if items.next().is_some() {
        return None;
    }
    let face = run.run().font();
    let font = FontRef::from_index(face.data.as_ref(), face.index).ok()?;
    let anchor_id = font.charmap().map('◌')?.to_u32();
    let mut glyphs = run.glyphs();
    let anchor = glyphs.next()?;
    if anchor.id != anchor_id || anchor_id == 0 {
        return None;
    }
    // The fallback face positions the marks on its dotted circle. Omit the
    // anchor and express the remaining positions relative to its center.
    let mut origin = anchor.advance / 2.0;
    let mut glyphs: Vec<_> = glyphs
        .map(|g| {
            let glyph = Glyph {
                id: g.id,
                x: origin + g.x,
                y: -g.y,
            };
            origin += g.advance;
            glyph
        })
        .collect();
    if glyphs.is_empty() || glyphs.iter().any(|g| g.id == 0) {
        return None;
    }
    // Some faces place an unattached accent beyond the dotted circle. Move
    // the group together to retain its internal mark-to-mark positioning.
    let metrics = font.glyph_metrics(
        Size::new(run.run().font_size()),
        LocationRef::new(bytemuck::cast_slice(run.run().normalized_coords())),
    );
    let ink = glyphs.iter().try_fold(
        (f32::INFINITY, f32::NEG_INFINITY),
        |(left, right), glyph| {
            let bounds = metrics.bounds(GlyphId::new(glyph.id))?;
            Some((
                left.min(glyph.x + bounds.x_min),
                right.max(glyph.x + bounds.x_max),
            ))
        },
    );
    if let Some((left, right)) = ink {
        let center = (left + right) / 2.0;
        if center.abs() > anchor.advance / 2.0 {
            for glyph in &mut glyphs {
                glyph.x -= center;
            }
        }
    }
    Some(MarkLayout {
        font: face.clone(),
        size: run.run().font_size(),
        coords: bytemuck::cast_slice(run.run().normalized_coords()).to_vec(),
        glyphs,
    })
}

impl MarkLayout {
    /// Draws the marks at the supplied base center and baseline.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        center: f32,
        baseline: f32,
        color: Color,
    ) {
        scene
            .draw_glyphs(&self.font)
            .transform(transform)
            .font_size(self.size)
            .normalized_coords(&self.coords)
            .brush(color)
            .hint(false)
            .draw(
                Fill::NonZero,
                self.glyphs.iter().map(|g| Glyph {
                    x: g.x + center,
                    y: g.y + baseline,
                    ..*g
                }),
            );
    }
}
