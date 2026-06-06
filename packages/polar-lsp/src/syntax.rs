//! Tree-only syntactic features (`planning/lsp.md` M1): document symbols /
//! outline, folding ranges, and selection ranges. All walk the tree-sitter tree
//! directly — no compiler analysis. Node kinds/fields track the grammar
//! (`tree-sitter-polar`'s `node-types.json`).

use ropey::Rope;
use tower_lsp_server::ls_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbol, FoldingRange, FoldingRangeKind, Position,
    SelectionRange, SymbolKind,
};
use tree_sitter::{Node, Tree};

use crate::encoding::{Encoding, node_range, position_to_byte};

/// The text of a node, or `""` if it is not valid UTF-8 (it always is here).
fn text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or_default()
}

// ----- Document symbols (outline) -----

pub fn document_symbols(rope: &Rope, tree: &Tree, enc: Encoding) -> Vec<DocumentSymbol> {
    let src = rope.to_string();
    let mut cursor = tree.root_node().walk();
    tree.root_node()
        .named_children(&mut cursor)
        .filter_map(|n| symbol(n, rope, &src, enc))
        .collect()
}

fn symbol(node: Node, rope: &Rope, src: &str, enc: Encoding) -> Option<DocumentSymbol> {
    let (kind, children) = match node.kind() {
        "function_definition" => (SymbolKind::FUNCTION, params(node, rope, src, enc)),
        "struct_definition" => (
            SymbolKind::STRUCT,
            body_members(node, "record_field_type", SymbolKind::FIELD, rope, src, enc),
        ),
        "port_definition" => (
            SymbolKind::INTERFACE,
            body_members(node, "port_field", SymbolKind::FIELD, rope, src, enc),
        ),
        "module_definition" => (SymbolKind::MODULE, module_children(node, rope, src, enc)),
        "impl_block" => (SymbolKind::NAMESPACE, impl_methods(node, rope, src, enc)),
        _ => return None,
    };
    let name_node = node.child_by_field_name("name").unwrap_or(node);
    Some(make_symbol(
        text(name_node, src).to_owned(),
        None,
        kind,
        node_range(rope, node, enc),
        node_range(rope, name_node, enc),
        children,
    ))
}

/// Value/named params of a fn, as leaf symbols (with their type as detail).
fn params(node: Node, rope: &Rope, src: &str, enc: Encoding) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for field in ["parameters", "named_parameters"] {
        let Some(section) = node.child_by_field_name(field) else {
            continue;
        };
        let mut cursor = section.walk();
        for p in section.named_children(&mut cursor) {
            if let Some(sym) = leaf(p, SymbolKind::VARIABLE, rope, src, enc) {
                out.push(sym);
            }
        }
    }
    out
}

/// Members declared in a definition's body (`record_field_type`/`port_field`).
fn body_members(
    node: Node,
    member_kind: &str,
    sym_kind: SymbolKind,
    rope: &Rope,
    src: &str,
    enc: Encoding,
) -> Vec<DocumentSymbol> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter(|c| c.kind() == member_kind)
        .filter_map(|c| leaf(c, sym_kind, rope, src, enc))
        .collect()
}

fn module_children(node: Node, rope: &Rope, src: &str, enc: Encoding) -> Vec<DocumentSymbol> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter_map(|c| symbol(c, rope, src, enc))
        .collect()
}

fn impl_methods(node: Node, rope: &Rope, src: &str, enc: Encoding) -> Vec<DocumentSymbol> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter(|c| c.kind() == "function_definition")
        .filter_map(|c| symbol(c, rope, src, enc))
        .collect()
}

/// A leaf symbol (param/field): name from the `name` field, type as detail.
fn leaf(
    node: Node,
    kind: SymbolKind,
    rope: &Rope,
    src: &str,
    enc: Encoding,
) -> Option<DocumentSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let detail = node
        .child_by_field_name("type")
        .map(|t| text(t, src).to_owned());
    Some(make_symbol(
        text(name_node, src).to_owned(),
        detail,
        kind,
        node_range(rope, node, enc),
        node_range(rope, name_node, enc),
        Vec::new(),
    ))
}

#[allow(deprecated)] // the `deprecated` field is required but itself deprecated
fn make_symbol(
    name: String,
    detail: Option<String>,
    kind: SymbolKind,
    range: tower_lsp_server::ls_types::Range,
    selection_range: tower_lsp_server::ls_types::Range,
    children: Vec<DocumentSymbol>,
) -> DocumentSymbol {
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    }
}

// ----- Folding ranges -----

/// Node kinds whose multi-line spans collapse.
const FOLDABLE: &[&str] = &[
    "block",
    "module_body",
    "impl_body",
    "record_type_body",
    "port_body",
    "parameter_section",
    "named_parameter_section",
    "comment",
];

pub fn folding_ranges(tree: &Tree) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    collect_folds(tree.root_node(), &mut out);
    out
}

