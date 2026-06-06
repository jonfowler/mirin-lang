//! `polar-fmt` — a rustfmt-shaped formatter for the Polar HDL.
//!
//! The pipeline is: parse with the tree-sitter grammar, lower the CST into a
//! Wadler/Prettier-style [`Doc`](doc::Doc), then render at a target width.
//! Formatting re-derives all whitespace from the tree, so it is deterministic
//! and idempotent. Line comments and single blank lines are preserved.

mod doc;
mod format;
mod parser;

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

/// Format Polar source at the default width (100 columns).
pub fn format_str(source: &str) -> Result<String, FormatError> {
    format_str_width(source, MAX_WIDTH)
}

/// Format Polar source at a caller-chosen width. Useful for tests that want to
/// exercise breaking at narrow widths.
pub fn format_str_width(source: &str, width: usize) -> Result<String, FormatError> {
    let tree = parser::parse_text(source);
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
        let tree = parser::parse_text(source);
        let mut out = Vec::new();
        walk(tree.root_node(), &mut out);
        out
    }

    fn working_examples() -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(examples_dir("working"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map_or(false, |x| x == "plr"))
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
            if path.extension().map_or(true, |x| x != "plr") {
                continue;
            }
            let src = fs::read_to_string(&path).unwrap();
            let has_parse_error = parser::parse_text(&src).root_node().has_error();
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
    fn single_line_if_else_stays_inline() {
        let src = "fn m(a: uint(8), b: uint(8), c: bool) -> uint(8) { if c { a } else { b } }\n";
        let out = format_str(src).unwrap();
        assert!(out.contains("    if c { a } else { b }\n"), "got:\n{out}");
    }

    #[test]
    fn long_signature_breaks_sections() {
        let src = "fn f { dom clk: Clock, rstn: Reset @clk = high, c: uint(8) @clk = 0 } (a: uint(8) @clk, b: uint(8) @clk) -> uint(8) @clk { return a; }\n";
        let out = format_str(src).unwrap();
        // The named and positional sections each land on their own line.
        assert!(out.contains("\n    { dom clk: Clock"), "got:\n{out}");
        assert!(
            out.contains("\n    (a: uint(8) @clk, b: uint(8) @clk)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn narrow_width_breaks_lists_one_per_line() {
        let src = "struct Packet = packet { valid: bool, payload: uint(8) }\n";
        let out = format_str_width(src, 30).unwrap();
        let expected = "\
struct Packet = packet {
    valid: bool,
    payload: uint(8),
}
";
        assert_eq!(out, expected);
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
