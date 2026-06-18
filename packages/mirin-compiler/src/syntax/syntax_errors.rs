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

/// QUERY: the syntax errors in a file's parse tree, in source order. Includes
/// reserved-word violations (a keyword used as a binding name) — in Rust these
/// are lexer-level errors, so they belong with the other parse diagnostics and
/// gate emission the same way.
#[salsa::tracked(returns(ref))]
pub fn syntax_errors(db: &dyn salsa::Database, file: SourceFile) -> Vec<SyntaxError> {
    let source = file.text(db);
    let tree = parse_text(source);
    let mut out = Vec::new();
    collect(tree.root_node(), &mut out);
    collect_reserved(tree.root_node(), source, &mut out);
    out
}

/// The binding-name field(s) to check on each binding-introducing node kind. A
/// reserved word (`out`, `in`, `dom`, …) in any of these positions is rejected;
/// the same words used as keywords (`for…in`, port `out`, the method receiver
/// `self`, `crate::`/`self.` paths) are not binding names and never reach here.
fn binding_name_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "function_definition"
        | "trait_definition"
        | "module_definition"
        | "trait_const"
        | "parameter"
        | "named_parameter"
        | "named_result"
        | "record_field_type"
        | "port_field" => &["name"],
        "struct_definition" | "port_definition" => &["name", "constructor"],
        _ => &[],
    }
}

/// Walk the whole tree, flagging any reserved word used as a user binding name.
/// Only `identifier`-kind nodes are checked, so the literal `self` receiver
/// (`field("name", "self")` in the grammar) is exempt while `let self` is not.
fn collect_reserved<'t>(node: Node<'t>, source: &str, out: &mut Vec<SyntaxError>) {
    let mut check = |n: Node<'t>| {
        if n.kind() == "identifier" {
            let name = &source[n.byte_range()];
            if crate::nameres::ids::is_reserved_word(name) {
                out.push(SyntaxError {
                    span: Span::new(n.start_byte(), n.end_byte()),
                    message: format!(
                        "reserved word `{name}` cannot be used as a name — rename the binding"
                    ),
                });
            }
        }
    };
    for field in binding_name_fields(node.kind()) {
        if let Some(n) = node.child_by_field_name(field) {
            check(n);
        }
    }
    // `var x, y: T` binds several names; `let`/`for` bind a pattern whose leaf
    // identifiers are the binders (a struct pattern's field NAMES are not).
    match node.kind() {
        "var_statement" => {
            let mut cursor = node.walk();
            for n in node.children_by_field_name("name", &mut cursor) {
                check(n);
            }
        }
        "let_statement" | "for_statement" => {
            if let Some(pat) = node.child_by_field_name("pattern") {
                collect_pattern_binders(pat, &mut check);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_reserved(child, source, out);
    }
}

/// Visit the binder identifiers of a pattern: a bare identifier, every element
/// of a tuple pattern, and the value side of each struct-pattern field.
fn collect_pattern_binders<'t>(pat: Node<'t>, check: &mut impl FnMut(Node<'t>)) {
    match pat.kind() {
        "identifier" => check(pat),
        "tuple_pattern" => {
            let mut cursor = pat.walk();
            for c in pat.children(&mut cursor) {
                collect_pattern_binders(c, check);
            }
        }
        "struct_pattern" => {
            let mut cursor = pat.walk();
            for f in pat.children(&mut cursor) {
                if f.kind() == "struct_pattern_field"
                    && let Some(v) = f.child_by_field_name("binding")
                {
                    collect_pattern_binders(v, check);
                }
            }
        }
        _ => {}
    }
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

    #[test]
    fn reserved_word_as_a_binding_name_is_rejected() {
        // `out` as a local — a reserved keyword, not a name.
        let es = errors("fn f (a: uint(8)) -> uint(8) {\n  let out = a;\n  out\n}");
        assert!(
            es.iter().any(|e| e.message.contains("reserved word `out`")),
            "{es:?}"
        );
        // Result name and param name too.
        assert!(
            errors("fn f (a: uint(8)) -> (in: uint(8)) { in = a; }")
                .iter()
                .any(|e| e.message.contains("reserved word `in`"))
        );
    }

    #[test]
    fn keyword_uses_and_shadowable_builtins_are_clean() {
        // Port directions, `for…in`, the `self` receiver, and `crate::` paths
        // are keyword uses, not binding names. Builtin type names shadow.
        let ok = [
            "port S = s {\n  in ready: bool,\n  out valid: bool,\n}",
            "fn f {dom clk: Clock} (v: Vec(2, uint(8)) @clk) -> uint(8) @clk {\n  var a: uint(8) @clk;\n  for x in v { a = x; }\n  a\n}",
            "fn f (x: uint(8)) -> uint(8) { let Clock = x; Clock }",
        ];
        for src in ok {
            assert!(
                !errors(src).iter().any(|e| e.message.contains("reserved")),
                "unexpected reserved-word error in: {src}\n{:?}",
                errors(src)
            );
        }
    }
}
