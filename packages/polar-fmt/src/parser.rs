//! Tree-sitter binding. The grammar's C entry point is linked by `build.rs`;
//! this mirrors the binding in `polar-compiler`/`polar-db` so the formatter is
//! a standalone crate that does not depend on either.

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

/// Parse source text into a tree-sitter [`Tree`]. Infallible: tree-sitter
/// always returns a tree (with ERROR/MISSING nodes on invalid input).
pub fn parse_text(text: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&language())
        .expect("polar grammar is a valid tree-sitter language");
    parser
        .parse(text, None)
        .expect("tree-sitter parse without a cancellation flag always yields a tree")
}
