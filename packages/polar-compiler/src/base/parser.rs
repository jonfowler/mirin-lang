//! Tree-sitter binding. The grammar's C entry point is linked by `build.rs`;
//! this module wraps it as a Rust [`Language`] and a single infallible
//! `parse_text`. Concrete syntax navigation (the `item_tree` lowering) lands in
//! Q1; for Q0 we only need to turn text into a [`Tree`].

use tree_sitter::{Language, Parser, Tree};
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_polar() -> *const ();
}

const LANGUAGE_FN: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_polar) };

/// The Polar tree-sitter [`Language`], linked from the C grammar by `build.rs`.
pub fn language() -> Language {
    Language::new(LANGUAGE_FN)
}

/// Parse source text into a tree-sitter [`Tree`].
///
/// Infallible: tree-sitter always returns a tree (with ERROR/MISSING nodes on
/// invalid input — the error recovery that makes it a good IDE frontend) unless
/// parsing is cancelled, which we never do.
pub fn parse_text(text: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&language())
        .expect("polar grammar is a valid tree-sitter language");
    parser
        .parse(text, None)
        .expect("tree-sitter parse without a cancellation flag always yields a tree")
}