fn collect_folds(node: Node, out: &mut Vec<FoldingRange>) {
    let (start, end) = (node.start_position().row, node.end_position().row);
    if end > start && FOLDABLE.contains(&node.kind()) {
        let kind = (node.kind() == "comment").then_some(FoldingRangeKind::Comment);
        out.push(FoldingRange {
            start_line: start as u32,
            end_line: end as u32,
            kind,
            ..Default::default()
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_folds(child, out);
    }
}

// ----- Syntactic diagnostics -----

/// ERROR and MISSING nodes as diagnostics. Coarse by design (`planning/lsp.md`
/// M1): a real error-recovery pass with messages arrives in M2 via the compiler.
/// MISSING nodes are zero-width and unqueryable, so this is a manual traversal.
pub fn diagnostics(rope: &Rope, tree: &Tree, enc: Encoding) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    collect_diagnostics(tree.root_node(), rope, enc, &mut out);
    out
}

fn collect_diagnostics(node: Node, rope: &Rope, enc: Encoding, out: &mut Vec<Diagnostic>) {
    let message = if node.is_missing() {
        Some(format!("missing `{}`", node.kind()))
    } else if node.is_error() {
        Some("syntax error".to_owned())
    } else {
        None
    };
    if let Some(message) = message {
        out.push(Diagnostic {
            range: node_range(rope, node, enc),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("polar-lsp".to_owned()),
            message,
            ..Default::default()
        });
    }
    // Descend only where an error lives (prunes the clean majority of the tree).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() || child.is_missing() {
            collect_diagnostics(child, rope, enc, out);
        }
    }
}

// ----- Selection ranges -----

pub fn selection_ranges(
    rope: &Rope,
    tree: &Tree,
    positions: &[Position],
    enc: Encoding,
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|&pos| selection_range_at(rope, tree, pos, enc))
        .collect()
}

fn selection_range_at(rope: &Rope, tree: &Tree, pos: Position, enc: Encoding) -> SelectionRange {
    let byte = position_to_byte(rope, pos, enc);
    let root = tree.root_node();
    let leaf = root.descendant_for_byte_range(byte, byte).unwrap_or(root);

    // Walk innermost → root, then nest so each `.parent` is the larger range.
    let mut chain = vec![leaf];
    while let Some(p) = chain.last().unwrap().parent() {
        chain.push(p);
    }
    let mut sr: Option<SelectionRange> = None;
    for node in chain.iter().rev() {
        sr = Some(SelectionRange {
            range: node_range(rope, *node, enc),
            parent: sr.map(Box::new),
        });
    }
    sr.expect("chain always contains at least the leaf")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    const SRC: &str = "fn addConstant\n  { dom clk: Clock }\n  \
        ( value: uint(8) @clk )\n  -> uint(8) @clk\n  {\n    \
        let bumped = value + 3;\n    bumped\n  }\n";

    #[test]
    fn outline_lists_the_function_and_its_params() {
        let doc = Document::open(SRC);
        let syms = document_symbols(&doc.rope, &doc.tree, Encoding::Utf8);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "addConstant");
        assert_eq!(syms[0].kind, SymbolKind::FUNCTION);
        // `clk` (named) and `value` (positional) params show up as children.
        let children = syms[0].children.as_ref().expect("params");
        let names: Vec<_> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"clk"), "params: {names:?}");
        assert!(names.contains(&"value"), "params: {names:?}");
    }

    #[test]
    fn folding_covers_the_body_block() {
        let doc = Document::open(SRC);
        let folds = folding_ranges(&doc.tree);
        // The `{ ... }` body spans lines 4..7.
        assert!(
            folds.iter().any(|f| f.start_line < f.end_line),
            "expected a multi-line fold, got {folds:?}"
        );
    }

    #[test]
    fn clean_source_has_no_diagnostics() {
        let doc = Document::open(SRC);
        let diags = diagnostics(&doc.rope, &doc.tree, Encoding::Utf8);
        assert!(diags.is_empty(), "clean source flagged: {diags:?}");
    }

    #[test]
    fn malformed_source_is_flagged() {
        let doc = Document::open(")(}{ fn\n");
        let diags = diagnostics(&doc.rope, &doc.tree, Encoding::Utf8);
        assert!(!diags.is_empty(), "expected a syntax diagnostic");
        assert!(
            diags
                .iter()
                .all(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        );
    }

    #[test]
    fn selection_range_nests_from_identifier_outward() {
        let doc = Document::open(SRC);
        // Position inside `bumped` on line 5.
        let pos = Position::new(5, 9);
        let srs = selection_ranges(&doc.rope, &doc.tree, &[pos], Encoding::Utf8);
        assert_eq!(srs.len(), 1);
        // Each parent must strictly contain its child.
        let mut cur = Some(&srs[0]);
        let mut prev: Option<&SelectionRange> = None;
        while let Some(sr) = cur {
            if let Some(p) = prev {
                assert!(sr.range.start <= p.range.start && sr.range.end >= p.range.end);
            }
            prev = Some(sr);
            cur = sr.parent.as_deref();
        }
    }
}
