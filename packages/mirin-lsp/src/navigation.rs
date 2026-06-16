//! Go-to-definition (`planning/lsp.md` M2). Rides the **resolved HIR**: the
//! cursor maps to an `ExprId`, whose `ExprKind` already names the target — a
//! resolved local (`Local`) or a resolved item (`Def` / record `ctor`). Name
//! resolution and `let`/`var` shadowing are therefore the compiler's, never
//! re-implemented here (which the design docs forbid).
//!
//! v1 resolves targets in the *same* file (locals, same-file fns/constructors)
//! and returns the target [`Range`]; the server attaches the request URI.
//! Cross-file targets (imported items) are deferred.

use std::path::Path;

use mirin_compiler::{
    DefId, DefKind, ExprKind, RootDatabase, SourceFile, ast_id_map, body, crate_def_map, parse_text,
};
use ropey::Rope;
use tower_lsp_server::ls_types::{Position, Range};

use crate::encoding::{Encoding, byte_to_position, position_to_byte};
use crate::semantic::Analysis;

/// The definition range for the entity under `position`, in the current file,
/// or `None` if there's nothing to jump to (or the target is in another file).
pub fn definition_range(
    analysis: &mut Analysis,
    path: &Path,
    rope: &Rope,
    position: Position,
    enc: Encoding,
) -> Option<Range> {
    let offset = position_to_byte(rope, position, enc);
    let src = rope.to_string();
    analysis
        .vfs
        .set_file_text(&mut analysis.db, path, src.clone());
    let krate = analysis.vfs.source_root(&mut analysis.db, path);
    let cur_file = analysis.vfs.file(path)?;
    let db = &analysis.db;
    let map = crate_def_map(db, krate);

    // The Fn/Method def in this file whose source range contains the cursor.
    let (def, def_start) = map.defs().collect::<Vec<_>>().into_iter().find_map(|d| {
        if d.file(db) != cur_file
            || !matches!(
                map.def_data(d).map(|x| x.kind),
                Some(DefKind::Fn | DefKind::Method)
            )
        {
            return None;
        }
        let (s, e) = ast_id_map(db, cur_file).range_of(d.ast_id(db))?;
        (s <= offset && offset < e).then_some((d, s))
    })?;

    let b = body(db, krate, def);
    let eid = b.expr_at((offset - def_start) as u32)?;
    match &b.expr(eid).kind {
        // A resolved local: jump to its declaration (def-relative → absolute).
        // `let`/`var` carry a real declaration span; params don't yet (their
        // span is degenerate), so we don't offer a misleading jump for those.
        ExprKind::Local(local) => {
            let ld = b.local(*local);
            // A result place (`return`, or a named result/tuple part) has no
            // body declaration site — jump into the signature instead: a named
            // result to its name, an unnamed `return` to the return type
            // (planning/return_variable.md).
            if ld.result_base.is_some() {
                return result_place_range(db, def, cur_file, &ld.name, &src, rope, enc);
            }
            let span = b.local_span(*local);
            (span.start != span.end).then(|| {
                range_of(
                    rope,
                    def_start + span.start as usize,
                    def_start + span.end as usize,
                    enc,
                )
            })
        }
        // A resolved item (called fn / builtin) or a record constructor.
        ExprKind::Def(d) => def_range(db, *d, cur_file, &src, rope, enc),
        ExprKind::Record { ctor: Some(d), .. } => def_range(db, *d, cur_file, &src, rope, enc),
        _ => None,
    }
}

/// The (same-file) definition range of an item, refined to its name identifier.
fn def_range<'db>(
    db: &'db RootDatabase,
    target: DefId<'db>,
    cur_file: SourceFile,
    src: &str,
    rope: &Rope,
    enc: Encoding,
) -> Option<Range> {
    if target.file(db) != cur_file {
        return None; // cross-file goto deferred (needs target-file URI).
    }
    let (s, e) = ast_id_map(db, cur_file).range_of(target.ast_id(db))?;
    let (ns, ne) = name_range(src, s, e).unwrap_or((s, e));
    Some(range_of(rope, ns, ne, enc))
}

/// The signature declaration of a result place. A named result jumps to its
/// `name` in the `-> (name: T, …)` list; an unnamed `return` jumps to the
/// return type (the closest declaration of the result's shape). Rides the CST
/// directly — result places have no body span (planning/return_variable.md).
fn result_place_range<'db>(
    db: &'db RootDatabase,
    def: DefId<'db>,
    cur_file: SourceFile,
    local_name: &str,
    src: &str,
    rope: &Rope,
    enc: Encoding,
) -> Option<Range> {
    let (s, e) = ast_id_map(db, cur_file).range_of(def.ast_id(db))?;
    let tree = parse_text(src);
    let fn_node = tree.root_node().descendant_for_byte_range(s, e)?;
    let rt = fn_node.child_by_field_name("return_type")?;
    if rt.kind() == "named_return" {
        // The matching named result's `name` identifier.
        let mut cursor = rt.walk();
        for nr in rt
            .children(&mut cursor)
            .filter(|c| c.kind() == "named_result")
        {
            if let Some(name) = nr.child_by_field_name("name")
                && src.get(name.start_byte()..name.end_byte()) == Some(local_name)
            {
                return Some(range_of(rope, name.start_byte(), name.end_byte(), enc));
            }
        }
    }
    // An unnamed `return` (or a name not found): the return type itself.
    Some(range_of(rope, rt.start_byte(), rt.end_byte(), enc))
}

