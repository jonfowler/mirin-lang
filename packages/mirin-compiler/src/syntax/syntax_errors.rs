//! `syntax_errors(file)` — the parse-error diagnostics.
//!
//! Tree-sitter is infallible (it always returns a tree, recovering with
//! `ERROR`/`MISSING` nodes), so malformed input would otherwise flow silently
//! into lowering. This query walks the tree for those nodes and reports each
//! with a [`Span`], so the CLI/LSP reject bad input instead of emitting partial
//! SystemVerilog. Keyed per file (a sibling of [`item_tree`](super::item_tree)).

use tree_sitter::Node;

use crate::base::db::SourceFile;
use crate::base::diagnostics::Span;
use crate::base::parser::parse_text;

/// A single syntax error: where it is and what was wrong.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SyntaxError {
    pub span: Span,
    pub message: String,
}

/// QUERY: the syntax errors in a file's parse tree, in source order.
#[salsa::tracked(returns(ref))]
pub fn syntax_errors(db: &dyn salsa::Database, file: SourceFile) -> Vec<SyntaxError> {
    let source = file.text(db);
    let tree = parse_text(source);
    let mut out = Vec::new();
    collect(tree.root_node(), &mut out);
    out
}

/// Walk the tree, recording each `ERROR`/`MISSING` node. An `ERROR` node's
/// subtree is not descended into (one diagnostic per recovery point).
fn collect(node: Node, out: &mut Vec<SyntaxError>) {
    if node.is_missing() {
        out.push(SyntaxError {
            span: Span::new(
                node.start_byte(),
                node.end_byte().max(node.start_byte() + 1),
            ),
            message: format!("syntax error: missing `{}`", node.kind()),
        });
        return;
    }
    if node.is_error() {
        out.push(SyntaxError {
            span: Span::new(node.start_byte(), node.end_byte()),
            message: "syntax error: unexpected input".to_owned(),
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() || child.is_missing() {
            collect(child, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;

    fn errors(src: &str) -> Vec<SyntaxError> {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let file = vfs.set_file_text(&mut db, "t.mrn", src.to_owned());
        syntax_errors(&db, file).clone()
    }

    #[test]
    fn clean_source_has_no_syntax_errors() {
        assert!(errors("fn f (a: uint(8)) -> uint(8) { return a; }").is_empty());
    }

    #[test]
    fn a_missing_semicolon_is_reported() {
        let es = errors("fn f (a: uint(8)) -> uint(8) {\n  let b = a\n  return b;\n}");
        assert!(!es.is_empty(), "expected a syntax error");
        assert!(es.iter().any(|e| e.message.contains("missing")), "{es:?}");
    }

    #[test]
    fn malformed_input_is_reported_not_silently_lowered() {
        // A struct field missing its `:` — the old silent-acceptance gap.
        let es = errors("struct S = s {\n  valid bool,\n}");
        assert!(!es.is_empty(), "{es:?}");
    }
}
