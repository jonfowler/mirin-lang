//! The one place LSP `Position`s (line + character) convert to/from byte
//! offsets and tree-sitter [`Point`]s. Position encoding is the #1 LSP bug
//! class (`planning/lsp.md`): the `character` field is UTF-16 code units by
//! default, or UTF-8 bytes when negotiated. tree-sitter is byte-based and
//! [`Point::column`] is a *byte* column, so every boundary crossing routes
//! through here.
//!
//! All conversions clamp to the document and snap to char boundaries, so a
//! malformed client position can never panic the rope.

use ropey::Rope;
use tower_lsp_server::ls_types::{Position, PositionEncodingKind};
use tree_sitter::Point;

/// The negotiated position encoding (`initialize`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    /// `character` is a UTF-8 byte offset within the line.
    Utf8,
    /// `character` is a UTF-16 code-unit offset within the line (LSP default).
    Utf16,
}

impl Encoding {
    pub fn from_kind(kind: &PositionEncodingKind) -> Self {
        if *kind == PositionEncodingKind::UTF8 {
            Encoding::Utf8
        } else {
            Encoding::Utf16
        }
    }
}

/// LSP [`Position`] → absolute byte offset, clamped to the document and snapped
/// to a char boundary.
pub fn position_to_byte(rope: &Rope, pos: Position, enc: Encoding) -> usize {
    let last_line = rope.len_lines().saturating_sub(1);
    let line = (pos.line as usize).min(last_line);
    let line_start_char = rope.line_to_char(line);
    // Bound `character` by the line's *content* end — exclude the trailing
    // line break so an over-long character clamps to end-of-line, not the
    // start of the next line.
    let line_end_char = rope.byte_to_char(line_content_end_byte(rope, line));

    let char_idx = match enc {
        Encoding::Utf8 => {
            let target_byte = rope.char_to_byte(line_start_char) + pos.character as usize;
            let max_byte = rope.char_to_byte(line_end_char);
            // Snap to a char boundary at or before the target, within the line.
            rope.byte_to_char(target_byte.min(max_byte))
        }
        Encoding::Utf16 => {
            let target_cu = rope.char_to_utf16_cu(line_start_char) + pos.character as usize;
            let max_cu = rope.char_to_utf16_cu(line_end_char);
            rope.utf16_cu_to_char(target_cu.min(max_cu))
        }
    };
    rope.char_to_byte(char_idx)
}

/// LSP [`Position`] → tree-sitter [`Point`] (row + **byte** column), for
/// building an [`tree_sitter::InputEdit`].
pub fn position_to_point(rope: &Rope, pos: Position, enc: Encoding) -> Point {
    byte_to_point(rope, position_to_byte(rope, pos, enc))
}

/// Absolute byte offset → tree-sitter [`Point`] (row + byte column).
pub fn byte_to_point(rope: &Rope, byte: usize) -> Point {
    let byte = byte.min(rope.len_bytes());
    let row = rope.byte_to_line(byte);
    let column = byte - rope.line_to_byte(row);
    Point { row, column }
}

/// Absolute byte offset → LSP [`Position`], in the negotiated encoding.
pub fn byte_to_position(rope: &Rope, byte: usize, enc: Encoding) -> Position {
    let byte = byte.min(rope.len_bytes());
    let char_idx = rope.byte_to_char(byte);
    let line = rope.char_to_line(char_idx);
    let line_start_char = rope.line_to_char(line);
    let character = match enc {
        Encoding::Utf8 => rope.char_to_byte(char_idx) - rope.char_to_byte(line_start_char),
        Encoding::Utf16 => rope.char_to_utf16_cu(char_idx) - rope.char_to_utf16_cu(line_start_char),
    };
    Position {
        line: line as u32,
        character: character as u32,
    }
}

/// The byte offset of a line's content end — i.e. the start of its trailing
/// line break (`\n` or `\r\n`), or end-of-rope for the last line. Used to keep
/// positions and tokens from spilling onto the line terminator.
pub fn line_content_end_byte(rope: &Rope, line: usize) -> usize {
    let start_char = rope.line_to_char(line);
    let slice = rope.line(line);
    let mut content = slice.len_chars();
    if content > 0 && slice.char(content - 1) == '\n' {
        content -= 1;
    }
    if content > 0 && slice.char(content - 1) == '\r' {
        content -= 1;
    }
    rope.char_to_byte(start_char + content)
}

#[cfg(test)]
mod tests {
    use super::*;

    // "café\nx": é is 2 UTF-8 bytes / 1 UTF-16 code unit / 1 char.
    // bytes: c=0 a=1 f=2 é=3..5 \n=5 x=6  (line 0 = "café", line 1 = "x")
    const SRC: &str = "café\nx";

    #[test]
    fn utf8_vs_utf16_end_of_line_with_multibyte() {
        let rope = Rope::from_str(SRC);
        // End of "café": UTF-8 sees 5 bytes, UTF-16 sees 4 code units — both
        // must land on the same byte (5, the newline).
        assert_eq!(
            position_to_byte(&rope, Position::new(0, 5), Encoding::Utf8),
            5
        );
        assert_eq!(
            position_to_byte(&rope, Position::new(0, 4), Encoding::Utf16),
            5
        );
    }

    #[test]
    fn byte_to_position_roundtrips_both_encodings() {
        let rope = Rope::from_str(SRC);
        for enc in [Encoding::Utf8, Encoding::Utf16] {
            for byte in 0..=rope.len_bytes() {
                // Only boundary bytes roundtrip exactly; snap first.
                let snapped = rope.char_to_byte(rope.byte_to_char(byte));
                let pos = byte_to_position(&rope, snapped, enc);
                assert_eq!(
                    position_to_byte(&rope, pos, enc),
                    snapped,
                    "roundtrip failed at byte {byte} ({enc:?})"
                );
            }
        }
    }

    #[test]
    fn start_of_second_line() {
        let rope = Rope::from_str(SRC);
        // 'x' begins at byte 6, line 1 col 0.
        assert_eq!(
            byte_to_position(&rope, 6, Encoding::Utf8),
            Position::new(1, 0)
        );
        assert_eq!(
            position_to_byte(&rope, Position::new(1, 0), Encoding::Utf16),
            6
        );
    }

    #[test]
    fn character_past_end_of_line_clamps() {
        let rope = Rope::from_str(SRC);
        // Way past the end of line 0 clamps to the newline byte, not a panic.
        let byte = position_to_byte(&rope, Position::new(0, 999), Encoding::Utf8);
        assert_eq!(byte, 5);
    }

    #[test]
    fn point_column_is_bytes() {
        let rope = Rope::from_str(SRC);
        // Just after é on line 0: byte column 5 (UTF-8 bytes), row 0.
        let p = position_to_point(&rope, Position::new(0, 5), Encoding::Utf8);
        assert_eq!(p, Point { row: 0, column: 5 });
    }
}
