//! Semantic diagnostics routed through the `polar-db` query engine
//! (`planning/lsp.md` M2). The server holds one in-memory [`RootDatabase`] +
//! [`Vfs`] (the incremental engine the doc's "sharing work" section calls for);
//! editor buffers are overlaid into the VFS and the per-def diagnostic queries
//! are run.
//!
//! `polar-db` diagnostics are message-only — they carry no source span (spans
//! are a later "Q6" concern). So each diagnostic is *located* best-effort: it
//! names the offending identifier, which we find in the owning def's CST range
//! (the def range comes from `ast_id_map`). Span-less diagnostics fall back to
//! the def's name. Go-to-def/hover wait on real source maps in `polar-db`.

use std::path::{Path, PathBuf};

use polar_compiler::{
    BodyDiagnostic, DefDiagnostic, DefKind, DirectionDiagnostic, DirectionDiagnosticKind,
    DriverDiagnostic, DriverDiagnosticKind, InferDiagnostic, RootDatabase, Vfs, ast_id_map, body,
    check_drivers, crate_def_map, directions, infer,
};
use ropey::Rope;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, Range, Uri};
use tree_sitter::{Node, Tree};

use crate::encoding::{Encoding, byte_to_position, node_range};

/// The server's single incremental analysis engine: a salsa database plus the
/// VFS overlay of editor buffers. Held behind a `Mutex` in the backend.
pub struct Analysis {
    db: RootDatabase,
    vfs: Vfs,
}

impl Analysis {
    pub fn new() -> Self {
        Self {
            db: RootDatabase::default(),
            vfs: Vfs::new(),
        }
    }
}

/// A `file://` URI to a filesystem path used as the VFS key. Best-effort: the
/// key only needs to be stable per document (module resolution across files is
/// future work), so percent-encoding is left as-is.
pub fn uri_to_path(uri: &Uri) -> PathBuf {
    let s = uri.as_str();
    PathBuf::from(s.strip_prefix("file://").unwrap_or(s))
}

/// Overlay `rope`'s text into the VFS and return the semantic diagnostics for
/// that file, located against `tree`.
pub fn diagnostics(
    analysis: &mut Analysis,
    path: &Path,
    rope: &Rope,
    tree: &Tree,
    enc: Encoding,
) -> Vec<Diagnostic> {
    let src = rope.to_string();
    analysis
        .vfs
        .set_file_text(&mut analysis.db, path, src.clone());
    let krate = analysis.vfs.source_root(&mut analysis.db, path);

    let db = &analysis.db;
    let map = crate_def_map(db, krate);
    let mut out = Vec::new();

    // Crate-level (name resolution / imports / duplicate defs).
    for d in map.diagnostics() {
        let (message, name) = def_message(d);
        out.push(locate(
            tree,
            &src,
            rope,
            enc,
            None,
            name.as_deref(),
            message,
        ));
    }

    // Per-def: body lowering, inference, driver + direction checks.
    for def in map.defs().collect::<Vec<_>>() {
        if !matches!(
            map.def_data(def).map(|d| d.kind),
            Some(DefKind::Fn | DefKind::Method)
        ) {
            continue;
        }
        let def_range = ast_id_map(db, def.file(db)).range_of(def.ast_id(db));
        let mut push = |message: String, name: Option<String>| {
            out.push(locate(
                tree,
                &src,
                rope,
                enc,
                def_range,
                name.as_deref(),
                message,
            ));
        };
        for d in body(db, krate, def).diagnostics() {
            let (m, n) = body_message(d);
            push(m, n);
        }
        for d in infer(db, krate, def).diagnostics() {
            let (m, n) = infer_message(d);
            push(m, n);
        }
        for d in check_drivers(db, krate, def) {
            let (m, n) = driver_message(&d);
            push(m, n);
        }
        for d in directions(db, krate, def) {
            let (m, n) = direction_message(&d);
            push(m, n);
        }
    }
    out
}

/// Build a diagnostic, choosing the tightest range we can: the named
/// identifier within `def_range` if given; else the def's name; else the def
/// start.
fn locate(
    tree: &Tree,
    src: &str,
    rope: &Rope,
    enc: Encoding,
    def_range: Option<(usize, usize)>,
    name: Option<&str>,
    message: String,
) -> Diagnostic {
    let scope = match def_range {
        Some((s, e)) => tree
            .root_node()
            .descendant_for_byte_range(s, e)
            .unwrap_or_else(|| tree.root_node()),
        None => tree.root_node(),
    };
    let range = name
        .and_then(|n| find_identifier(scope, src.as_bytes(), n))
        .or_else(|| scope.child_by_field_name("name"))
        .map(|node| node_range(rope, node, enc))
        .unwrap_or_else(|| {
            let start = byte_to_position(rope, scope.start_byte(), enc);
            Range { start, end: start }
        });
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("polar-lsp".to_owned()),
        message,
        ..Default::default()
    }
}