/// The byte range of a definition's `name` identifier, given the def's range.
fn name_range(src: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let tree = parse_text(src);
    let node = tree.root_node().descendant_for_byte_range(start, end)?;
    let name = node.child_by_field_name("name")?;
    Some((name.start_byte(), name.end_byte()))
}

fn range_of(rope: &Rope, start: usize, end: usize, enc: Encoding) -> Range {
    Range {
        start: byte_to_position(rope, start, enc),
        end: byte_to_position(rope, end, enc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn goto(src: &str, pos: Position) -> Option<Range> {
        let mut a = Analysis::new();
        let doc = Document::open(src);
        definition_range(&mut a, Path::new("/t.mrn"), &doc.rope, pos, Encoding::Utf8)
    }

    // line 5 = "    let x = a;", line 6 = "    x".
    const LET_SRC: &str = "fn f\n  { dom clk: Clock }\n  ( a: uint(8) @clk )\n  \
        -> uint(8) @clk\n  {\n    let x = a;\n    x\n  }\n";

    #[test]
    fn goto_local_jumps_to_its_let_declaration() {
        // Cursor on the tail `x` (line 6) → the `let x` declaration (line 5).
        let r = goto(LET_SRC, Position::new(6, 4)).expect("goto on `x`");
        assert_eq!(r.start.line, 5, "expected jump to `let x` on line 5: {r:?}");
    }

    #[test]
    fn goto_param_jumps_to_its_declaration() {
        // Cursor on `a` in `let x = a;` (line 5, col 12) → the param `a`
        // declaration in the signature (line 2, col 4).
        let r = goto(LET_SRC, Position::new(5, 12)).expect("goto on param `a`");
        assert_eq!(
            r.start,
            Position::new(2, 4),
            "expected param `a` at (2,4): {r:?}"
        );
    }

    #[test]
    fn goto_local_lands_on_just_the_name() {
        // The jump to `let x` is the `x` identifier (line 5, col 8), not the
        // whole `let x = a;` statement.
        let r = goto(LET_SRC, Position::new(6, 4)).expect("goto on `x`");
        assert_eq!(r.start, Position::new(5, 8));
        assert_eq!(r.end, Position::new(5, 9));
    }

    // line 2 = "    return.a = p.a;"; the return type `Pair` is on line 1.
    const RETURN_SRC: &str = "struct Pair = pair { a: uint(8), b: uint(8) }\nfn f(p: Pair) -> Pair {\n    return.a = p.a;\n}\n";

    #[test]
    fn goto_return_jumps_to_the_return_type() {
        // Cursor on `return` (line 2, col 4) → the return type `Pair` (line 1).
        let r = goto(RETURN_SRC, Position::new(2, 4)).expect("goto on `return`");
        assert_eq!(
            r.start,
            Position::new(1, 17),
            "expected the return type: {r:?}"
        );
        assert_eq!(r.end, Position::new(1, 21));
    }

    // line 2 = "    output.a = p.a;"; `output` is named on line 1.
    const NAMED_SRC: &str = "struct Pair = pair { a: uint(8), b: uint(8) }\nfn f(p: Pair) -> (output: Pair) {\n    output.a = p.a;\n}\n";

    #[test]
    fn goto_named_result_jumps_to_its_signature_name() {
        // Cursor on `output` (line 2, col 4) → its name in `-> (output: …)`
        // (line 1, col 18).
        let r = goto(NAMED_SRC, Position::new(2, 4)).expect("goto on `output`");
        assert_eq!(
            r.start,
            Position::new(1, 18),
            "expected the named result: {r:?}"
        );
        assert_eq!(r.end, Position::new(1, 24));
    }

    // line 1 = "    carry = false;"; `carry` is the 2nd named tuple part on line 0.
    const TUPLE_SRC: &str =
        "fn g(x: uint(8)) -> (sum: uint(8), carry: bool) {\n    carry = false;\n    sum = x;\n}\n";

    #[test]
    fn goto_named_tuple_part_jumps_to_its_signature_name() {
        // Cursor on `carry` (line 1, col 4) → the `carry` part name (line 0).
        let r = goto(TUPLE_SRC, Position::new(1, 4)).expect("goto on `carry`");
        assert_eq!(r.start.line, 0, "expected the `carry` part name: {r:?}");
        assert_eq!(r.start.character, 35);
    }

    #[test]
    fn goto_constructor_jumps_to_the_struct_definition() {
        let src = "struct Packet = packet {\n  valid: bool,\n}\n\nfn f\n  \
            { dom clk: Clock }\n  ( inp: Packet @clk )\n  -> Packet @clk\n  {\n    \
            packet { valid: false }\n  }\n";
        // Cursor on the `packet` constructor use (line 9, col ~4).
        let r = goto(src, Position::new(9, 5)).expect("goto on `packet`");
        assert_eq!(
            r.start.line, 0,
            "expected jump to the struct def on line 0: {r:?}"
        );
    }
}
