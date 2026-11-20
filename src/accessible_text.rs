//! Bounded read-only text documents for native accessibility adapters.

// Rust guideline compliant 2026-10-07
use std::{collections::HashSet, fmt, ops::Range};

/// Maximum physical rows in one snapshot.
pub const MAX_LINES: usize = 512;
/// Maximum Unicode scalars, including line breaks, in one snapshot.
pub const MAX_CHARACTERS: usize = 262_144;
/// Maximum UTF-8 text bytes across all rows.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 bytes in the document label.
pub const MAX_LABEL_BYTES: usize = 4096;

/// Invalid geometry, text limits, identity or selection in a readable document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidDocument;
impl fmt::Display for InvalidDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid or oversized accessible text document")
    }
}
impl std::error::Error for InvalidDocument {}

/// A row and Unicode-scalar boundary within it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    /// Stable local row identity, scoped by the embedding document owner.
    pub line: u64,
    /// Scalar boundary from zero through the row's character count.
    pub character: usize,
}
/// A directional text selection, or caret when both endpoints are equal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    /// Fixed selection endpoint.
    pub anchor: Position,
    /// Moving selection endpoint.
    pub focus: Position,
}

/// One physical text row with normalized character geometry.
#[derive(Clone, PartialEq)]
pub struct Line {
    key: u64,
    text: String,
    rect: [f32; 4],
    lengths: Vec<u8>,
    positions: Vec<f32>,
    widths: Vec<f32>,
    selected_characters: Option<Range<usize>>,
}
impl Line {
    /// Selected scalar boundaries on this row of a discontiguous selection.
    pub fn selected_characters(&self) -> Option<Range<usize>> {
        self.selected_characters.clone()
    }

    /// Returns the local identity of this physical row.
    pub fn key(&self) -> u64 {
        self.key
    }
    /// Borrows the row's exact UTF-8 text, including an optional line break.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Returns normalized x/y/width/height relative to the complete document viewport.
    pub fn rect(&self) -> [f32; 4] {
        self.rect
    }
    /// Returns UTF-8 byte lengths for individual Unicode scalars.
    pub fn character_lengths(&self) -> &[u8] {
        &self.lengths
    }
    /// Returns scalar x positions relative to this row's width.
    pub fn character_positions(&self) -> &[f32] {
        &self.positions
    }
    /// Returns scalar widths relative to this row's width.
    pub fn character_widths(&self) -> &[f32] {
        &self.widths
    }
}
impl fmt::Debug for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessibleLine")
            .field("key", &self.key)
            .field("bytes", &self.text.len())
            .field("characters", &self.lengths.len())
            .finish_non_exhaustive()
    }
}

/// An immutable validated document without platform handles or action authority.
#[derive(Clone, PartialEq)]
pub struct Document {
    label: String,
    lines: Vec<Line>,
    selection: Option<Selection>,
    focused: bool,
}
impl Document {
    /// Borrows the document's bounded descriptive label.
    pub fn label(&self) -> &str {
        &self.label
    }
    /// Borrows the physical rows in reading order.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }
    /// Returns the visible selection or caret, if both endpoints are available.
    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }
    /// Whether the producer considers its text document focused.
    pub fn is_focused(&self) -> bool {
        self.focused
    }
    /// Converts a flattened scalar boundary to a row-local position.
    pub fn position_at(&self, mut scalar: usize) -> Option<Position> {
        for (index, line) in self.lines.iter().enumerate() {
            if scalar < line.lengths.len()
                || index + 1 == self.lines.len() && scalar == line.lengths.len()
            {
                return Some(Position {
                    line: line.key,
                    character: scalar,
                });
            }
            scalar = scalar.checked_sub(line.lengths.len())?;
        }
        None
    }
}
impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessibleDocument")
            .field("lines", &self.lines.len())
            .field("focused", &self.focused)
            .finish_non_exhaustive()
    }
}

