//! Native font selection for independent SDK overlay hosts.

// Rust guideline compliant 2026-10-07
use super::*;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle as ParleyFontStyle, FontWeight,
    Layout, LayoutContext, LineHeight, StyleProperty,
};

/// Native Parley font and layout state owned by one host worker.
///
/// Construct this on the worker that performs shaping. No GUI event loop or
/// terminal runtime is created. Call measurement helpers for untrusted input;
/// the shaping traits receive validated or internally derived paragraphs.
pub struct FontSystem {
    font_cx: FontContext,
    layout_cx: LayoutContext<()>,
    family: String,
}
impl FontSystem {
    /// Opens native font discovery with a bounded default family.
    ///
    /// An empty family selects the platform's sans-serif family.
    ///
    /// # Errors
    /// Rejects malformed family names or an empty system font collection.
    pub fn new(family: impl Into<String>) -> Result<Self, InvalidScene> {
        let family = family.into();
        if family.len() > 256 || family.contains('\0') {
            return Err(InvalidScene::new("invalid host font family"));
        }
        let mut font_cx = FontContext::new();
        if font_cx.collection.family_names().next().is_none() {
            return Err(InvalidScene::new("no native fonts are available"));
        }
        Ok(Self {
            font_cx,
            layout_cx: LayoutContext::default(),
            family: if family.is_empty() {
                "sans-serif".into()
            } else {
                family
            },
        })
    }

    /// Measures one plain text request using the same shaping as drawing.
    ///
    /// # Errors
    /// Rejects invalid text, excessive measurement input or unbounded geometry.
    pub fn measure_plain(&mut self, text: &Text) -> Result<TextMeasurement, InvalidScene> {
        if text.text.len() > MAX_MEASURE_TEXT_BYTES {
            return Err(InvalidScene::new("text measurement source exceeds limit"));
        }
        let mut canvas = vivid_protocol::vector::Canvas::new();
        canvas.push(vivid_protocol::vector::Command::Text(text.clone()))?;
        let layout = self.shape_overlay(text);
        let mut measured = layout_geometry(&layout, &text.text, usize::MAX)?;
        measured.width = Scalar::new(if text.text.is_empty() {
            0.
        } else {
            f64::from(layout.full_width())
        })?;
        measured.height = Scalar::new(f64::from(layout.height()))?;
        Ok(measured)
    }
}
impl TextShaper for FontSystem {
    fn shape_overlay(&mut self, text: &vivid_protocol::vector::Text) -> Layout<()> {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, &text.text, 1.0, true);
        if text.family.is_empty() {
            builder.push_default(parley::FontFamily::from(self.family.as_str()));
        } else {
            builder.push_default(FontFamily::from(text.family.as_str()));
        }
        builder.push_default(StyleProperty::FontSize(text.size.get() as f32));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(f32::from(
            text.weight,
        ))));
        builder.push_default(StyleProperty::FontStyle(if text.italic {
            ParleyFontStyle::Italic
        } else {
            ParleyFontStyle::Normal
        }));
        builder.push_default(StyleProperty::Locale(None));
        let mut layout = builder.build(&text.text);
        layout.break_all_lines(text.max_width.map(|width| width.get() as f32));
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }
}
impl StyledTextShaper for FontSystem {
    fn shape_styled(
        &mut self,
        text: &vivid_protocol::overlay::wire::text::styled::StyledText,
    ) -> Result<Layout<usize>, InvalidScene> {
        use vivid_protocol::overlay::wire::text::styled::TextAlignment;
        if text.runs.is_empty() {
            return Err(InvalidScene::new("styled text has no runs"));
        }
        let source = text.text();
        let mut context = LayoutContext::<usize>::default();
        let mut builder = context.ranged_builder(&mut self.font_cx, &source, 1.0, true);
        builder.push_default(parley::FontFamily::from(self.family.as_str()));
        builder.push_default(StyleProperty::Locale(None));
        // Empty paragraphs still need the requested line height.
        builder.push_default(StyleProperty::LetterSpacing(
            text.typography.letter_spacing.get() as f32,
        ));
        builder.push_default(StyleProperty::WordSpacing(
            text.typography.word_spacing.get() as f32,
        ));
        if let Some(height) = text.typography.line_height {
            builder.push_default(LineHeight::Absolute(height.get() as f32));
        }
        builder.push_default(parley::FontFeatures::from(
            match (text.typography.ligatures, text.typography.kerning) {
                (true, true) => "",
                (true, false) => "\"kern\" 0",
                (false, true) => "\"liga\" 0, \"clig\" 0, \"dlig\" 0, \"calt\" 0",
                (false, false) => "\"liga\" 0, \"clig\" 0, \"dlig\" 0, \"calt\" 0, \"kern\" 0",
            },
        ));
        builder.push_default(StyleProperty::FontSize(text.runs[0].style.size.get() as f32));
        let mut start = 0;
        for (index, run) in text.runs.iter().enumerate() {
            let end = start + run.text.len();
            let range = start..end;
            let style = &run.style;
            if !style.family.is_empty() {
                builder.push(FontFamily::from(style.family.as_str()), range.clone());
            }
            builder.push(
                StyleProperty::FontSize(style.size.get() as f32),
                range.clone(),
            );
            builder.push(
                StyleProperty::FontWeight(FontWeight::new(f32::from(style.weight))),
                range.clone(),
            );
            builder.push(
                StyleProperty::FontStyle(if style.italic {
                    ParleyFontStyle::Italic
                } else {
                    ParleyFontStyle::Normal
                }),
                range.clone(),
            );
            builder.push(StyleProperty::Brush(index), range);
            start = end;
        }
        let mut layout = builder.build(&source);
        let width = text.max_width.map(|w| w.get() as f32);
        if !text.wrap && width.is_some() {
            let mut breaker = layout.break_lines();
            breaker.state_mut().set_layout_max_advance(f32::MAX);
            breaker.state_mut().set_line_max_advance(f32::MAX);
            while breaker.break_next().is_some() {
                breaker.set_prior_line_width(width.unwrap_or(f32::MAX));
            }
        } else {
            layout.break_all_lines(width);
        }
        layout.align(
            match text.alignment {
                TextAlignment::Start => Alignment::Start,
                TextAlignment::Center => Alignment::Center,
                TextAlignment::End => Alignment::End,
                TextAlignment::Justify => Alignment::Justify,
                _ => Alignment::Start,
            },
            AlignmentOptions::default(),
        );
        Ok(layout)
    }
}
impl std::fmt::Debug for FontSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSystem").finish_non_exhaustive()
    }
}
