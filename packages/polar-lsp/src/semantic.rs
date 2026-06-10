//! Semantic diagnostics routed through the `polar-compiler` query engine
//! (`planning/lsp.md` M2). The server holds one in-memory [`RootDatabase`] +
//! [`Vfs`] (the incremental engine the doc's "sharing work" section calls for);
//! editor buffers are overlaid into the VFS and the per-def diagnostic queries
//! are run.
//!
//! Each diagnostic carries a precise source span (Q6): per-def diagnostics
//! (`body`/`infer`/`check_drivers`/`directions`) hold a **def-relative** [`Span`]
//! (offset from the owning def's start, so it survives edits to other defs),
//! and `DefDiagnostic` an item anchor. We turn both into absolute byte ranges
//! (def start via `ast_id_map`) and map to LSP ranges through `encoding`.

use std::path::{Path, PathBuf};

use polar_compiler::{
    DefKind, RootDatabase, Span, Vfs, ast_id_map, body, check_drivers, crate_def_map, directions,
    infer, sig_of,
};
use ropey::Rope;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, Range, Uri};

use crate::encoding::{Encoding, byte_to_position};

/// The server's single incremental analysis engine: a salsa database plus the
/// VFS overlay of editor buffers. Held behind a `Mutex` in the backend.
pub struct Analysis {
    pub(crate) db: RootDatabase,
    pub(crate) vfs: Vfs,
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
/// the file at `path` (only diagnostics whose source is *this* file).
pub fn diagnostics(
    analysis: &mut Analysis,
    path: &Path,
    rope: &Rope,
    enc: Encoding,
) -> Vec<Diagnostic> {
    let src = rope.to_string();
    analysis.vfs.set_file_text(&mut analysis.db, path, src);
    let krate = analysis.vfs.source_root(&mut analysis.db, path);

    let cur_file = analysis.vfs.file(path);
    let db = &analysis.db;
    let map = crate_def_map(db, krate);
    let mut out = Vec::new();

    // Crate-level (name resolution / imports / duplicate defs), anchored at the
    // offending item. Only surface those anchored in this file.
    for d in map.diagnostics() {
        match d.anchor {
            Some((file, ast_id)) if Some(file) == cur_file => {
                let range = ast_id_map(db, file).range_of(ast_id);
                out.push(make_diag(rope, enc, range, d.message()));
            }
            _ => {}
        }
    }

    // Per-def: body lowering, inference, driver + direction checks.
    for def in map.defs().collect::<Vec<_>>() {
        if cur_file.is_some() && Some(def.file(db)) != cur_file {
            continue; // a def in another file of the crate — not our diagnostics.
        }
        if !matches!(
            map.def_data(def).map(|d| d.kind),
            Some(DefKind::Fn | DefKind::Method)
        ) {
            continue;
        }
        let Some((def_start, _)) = ast_id_map(db, def.file(db)).range_of(def.ast_id(db)) else {
            continue;
        };
        // def-relative span -> absolute byte range.
        let abs = |span: Span| {
            (
                def_start + span.start as usize,
                def_start + span.end as usize,
            )
        };

        for d in &sig_of(db, krate, def).diagnostics {
            out.push(make_diag(rope, enc, Some(abs(d.span)), d.message()));
        }
        for d in body(db, krate, def).diagnostics() {
            out.push(make_diag(rope, enc, Some(abs(d.span)), d.message()));
        }
        for d in infer(db, krate, def).diagnostics() {
            out.push(make_diag(rope, enc, Some(abs(d.span)), d.message()));
        }
        for d in check_drivers(db, krate, def) {
            out.push(make_diag(rope, enc, Some(abs(d.span)), d.message()));
        }
        for d in directions(db, krate, def) {
            out.push(make_diag(rope, enc, Some(abs(d.span)), d.message()));
        }
    }
    out
}

/// Build an error diagnostic at an absolute byte range (or the file start if
/// none / unresolved).
fn make_diag(
    rope: &Rope,
    enc: Encoding,
    range: Option<(usize, usize)>,
    message: String,
) -> Diagnostic {
    let range = match range {
        Some((s, e)) => Range {
            start: byte_to_position(rope, s, enc),
            end: byte_to_position(rope, e, enc),
        },
        None => {
            let z = byte_to_position(rope, 0, enc);
            Range { start: z, end: z }
        }
    };
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("polar-lsp".to_owned()),
        message,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn run(src: &str) -> Vec<Diagnostic> {
        let mut a = Analysis::new();
        let doc = Document::open(src);
        diagnostics(&mut a, Path::new("/t.plr"), &doc.rope, Encoding::Utf8)
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
    fn unresolved_name_lands_exactly_on_the_span() {
        // `missing` is never bound; line 5 is "    a + missing".
        let src = "fn f\n  { dom clk: Clock }\n  ( a: uint(8) @clk )\n  \
            -> uint(8) @clk\n  {\n    a + missing\n  }\n";
        let diags = run(src);
        let d = diags
            .iter()
            .find(|d| d.message.contains("missing"))
            .unwrap_or_else(|| panic!("no unresolved-name diagnostic: {diags:?}"));
        // Span-accurate: starts at `missing` (col 8) and ends after it (col 15),
        // not a whole-def or best-effort identifier match.
        assert_eq!(
            d.range.start,
            tower_lsp_server::ls_types::Position::new(5, 8)
        );
        assert_eq!(
            d.range.end,
            tower_lsp_server::ls_types::Position::new(5, 15)
        );
    }
}