/// Streams rows into a document under aggregate text and geometry limits.
pub struct Builder {
    document: Document,
    keys: HashSet<u64>,
    characters: usize,
    bytes: usize,
}
impl Builder {
    /// Creates an empty bounded document builder.
    ///
    /// # Errors
    /// Rejects labels exceeding the byte ceiling before copying them.
    pub fn new(label: &str) -> Result<Self, InvalidDocument> {
        if label.len() > MAX_LABEL_BYTES {
            return Err(InvalidDocument);
        }
        Ok(Self {
            document: Document {
                label: label.to_owned(),
                lines: Vec::new(),
                selection: None,
                focused: false,
            },
            keys: HashSet::new(),
            characters: 0,
            bytes: 0,
        })
    }
    /// Appends a complete row with normalized scalar x positions and widths.
    ///
    /// Geometry uses x/y/width/height in the document's unit square. Character
    /// positions and widths use the row's unit interval. Zero widths support
    /// combining marks and line breaks. Failed rows leave the builder unchanged.
    ///
    /// # Errors
    /// Rejects duplicate IDs, invalid geometry and excess rows, scalars or bytes.
    pub fn push_line(
        &mut self,
        key: u64,
        rect: [f32; 4],
        characters: impl IntoIterator<Item = (char, f32, f32)>,
    ) -> Result<(), InvalidDocument> {
        self.push_line_with_selection(key, rect, characters, None)
    }
    /// Appends a physical row with an optional row-local selection.
    ///
    /// This preserves each span of a rectangular selection, including on soft-wrapped rows.
    ///
    /// # Errors
    /// Applies the same limits as `push_line` and rejects reversed or out-of-row spans.
    pub fn push_line_with_selection(
        &mut self,
        key: u64,
        rect: [f32; 4],
        characters: impl IntoIterator<Item = (char, f32, f32)>,
        selected_characters: Option<Range<usize>>,
    ) -> Result<(), InvalidDocument> {
        let [x, y, w, h] = rect;
        if self.document.lines.len() >= MAX_LINES
            || self.keys.contains(&key)
            || !rect.iter().all(|n| n.is_finite() && *n >= 0. && *n <= 1.)
            || w <= 0.
            || h <= 0.
            || x + w > 1.
            || y + h > 1.
        {
            return Err(InvalidDocument);
        }
        let mut line = Line {
            key,
            rect,
            text: String::new(),
            lengths: Vec::new(),
            positions: Vec::new(),
            widths: Vec::new(),
            selected_characters,
        };
        let mut previous = 0.;
        for (character, position, width) in characters {
            let count = self
                .characters
                .checked_add(line.lengths.len())
                .and_then(|n| n.checked_add(1));
            let bytes = self
                .bytes
                .checked_add(line.text.len())
                .and_then(|n| n.checked_add(character.len_utf8()));
            if count.is_none_or(|n| n > MAX_CHARACTERS)
                || bytes.is_none_or(|n| n > MAX_TEXT_BYTES)
                || !position.is_finite()
                || !width.is_finite()
                || position < previous
                || position < 0.
                || width < 0.
                || position + width > 1.
            {
                return Err(InvalidDocument);
            }
            previous = position;
            line.text.push(character);
            line.lengths.push(character.len_utf8() as u8);
            line.positions.push(position);
            line.widths.push(width);
        }
        if line
            .selected_characters
            .as_ref()
            .is_some_and(|r| r.start > r.end || r.end > line.lengths.len())
        {
            return Err(InvalidDocument);
        }
        self.bytes += line.text.len();
        self.characters += line.lengths.len();
        self.keys.insert(key);
        self.document.lines.push(line);
        Ok(())
    }
    /// Resolves a flattened scalar boundary against the currently retained rows.
    pub fn position_at(&self, scalar: usize) -> Option<Position> {
        self.document.position_at(scalar)
    }
    /// Finishes the document with an optional validated caret or selection.
    ///
    /// # Errors
    /// Rejects endpoints outside the retained rows or inside nonexistent characters.
    pub fn finish(
        mut self,
        selection: Option<Selection>,
        focused: bool,
    ) -> Result<Document, InvalidDocument> {
        if let Some(selection) = selection {
            if self
                .document
                .lines
                .iter()
                .any(|line| line.selected_characters.is_some())
            {
                return Err(InvalidDocument);
            }
            for position in [selection.anchor, selection.focus] {
                if !self.document.lines.iter().any(|line| {
                    line.key == position.line && position.character <= line.lengths.len()
                }) {
                    return Err(InvalidDocument);
                }
            }
        }
        self.document.selection = selection;
        self.document.focused = focused;
        Ok(self.document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_rows_and_selection_keep_scalar_boundaries_without_debug_text() {
        let mut b = Builder::new("private title").unwrap();
        b.push_line(
            7,
            [0., 0., 1., 0.5],
            [('😀', 0., 0.2), ('\u{301}', 0., 0.), ('\n', 0.2, 0.)],
        )
        .unwrap();
        b.push_line(8, [0., 0.5, 1., 0.5], [('λ', 0., 0.1)])
            .unwrap();
        let selection = Selection {
            anchor: b.position_at(0).unwrap(),
            focus: b.position_at(4).unwrap(),
        };
        let doc = b.finish(Some(selection), true).unwrap();
        assert_eq!(doc.lines()[0].character_lengths(), &[4, 2, 1]);
        assert_eq!(
            doc.selection().unwrap().focus,
            Position {
                line: 8,
                character: 1
            }
        );
        assert!(!format!("{doc:?}").contains("private"));
    }
    #[test]
    fn invalid_selected_span_does_not_consume_the_row_key() {
        let mut b = Builder::new("text").unwrap();
        let rect = [0., 0., 1., 1.];
        let characters = [('🌱', 0., 1.)];
        assert!(
            b.push_line_with_selection(7, rect, characters, Some(0..4))
                .is_err()
        );
        b.push_line_with_selection(7, rect, characters, Some(0..1))
            .unwrap();
        assert_eq!(
            b.finish(None, true).unwrap().lines()[0].selected_characters(),
            Some(0..1)
        );
    }
    #[test]
    fn invalid_rows_do_not_mutate_existing_rows_or_budget() {
        let mut b = Builder::new("text").unwrap();
        b.push_line(0, [0., 0., 1., 1.], [('a', 0., 1.)]).unwrap();
        assert!(b.push_line(0, [0., 0., 1., 1.], []).is_err());
        assert!(
            b.push_line(1, [0., 0., 1., 1.], [('x', f32::NAN, 0.)])
                .is_err()
        );
        assert!(b.push_line(1, [0., 0., 1., 1.], [('x', 0.7, 0.4)]).is_err());
        assert!(
            b.push_line(1, [0., 0., 1., 1.], std::iter::repeat(('x', 0., 0.)))
                .is_err()
        );
        b.push_line(1, [0., 0., 1., 1.], [('b', 0., 1.)]).unwrap();
        assert_eq!(b.finish(None, false).unwrap().lines().len(), 2);
        assert!(Builder::new(&"x".repeat(MAX_LABEL_BYTES + 1)).is_err());
    }
}
