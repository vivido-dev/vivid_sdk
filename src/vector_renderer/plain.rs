//! Native text with fallback mark placement and original UTF-8 geometry.

// Rust guideline compliant 2026-10-07
use super::{TextShaper, marks, text::layout_geometry, wire};
use icu_properties::props::{DefaultIgnorableCodePoint, ExtendedPictographic, GeneralCategory};
use icu_properties::{CodePointMapData, CodePointSetData};
use parley::Layout;
use skrifa::{FontRef, MetadataProvider};
use std::{collections::BTreeMap, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
use vello::{Scene, kurbo::Affine, peniko::Color};
use vivid_protocol::overlay::wire::text::TextMeasurement;

pub(super) struct Plain {
    pub layout: Layout<()>,
    plan: MarkPlan,
}

#[derive(Default)]
pub(super) struct MarkPlan {
    pub adjusted: String,
    // Shortened boundary and cumulative bytes omitted at that boundary.
    offsets: Vec<(usize, usize)>,
    marks: BTreeMap<usize, Arc<Repair>>,
}

impl MarkPlan {
    pub fn detect<B: parley::Brush, K: Clone + Ord>(
        source: &str,
        layout: &Layout<B>,
        style_at: impl Fn(usize, usize, &B) -> Option<K>,
        mut resolve: impl FnMut(&K, &parley::FontData, &str) -> Option<Repair>,
    ) -> Self {
        let mut result = Self::default();
        if source.is_ascii() {
            return result;
        }
        let candidates: BTreeMap<_, _> = source
            .grapheme_indices(true)
            .filter(|(_, value)| {
                let mut chars = value.chars();
                chars
                    .next()
                    .is_some_and(|ch| CodePointSetData::new::<ExtendedPictographic>().contains(ch))
                    && chars.any(|ch| is_mark(ch) && !is_selector(ch))
                    && value.chars().skip(1).all(is_mark)
            })
            .collect();
        if candidates.is_empty() {
            return result;
        }
        // Cache both resolved glyphs and failed lookups for repeated styled clusters.
        let mut resolved = BTreeMap::new();
        let mut fixes = BTreeMap::new();
        for line in layout.lines() {
            for run in line.runs() {
                for cluster in run.visual_clusters() {
                    let range = cluster.text_range();
                    let Some(&value) = candidates.get(&range.start) else {
                        continue;
                    };
                    if range.len() == value.len() && cluster.glyphs().all(|g| g.id != 0) {
                        continue;
                    }
                    let face = run.font();
                    let Some(brush) = style_at(range.start, value.len(), &cluster.style().brush)
                    else {
                        continue;
                    };
                    let key = (face.data.id(), face.index, brush.clone(), value);
                    if let Some(fix) = resolved
                        .entry(key)
                        .or_insert_with(|| resolve(&brush, face, value).map(Arc::new))
                    {
                        fixes.insert(range.start, (value, Arc::clone(fix)));
                    }
                }
            }
        }
        if fixes.is_empty() {
            return result;
        }
        let mut cursor = 0;
        for (start, (value, fix)) in fixes {
            result.adjusted.push_str(&source[cursor..start]);
            result.marks.insert(result.adjusted.len(), Arc::clone(&fix));
            let mut offset = 0;
            for range in &fix.removed {
                result.adjusted.push_str(&value[offset..range.start]);
                offset = range.end;
                let end = result.adjusted.len();
                result.offsets.push((end, start + offset - end));
            }
            result.adjusted.push_str(&value[offset..]);
            cursor = start + value.len();
        }
        result.adjusted.push_str(&source[cursor..]);
        result
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    pub fn original_offset(&self, offset: usize) -> usize {
        let i = self.offsets.partition_point(|(end, _)| *end <= offset);
        offset + i.checked_sub(1).map_or(0, |i| self.offsets[i].1)
    }

    pub fn shortened_boundary(&self, original: usize) -> usize {
        let i = self
            .offsets
            .partition_point(|(end, removed)| end + removed <= original);
        original - i.checked_sub(1).map_or(0, |i| self.offsets[i].1)
    }

    pub fn paint_cluster(
        &self,
        scene: &mut Scene,
        transform: Affine,
        offset: usize,
        center: f32,
        baseline: f32,
        color: Color,
    ) {
        if let Some(repair) = self.marks.get(&offset) {
            for (group, brush) in &repair.groups {
                group.paint(scene, transform, center, baseline, brush.unwrap_or(color));
            }
        }
    }
}

impl Plain {
    pub fn shape(system: &mut impl TextShaper, text: &wire::Text) -> Self {
        let mut layout = system.shape_overlay(text);
        let plan = MarkPlan::detect(
            &text.text,
            &layout,
            |_, _, _| Some(()),
            |_, face, value| {
                let shape = |anchored: String, family: &str| {
                    let request = wire::Text {
                        text: anchored,
                        family: family.into(),
                        max_width: None,
                        size: text.size,
                        origin: text.origin,
                        weight: text.weight,
                        italic: text.italic,
                        color: text.color,
                    };
                    system.shape_overlay(&request)
                };
                resolve(face, value, &text.family, shape)
            },
        );
        if !plan.is_empty() {
            let repaired = wire::Text {
                text: plan.adjusted.clone(),
                origin: text.origin,
                size: text.size,
                family: text.family.clone(),
                weight: text.weight,
                italic: text.italic,
                color: text.color,
                max_width: text.max_width,
            };
            layout = system.shape_overlay(&repaired);
        }
        Self { layout, plan }
    }

    pub fn measure(&self, original: &str) -> Result<TextMeasurement, wire::InvalidScene> {
        let source = if self.plan.is_empty() {
            original
        } else {
            &self.plan.adjusted
        };
        let mut m = layout_geometry(&self.layout, source, usize::MAX)?;
        for entry in m.lines.iter_mut().chain(&mut m.clusters) {
            entry.start = self.plan.original_offset(entry.start as usize) as u32;
            entry.end = self.plan.original_offset(entry.end as usize) as u32;
        }
        m.width = wire::Scalar::new(if original.is_empty() {
            0.
        } else {
            self.layout.full_width().into()
        })?;
        m.height = wire::Scalar::new(self.layout.height().into())?;
        m.validate_text(original)
            .map_err(|_| wire::InvalidScene::new("invalid original text geometry"))?;
        Ok(m)
    }

    pub fn paint_marks(&self, scene: &mut Scene, transform: Affine, color: Color) {
        if self.plan.is_empty() {
            return;
        }
        for line in self.layout.lines() {
            let mut x = line.metrics().offset;
            for run in line.runs() {
                for cluster in run.visual_clusters() {
                    self.plan.paint_cluster(
                        scene,
                        transform,
                        cluster.text_range().start,
                        x + cluster.advance() / 2.,
                        line.metrics().baseline,
                        color,
                    );
                    x += cluster.advance();
                }
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Repair {
    removed: Vec<std::ops::Range<usize>>,
    groups: Vec<(marks::MarkLayout, Option<Color>)>,
}

impl Repair {
    pub fn append(&mut self, part: Self, offset: usize, color: Color) {
        self.removed.extend(
            part.removed
                .into_iter()
                .map(|r| r.start + offset..r.end + offset),
        );
        self.groups.extend(
            part.groups
                .into_iter()
                .map(|(group, _)| (group, Some(color))),
        );
    }

    pub fn has_marks(&self) -> bool {
        !self.groups.is_empty()
    }
}

pub(super) fn resolve<B: parley::Brush>(
    base: &parley::FontData,
    value: &str,
    family: &str,
    mut shape: impl FnMut(String, &str) -> Layout<B>,
) -> Option<Repair> {
    let font = FontRef::from_index(base.data.as_ref(), base.index).ok()?;
    let cmap = font.charmap();
    let removed: Vec<_> = value
        .char_indices()
        .skip(1)
        .filter(|(_, ch)| !is_selector(*ch) && cmap.map(*ch).is_none_or(|id| id.to_u32() == 0))
        .map(|(offset, ch)| offset..offset + ch.len_utf8())
        .collect();
    if removed.is_empty() {
        return None;
    }
    // A proportional face can cover an accent but lack its positioning anchor.
    let groups = marks::fallback(value, base, removed.len(), |s| shape(s, family))
        .or_else(|| marks::fallback(value, base, removed.len(), |s| shape(s, "monospace")))?;
    Some(Repair {
        removed,
        groups: groups.into_iter().map(|g| (g, None)).collect(),
    })
}

fn is_mark(ch: char) -> bool {
    matches!(
        CodePointMapData::<GeneralCategory>::new().get(ch),
        GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
    )
}

fn is_selector(ch: char) -> bool {
    CodePointSetData::new::<DefaultIgnorableCodePoint>().contains(ch) && is_mark(ch)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::vector_renderer::{compile, text::FontSystem};
    use wire::{Canvas, Command, Point, Scalar, Text};

    #[test]
    fn plain_vector_marks_preserve_source_geometry_and_next_glyph() {
        let mut fonts = FontSystem::new("").unwrap();
        let mut request = Text {
            text: "🦇X".into(),
            origin: Point::new(0., 0.).unwrap(),
            size: Scalar::new(16.).unwrap(),
            family: String::new(),
            weight: 400,
            italic: false,
            color: wire::Color(0xffffffff),
            max_width: None,
        };
        let bare = fonts.measure_plain(&request).unwrap();
        for value in ["🦇́X", "🦇़X", "🦇़́X"] {
            request.text = value.into();
            let m = fonts.measure_plain(&request).unwrap();
            assert_eq!(m.width, bare.width, "{value}");
            assert_eq!(m.clusters.len(), 2);
            assert_eq!(m.clusters[0].end as usize, value.len() - 1);
            assert_eq!(m.clusters[1].x, bare.clusters[1].x);
            let mut canvas = Canvas::new();
            canvas.push(Command::Text(request.clone())).unwrap();
            let scene = compile(&canvas, &mut fonts, &BTreeMap::new()).unwrap();
            let glyphs = &scene.scene.encoding().resources.glyphs;
            assert!(glyphs.iter().all(|g| g.id != 0));
            assert!(glyphs.iter().any(|g| g.x == m.clusters[1].x.get() as f32));
            assert!(
                glyphs
                    .iter()
                    .filter(|g| g.x > 0. && g.x < m.clusters[1].x.get() as f32)
                    .count()
                    >= value.chars().count() - 2
            );
        }
        // Repair byte ranges independently at each occurrence across wrapped
        // lines, variation selectors, and a right-to-left paragraph.
        request.max_width = Some(Scalar::new(48.).unwrap());
        for (base, marked) in [
            ("🦇X 🦇X 🦇X", "🦇́X 🦇़X 🦇़́X"),
            ("🦇️X 🦇️X", "🦇️́X 🦇️़X"),
            ("אב 🦇X אב 🦇X", "אב 🦇́X אב 🦇़X"),
        ] {
            request.text = base.into();
            let reference = fonts.measure_plain(&request).unwrap();
            request.text = marked.into();
            let actual = fonts.measure_plain(&request).unwrap();
            assert_eq!(actual.width, reference.width);
            assert_eq!(actual.height, reference.height);
            assert_eq!(actual.lines.len(), reference.lines.len());
            assert_eq!(actual.clusters.len(), reference.clusters.len());
            for (cluster, expected) in actual.clusters.iter().zip(&reference.clusters) {
                assert_eq!(
                    (cluster.x, cluster.y, cluster.width, cluster.rtl),
                    (expected.x, expected.y, expected.width, expected.rtl)
                );
                let source = &marked[cluster.start as usize..cluster.end as usize];
                let clean: String = source.chars().filter(|ch| !matches!(ch, '́' | '़')).collect();
                assert_eq!(clean, base[expected.start as usize..expected.end as usize]);
            }
        }
    }
}
