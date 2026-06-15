//! `mirin-fmt` — a rustfmt-shaped formatter for the Mirin HDL.
//!
//! The pipeline is: parse with the tree-sitter grammar, lower the CST into a
//! Wadler/Prettier-style [`Doc`](doc::Doc), then render at a target width.
//! Formatting re-derives all whitespace from the tree, so it is deterministic
//! and idempotent. Line comments and single blank lines are preserved.

mod doc;
mod format;

// The tree-sitter grammar is owned by `mirin-compiler`; we reuse its parser so
// the workspace links a single copy of the C grammar.
pub use mirin_compiler::parse_text;
use tree_sitter::Tree;

use crate::doc::MAX_WIDTH;
use crate::format::Formatter;

/// Why a source string could not be formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The input did not parse cleanly (tree-sitter produced ERROR/MISSING
    /// nodes). We refuse to format rather than risk corrupting source.
    Parse,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Parse => write!(f, "input has syntax errors; not formatting"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Format Mirin source at the default width (100 columns).
pub fn format_str(source: &str) -> Result<String, FormatError> {
    format_str_width(source, MAX_WIDTH)
}

/// Format Mirin source at a caller-chosen width. Useful for tests that want to
/// exercise breaking at narrow widths.
pub fn format_str_width(source: &str, width: usize) -> Result<String, FormatError> {
    let tree = parse_text(source);
    format_tree_width(source, &tree, width)
}

/// Format from an already-parsed tree, at the default width. Lets callers that
/// already hold a [`Tree`] (e.g. an editor keeping a live parse) avoid
/// re-parsing. `tree` must be the parse of `source`.
pub fn format_tree(source: &str, tree: &Tree) -> Result<String, FormatError> {
    format_tree_width(source, tree, MAX_WIDTH)
}

/// Format from an already-parsed tree at a caller-chosen width.
pub fn format_tree_width(source: &str, tree: &Tree, width: usize) -> Result<String, FormatError> {
    let root = tree.root_node();
    if root.has_error() {
        return Err(FormatError::Parse);
    }
    let formatter = Formatter::new(source);
    let document = formatter.format(root);
    Ok(doc::render(&document, width))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn examples_dir(sub: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples")
            .join(sub)
    }

    /// Pre-order sequence of named node kinds, ignoring comments (which the
    /// formatter may keep but whose surrounding trivia changes). Two sources
    /// with the same sequence have the same parse structure.
    fn kind_skeleton(source: &str) -> Vec<String> {
        fn walk(node: tree_sitter::Node, out: &mut Vec<String>) {
            if node.is_named() && node.kind() != "comment" {
                out.push(node.kind().to_string());
            }
            let mut c = node.walk();
            for child in node.children(&mut c) {
                walk(child, out);
            }
        }
        let tree = parse_text(source);
        let mut out = Vec::new();
        walk(tree.root_node(), &mut out);
        out
    }

    fn working_examples() -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(examples_dir("working"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map_or(false, |x| x == "mrn"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn every_working_example_formats_idempotently_and_preserves_structure() {
        for path in working_examples() {
            let src = fs::read_to_string(&path).unwrap();
            let once = format_str(&src).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let twice =
                format_str(&once).unwrap_or_else(|e| panic!("{} (reformat): {e}", path.display()));
            assert_eq!(once, twice, "not idempotent: {}", path.display());
            assert_eq!(
                kind_skeleton(&src),
                kind_skeleton(&once),
                "formatting changed parse structure: {}",
                path.display()
            );
            assert!(
                once.ends_with('\n'),
                "missing trailing newline: {}",
                path.display()
            );
        }
    }

    /// `fail-expected/` mixes true parse errors (`missing-*`) with files that
    /// parse fine but fail *semantic* checks (`duplicate-var`, `undefined-name`).
    /// A formatter must refuse only the former and happily format the latter.
    #[test]
    fn refuses_parse_errors_but_formats_semantically_invalid() {
        for entry in fs::read_dir(examples_dir("fail-expected")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().map_or(true, |x| x != "mrn") {
                continue;
            }
            let src = fs::read_to_string(&path).unwrap();
            let has_parse_error = parse_text(&src).root_node().has_error();
            let result = format_str(&src);
            if has_parse_error {
                assert_eq!(
                    result,
                    Err(FormatError::Parse),
                    "should refuse: {}",
                    path.display()
                );
            } else {
                let out = result.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert_eq!(
                    kind_skeleton(&src),
                    kind_skeleton(&out),
                    "structure changed: {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn collapses_short_constructs_to_one_line() {
        let src = "fn  add3 ( x : uint(8) )->uint(8){return x+3;}\n";
        let expected = "\
fn add3(x: uint(8)) -> uint(8) {
    return x + 3;
}
";
        assert_eq!(format_str(src).unwrap(), expected);
    }

    #[test]
    fn unary_operator_is_preserved() {
        // Regression: the unary doc once hardcoded `-`, rewriting `!x` to `-x`.
        // Each prefix operator must round-trip to itself.
        let src = "fn f(a: bool, x: sint(8)) -> bool { let n = -x; !a }\n";
        let out = format_str(src).unwrap();
        assert!(out.contains("let n = -x;"), "got:\n{out}");
        assert!(out.contains("!a"), "got:\n{out}");
        assert!(!out.contains("-a"), "`!` must not become `-`:\n{out}");
    }

    #[test]
    fn single_line_if_else_stays_inline() {
        let src = "fn m(a: uint(8), b: uint(8), c: bool) -> uint(8) { if c { a } else { b } }\n";
        let out = format_str(src).unwrap();
        assert!(out.contains("    if c { a } else { b }\n"), "got:\n{out}");
    }

    #[test]
    fn long_signature_breaks_sections() {
        let src = "fn f { dom clk: Clock, rstn: Reset @clk = high, c: uint(8) @clk = 0 } (a: uint(8) @clk, b: uint(8) @clk) -> uint(8) @clk { return a; }\n";
        let out = format_str(src).unwrap();
        // The named and positional sections each land on their own line, with
        // the named section unpadded to match the positional `(…)` style.
        assert!(out.contains("\n    {dom clk: Clock"), "got:\n{out}");
        assert!(
            out.contains("\n    (a: uint(8) @clk, b: uint(8) @clk)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn narrow_width_breaks_record_literal_one_per_line() {
        // Record *literals* (unlike definitions) collapse when they fit and
        // break by width — exercise the width-based delimited path.
        let src = "fn f() -> Packet { packet { valid = false, payload = 0 } }\n";
        let wide = format_str_width(src, 100).unwrap();
        assert!(
            wide.contains("packet { valid = false, payload = 0 }"),
            "should collapse when it fits:\n{wide}"
        );
        let narrow = format_str_width(src, 24).unwrap();
        assert!(
            narrow.contains("packet {\n"),
            "should break when narrow:\n{narrow}"
        );
        assert!(
            narrow.contains("        valid = false,\n"),
            "got:\n{narrow}"
        );
    }

    #[test]
    fn struct_definition_is_always_vertical_even_when_short() {
        let src = "struct P = p { a: bool, b: bool }\n";
        let expected = "\
struct P = p {
    a: bool,
    b: bool,
}
";
        assert_eq!(format_str(src).unwrap(), expected);
    }

    #[test]
    fn port_header_stays_on_one_line_with_vertical_body() {
        let src = "port S { dom clk: Clock } = s { in ready: bool @clk, out valid: bool @clk }\n";
        let out = format_str(src).unwrap();
        assert!(
            out.starts_with("port S {dom clk: Clock} = s {\n"),
            "got:\n{out}"
        );
        assert!(out.contains("\n    in ready: bool @clk,\n"), "got:\n{out}");
    }

    #[test]
    fn method_chain_breaks_before_dots_when_over_width() {
        // Two links, narrow width: receiver + first link on line 1, rest below.
        let src = "fn f() -> uint(8) { let y = recv.alpha(p).beta(q); return y; }\n";
        let out = format_str_width(src, 28).unwrap();
        assert!(out.contains("let y = recv.alpha(p)\n"), "got:\n{out}");
        assert!(out.contains("\n        .beta(q);\n"), "got:\n{out}");
    }

    #[test]
    fn single_call_does_not_chain_break() {
        // One link is a plain call, never a chain — its args break instead.
        let src = "fn f() -> uint(8) { let y = recv.only(aaaa, bbbb); return y; }\n";
        let out = format_str_width(src, 24).unwrap();
        assert!(
            !out.contains("\n        .only"),
            "should not chain-break:\n{out}"
        );
    }

    #[test]
    fn comments_in_argument_lists_are_preserved_verbatim() {
        let src = "fn f() {\n    g(\n        a, // first\n        b,\n    );\n}\n";
        let out = format_str(src).unwrap();
        // rustfmt-style: we don't reformat a comment-bearing list; the original
        // text (including the trailing comment) survives intact.
        assert!(out.contains("a, // first"), "comment lost:\n{out}");
        assert!(out.contains('b'), "arg lost:\n{out}");
    }

    #[test]
    fn collapses_runs_of_blank_lines() {
        let src = "fn a() {\n    return 1;\n}\n\n\n\nfn b() {\n    return 2;\n}\n";
        let out = format_str(src).unwrap();
        assert!(
            !out.contains("\n\n\n"),
            "blank-line run not collapsed:\n{out}"
        );
        assert!(
            out.contains("}\n\nfn b"),
            "single blank not preserved:\n{out}"
        );
    }
}
