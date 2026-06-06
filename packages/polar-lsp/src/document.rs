//! Per-document state: a [`ropey`] rope (the editable text, with O(log n)
//! line/column ↔ byte conversion for later position mapping) plus the
//! tree-sitter [`Tree`] for that text.
//!
//! M0 uses `FULL` text sync — every edit replaces the whole document and
//! reparses from scratch (`planning/lsp.md`'s sanctioned v0 shortcut).
//! Incremental sync (`Tree::edit` + a reused parser) lands in M1.

use polar_db::parse_text;
use ropey::Rope;
use tree_sitter::Tree;

pub struct Document {
    pub rope: Rope,
    pub tree: Tree,
}

impl Document {
    /// Build a document from its initial full text.
    pub fn open(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            tree: parse_text(text),
        }
    }

    /// Replace the whole document (M0 `FULL`-sync path) and reparse.
    pub fn set_text(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.tree = parse_text(text);
    }
}
