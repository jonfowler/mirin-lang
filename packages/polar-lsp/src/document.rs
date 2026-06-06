//! Per-document state: a [`ropey`] rope (the editable text, with O(log n)
//! line/column ↔ byte conversion) plus the tree-sitter [`Tree`] for that text.
//!
//! M1 syncs incrementally: an edit updates the rope, builds a tree-sitter
//! [`InputEdit`], and reparses with the old tree so reparse cost is
//! proportional to the edit, not the file. A range-less change (or a client
//! that only sends full text) still falls back to a whole-document replace.

use polar_compiler::language;
use ropey::Rope;
use tower_lsp_server::ls_types::Range;
use tree_sitter::{InputEdit, Parser, Tree};

use crate::encoding::{Encoding, byte_to_point, position_to_byte, position_to_point};

pub struct Document {
    pub rope: Rope,
    pub tree: Tree,
}

/// Parse `text`, reusing `old` for incremental reparse when present.
fn parse(text: &str, old: Option<&Tree>) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&language())
        .expect("polar grammar is a valid tree-sitter language");
    parser
        .parse(text, old)
        .expect("tree-sitter parse without a cancellation flag always yields a tree")
}

impl Document {
    /// Build a document from its initial full text.
    pub fn open(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            tree: parse(text, None),
        }
    }

    /// Replace the whole document and reparse from scratch.
    pub fn apply_full(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.tree = parse(text, None);
    }

    /// Apply a single ranged change, then incrementally reparse.
    pub fn apply_incremental(&mut self, range: Range, text: &str, enc: Encoding) {
        let start_byte = position_to_byte(&self.rope, range.start, enc);
        let old_end_byte = position_to_byte(&self.rope, range.end, enc);
        let start_position = position_to_point(&self.rope, range.start, enc);
        let old_end_position = position_to_point(&self.rope, range.end, enc);

        // Edit the rope in char space (ropey indexes by char).
        let start_char = self.rope.byte_to_char(start_byte);
        let old_end_char = self.rope.byte_to_char(old_end_byte);
        self.rope.remove(start_char..old_end_char);
        self.rope.insert(start_char, text);

        let new_end_byte = start_byte + text.len();
        let new_end_position = byte_to_point(&self.rope, new_end_byte);

        self.tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        });
        let new_text = self.rope.to_string();
        self.tree = parse(&new_text, Some(&self.tree));
    }
}

#[cfg(test)]
mod tests {
    use tower_lsp_server::ls_types::Position;

    use super::*;

    #[test]
    fn incremental_edit_spanning_lines_updates_rope_and_reparses() {
        let mut doc = Document::open("ab\ncd");
        // Replace the span "b\nc" (line 0 col 1 .. line 1 col 1) with "X".
        let range = Range {
            start: Position::new(0, 1),
            end: Position::new(1, 1),
        };
        doc.apply_incremental(range, "X", Encoding::Utf8);
        assert_eq!(doc.rope.to_string(), "aXd");
        // The reparsed tree must cover exactly the new text.
        assert_eq!(doc.tree.root_node().end_byte(), "aXd".len());
    }
}
