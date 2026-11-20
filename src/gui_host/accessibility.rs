//! Projects validated readable text into an owner-scoped GPUI subtree.

// Rust guideline compliant 2026-10-07
use crate::{
    accessible_text::Document,
    gui::{
        A11ySubtreeBuilder, AccessibleNode, AccessibleTextPosition, AccessibleTextSelection, Role,
    },
};

/// Populates a text element with bounded physical rows and selection geometry.
///
/// Call from an element's synthetic-child callback. The element must have a
/// unique owner-qualified ID and a text role. Child IDs derive from that parent,
/// so two applications can safely reuse the same local row keys. No actions or
/// filesystem authority are attached to this read-only document.
pub fn populate(builder: &mut A11ySubtreeBuilder<'_>, document: &Document) {
    let Some(bounds) = builder.parent_node().bounds() else {
        return;
    };
    let width = bounds.x1 - bounds.x0;
    let height = bounds.y1 - bounds.y0;
    if ![bounds.x0, bounds.y0, width, height]
        .iter()
        .all(|n| n.is_finite())
        || width <= 0.
        || height <= 0.
        || width > f64::from(f32::MAX)
        || height > f64::from(f32::MAX)
    {
        return;
    }
    builder.parent_node().set_read_only();
    builder.parent_node().set_label(document.label());
    let mut first_selected_row = None;
    for line in document.lines() {
        let id = builder.synthetic_node_id(line.key());
        let [x, y, w, h] = line.rect().map(f64::from);
        let mut node = AccessibleNode::new(Role::TextRun);
        let mut rect = bounds;
        rect.x0 += x * width;
        rect.y0 += y * height;
        rect.x1 = rect.x0 + w * width;
        rect.y1 = rect.y0 + h * height;
        node.set_bounds(rect);
        // Row metrics measure offsets from the left edge of the terminal grid.
        node.set_text_direction(crate::gui::AccessibleTextDirection::LeftToRight);
        node.set_value(line.text());
        node.set_character_lengths(line.character_lengths().to_vec());
        node.set_character_positions(
            line.character_positions()
                .iter()
                .map(|x| *x * (w * width) as f32)
                .collect::<Vec<_>>(),
        );
        node.set_character_widths(
            line.character_widths()
                .iter()
                .map(|x| *x * (w * width) as f32)
                .collect::<Vec<_>>(),
        );
        if let Some(range) = line.selected_characters() {
            // AccessKit exposes one selection per text provider. A selected row
            // gets its own provider; the terminal still contains every text run.
            let row_id = builder.synthetic_node_id(("selected-row", line.key()));
            let mut row = AccessibleNode::new(Role::Label);
            row.set_bounds(rect);
            row.set_read_only();
            let selection = AccessibleTextSelection {
                anchor: AccessibleTextPosition {
                    node: id,
                    character_index: range.start,
                },
                focus: AccessibleTextPosition {
                    node: id,
                    character_index: range.end,
                },
            };
            first_selected_row.get_or_insert(selection);
            row.set_text_selection(selection);
            builder.push_subtree(row_id, row, |row| {
                row.push_child(id, node);
            });
        } else {
            builder.push_child(id, node);
        }
    }
    if let Some(selection) = document.selection() {
        let anchor = AccessibleTextPosition {
            node: builder.synthetic_node_id(selection.anchor.line),
            character_index: selection.anchor.character,
        };
        let focus = AccessibleTextPosition {
            node: builder.synthetic_node_id(selection.focus.line),
            character_index: selection.focus.character,
        };
        builder
            .parent_node()
            .set_text_selection(AccessibleTextSelection { anchor, focus });
    } else if let Some(selection) = first_selected_row {
        builder.parent_node().set_text_selection(selection);
    }
}
