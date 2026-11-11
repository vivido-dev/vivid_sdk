// Copyright 2026 the Vivid Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Preserve the cluster base when no candidate covers the whole grapheme.

// Rust guideline compliant 2026-10-07
use parley_engine::shape::Coverage;

pub(super) fn prefer_coverage(
    candidate: Coverage,
    covers_base: bool,
    current: Coverage,
    current_covers_base: bool,
) -> bool {
    // Complete coverage, including normalization, always wins. Among incomplete
    // fonts, losing the base is worse than losing an unsupported combining mark.
    // Exact ties preserve the configured family order.
    (candidate.is_complete(), covers_base, candidate)
        > (current.is_complete(), current_covers_base, current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontContext, FontFamily, LayoutContext, PositionedLayoutItem};
    use alloc::sync::Arc;

    #[test]
    fn licensed_emoji_fixtures_preserve_the_base_when_the_primary_font_only_covers_the_mark() {
        for emoji in [
            include_bytes!("../../test-fonts/noto_color_emoji/NotoColorEmoji-Subset.ttf")
                .as_slice(),
            include_bytes!("../../test-fonts/noto_color_emoji/NotoColorEmoji-CBTF-Subset.ttf")
                .as_slice(),
        ] {
            let mut fonts = FontContext {
                collection: fontique::Collection::new(fontique::CollectionOptions {
                    shared: false,
                    system_fonts: false,
                }),
                source_cache: fontique::SourceCache::default(),
            };
            let latin = include_bytes!("../../test-fonts/roboto_fonts/Roboto-Regular.ttf");
            fonts.collection.register_fonts(
                linebender_resource_handle::Blob::new(Arc::new(latin.to_vec())),
                None,
            );
            let families = fonts.collection.register_fonts(
                linebender_resource_handle::Blob::new(Arc::new(emoji.to_vec())),
                None,
            );
            assert!(!families.is_empty());
            fonts.collection.set_generic_families(
                parlance::GenericFamily::Emoji,
                families.iter().map(|(id, _)| *id),
            );
            let mut context = LayoutContext::<()>::default();
            for source in ["🎉", "🎉\u{301}", "🎉\u{301}\u{302}\u{303}"] {
                let mut builder = context.ranged_builder(&mut fonts, source, 1., true);
                builder.push_default(FontFamily::from("Roboto"));
                let mut layout = builder.build(source);
                layout.break_all_lines(None);
                let ids = layout
                    .lines()
                    .flat_map(|line| line.items())
                    .filter_map(|item| match item {
                        PositionedLayoutItem::GlyphRun(run) => Some(
                            run.glyphs()
                                .map(|glyph| glyph.id)
                                .collect::<alloc::vec::Vec<_>>(),
                        ),
                        _ => None,
                    })
                    .flatten()
                    .collect::<alloc::vec::Vec<_>>();
                assert!(
                    ids.first().is_some_and(|id| *id != 0),
                    "emoji base lost: {source:?}, {ids:?}"
                );
                assert!(layout.full_width() > 0.);
            }
        }
    }

    #[test]
    fn complete_normalization_wins_then_base_then_coverage_then_family_order() {
        let one = Coverage {
            covered: 1,
            total: 3,
        };
        let two = Coverage {
            covered: 2,
            total: 3,
        };
        assert!(prefer_coverage(Coverage::COMPLETE, false, two, true));
        assert!(!prefer_coverage(two, true, Coverage::COMPLETE, false));
        assert!(prefer_coverage(one, true, two, false));
        assert!(!prefer_coverage(two, false, one, true));
        assert!(prefer_coverage(two, true, one, true));
        assert!(!prefer_coverage(one, true, one, true));
        assert!(!prefer_coverage(one, false, one, false));
    }
}