/// First `identifier` node in `node`'s subtree whose text equals `name`.
fn find_identifier<'a>(node: Node<'a>, src: &[u8], name: &str) -> Option<Node<'a>> {
    if node.kind() == "identifier" && node.utf8_text(src).ok() == Some(name) {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_identifier(child, src, name) {
            return Some(found);
        }
    }
    None
}

// ----- message + offending-name extraction per diagnostic kind -----

fn def_message(d: &DefDiagnostic) -> (String, Option<String>) {
    match d {
        DefDiagnostic::UnresolvedModule { name } => {
            (format!("unresolved module `{name}`"), Some(name.clone()))
        }
        DefDiagnostic::UnresolvedImport { path } => (
            format!("unresolved import `{}`", path.join("::")),
            path.last().cloned(),
        ),
        DefDiagnostic::PrivateImport { name } => {
            (format!("`{name}` is private"), Some(name.clone()))
        }
        DefDiagnostic::DuplicateDef { name } => (
            format!("the name `{name}` is defined multiple times"),
            Some(name.clone()),
        ),
        DefDiagnostic::UnresolvedImplOwner { name } => (
            format!("cannot find type `{name}` for this impl"),
            Some(name.clone()),
        ),
    }
}

fn body_message(d: &BodyDiagnostic) -> (String, Option<String>) {
    use polar_compiler::BodyDiagnosticKind as K;
    match &d.kind {
        K::UnresolvedName { name } => (
            format!("cannot find `{name}` in this scope"),
            Some(name.clone()),
        ),
        K::DuplicateVar { name } => (
            format!("`{name}` is declared more than once as `var` in this block"),
            Some(name.clone()),
        ),
        K::VarAfterLet { name } => (
            format!("cannot declare `var {name}` after a `let {name}` binding in the same block"),
            Some(name.clone()),
        ),
        K::Unsupported { what } => (format!("unsupported: {what}"), None),
    }
}

fn infer_message(d: &InferDiagnostic) -> (String, Option<String>) {
    match d {
        InferDiagnostic::TypeMismatch => ("type mismatch".to_owned(), None),
        InferDiagnostic::WidthMismatch => ("width mismatch".to_owned(), None),
        InferDiagnostic::DomainMismatch => ("clock-domain mismatch".to_owned(), None),
        InferDiagnostic::UnresolvedMethod { name } => {
            (format!("no method `{name}`"), Some(name.clone()))
        }
    }
}

fn driver_message(d: &DriverDiagnostic) -> (String, Option<String>) {
    match &d.kind {
        DriverDiagnosticKind::Undriven { name } => {
            (format!("`{name}` is never driven"), Some(name.clone()))
        }
        DriverDiagnosticKind::MultipleDrivers { name } => (
            format!("`{name}` is driven more than once"),
            Some(name.clone()),
        ),
    }
}

fn direction_message(d: &DirectionDiagnostic) -> (String, Option<String>) {
    match &d.kind {
        DirectionDiagnosticKind::ValueToOut { param } => (
            format!("`{param}`: a value is connected to an `out` parameter"),
            Some(param.clone()),
        ),
        DirectionDiagnosticKind::OutToNonOut { param } => (
            format!("`{param}`: an `out` parameter is connected to a non-output target"),
            Some(param.clone()),
        ),
        DirectionDiagnosticKind::UnknownNamedArg { callee, name } => (
            format!("`{callee}` has no named parameter `{name}`"),
            Some(name.clone()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn run(src: &str) -> Vec<Diagnostic> {
        let mut a = Analysis::new();
        let doc = Document::open(src);
        diagnostics(
            &mut a,
            Path::new("/t.plr"),
            &doc.rope,
            &doc.tree,
            Encoding::Utf8,
        )
    }

    #[test]
    fn clean_example_has_no_semantic_diagnostics() {
        // Valid counter.plr — should type-check clean.
        let src = "fn counter\n  { dom clk: Clock, rstn: Reset @clk = high }\n  \
            ( param bits: usize )\n  -> uint(bits) @clk\n  {\n    \
            var count: uint(bits) @clk;\n    count = (count + 1).reg(rstn, 0);\n    \
            return count;\n  }\n";
        let diags = run(src);
        assert!(diags.is_empty(), "clean source flagged: {diags:?}");
    }

    #[test]
    fn unresolved_name_is_reported_and_located() {
        // `missing` is never bound.
        let src = "fn f\n  { dom clk: Clock }\n  ( a: uint(8) @clk )\n  \
            -> uint(8) @clk\n  {\n    a + missing\n  }\n";
        let diags = run(src);
        assert!(
            diags.iter().any(|d| d.message.contains("missing")),
            "expected an unresolved-name diagnostic, got {diags:?}"
        );
        // Located on the `missing` identifier (line 5), not at file start.
        let d = diags
            .iter()
            .find(|d| d.message.contains("missing"))
            .unwrap();
        assert_eq!(d.range.start.line, 5, "diag not on the right line: {d:?}");
    }
}
