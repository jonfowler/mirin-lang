//! Per-def **check** queries over the body HIR (`planning/q3_typed_hir.md`,
//! pass #8). Diagnostics that need the lowered body but little else.
//!
//! - [`check_drivers`] (Q3e): every `var` has exactly one driver — a driving
//!   equation or an out-connection (`=>`) target.
//! - [`directions`] (Q5a): a call's connection operators agree with the callee's
//!   parameter directions (`=`→`in`, `=>`→`out`).
//!
//! Still deferred: port-to-port equation field-direction pairing (a flatten-time
//! concern, Q5d) and ground width checks (`const_eval`, Q4).

use std::collections::HashMap;

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::hir::body::{Block, Body, ExprId, ExprKind, LocalKind, Stmt, body};
use crate::hir::sig::sig_of;
use crate::hir::types::{Direction, LocalId};
use crate::nameres::def_map::crate_def_map;
use crate::nameres::ids::{DefId, DefKind};

/// A `var`-driver violation, with the var's def-relative declaration span.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct DriverDiagnostic {
    pub span: Span,
    pub kind: DriverDiagnosticKind,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum DriverDiagnosticKind {
    /// A `var` with no driving equation.
    Undriven { name: String },
    /// A `var` with two or more driving equations.
    MultipleDrivers { name: String },
    /// A field-driven local whose field equations don't cover the type
    /// (typed completeness — `completeness(def)`).
    UndrivenField { name: String, field: String },
    /// An `out` param the body never drives.
    UndrivenOut { name: String },
}

impl DriverDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            DriverDiagnosticKind::Undriven { name } => {
                format!("`var {name}` is never driven (needs an equation or out-connection)")
            }
            DriverDiagnosticKind::MultipleDrivers { name } => {
                format!("`{name}` is driven more than once")
            }
            DriverDiagnosticKind::UndrivenField { name, field } => {
                format!("field `{field}` of `{name}` is never driven")
            }
            DriverDiagnosticKind::UndrivenOut { name } => {
                format!("`out {name}` is never driven")
            }
        }
    }
}

/// QUERY: check that every `var` in a function body has exactly one driver.
///
/// A driver is an equation `var = …` (including a `var x = e` initialiser).
/// Runs on the tree-shaped body HIR — before block flattening (Q5) — so a `var`
/// driven by an `if`/`when` *value* is a single equation (`x = if … {…}`), not
/// per-branch assignments; conditional/out-argument driving is revisited when
/// those lower.
#[salsa::tracked(returns(ref))]
pub fn check_drivers<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Vec<DriverDiagnostic> {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Vec::new();
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return Vec::new();
    }
    let body = body(db, krate, def);

    // Collect drive *paths* per local: `x = …` drives the whole var (empty
    // path), `x.f = …` drives the leaf `f`. Per-leaf accounting accepts a var
    // wired field-by-field, and rejects overlapping drives (whole + field, or
    // the same path twice).
    let mut drives: HashMap<LocalId, Vec<Vec<String>>> = HashMap::new();
    count_block(body, body.block(), &mut drives);

    let mut out = Vec::new();
    for (i, local) in body.locals().iter().enumerate() {
        let span = body.local_span(LocalId(i as u32));
        let paths = drives.get(&LocalId(i as u32)).map(Vec::as_slice).unwrap_or(&[]);
        if paths.is_empty() {
            // Only a `var` must be driven; params are driven by the caller
            // (or drive field equations themselves), lets by their binding.
            if local.kind == LocalKind::Var {
                out.push(DriverDiagnostic {
                    span,
                    kind: DriverDiagnosticKind::Undriven {
                        name: local.name.clone(),
                    },
                });
            }
            continue;
        }
        // Two drives conflict when one path is a prefix of the other
        // (equality included): `x` + `x.a`, or `x.a` twice — for every local
        // kind (a param's fields are drivable too: `downstream.valid = …`).
        // Disjoint field paths are fine — that's per-field wiring. (Whether
        // the field drives *cover* the type needs type info — the typed
        // completeness pass.)
        let overlap = paths.iter().enumerate().any(|(i, a)| {
            paths[i + 1..]
                .iter()
                .any(|b| a.starts_with(&b[..]) || b.starts_with(&a[..]))
        });
        if overlap {
            out.push(DriverDiagnostic {
                span,
                kind: DriverDiagnosticKind::MultipleDrivers {
                    name: local.name.clone(),
                },
            });
        }
    }
    out
}

/// Resolve an equation LHS to its base local and field path (`x.a.b` →
/// `(x, ["a", "b"])`). `None` for non-place LHS shapes.
fn place_of(body: &Body, expr: ExprId) -> Option<(LocalId, Vec<String>)> {
    match &body.expr(expr).kind {
        ExprKind::Local(l) => Some((*l, Vec::new())),
        ExprKind::Field { receiver, field } => {
            let (l, mut path) = place_of(body, *receiver)?;
            path.push(field.clone());
            Some((l, path))
        }
        _ => None,
    }
}

/// QUERY: typed drive **completeness** (post-infer — single-assignment
/// conflicts are syntactic and live in [`check_drivers`], but which fields a
/// type *has* is only known once it has a type). A struct-typed local driven
/// through field equations must cover every leaf; an `out` param must be
/// driven at all. Port-typed locals that are *partially* field-driven are
/// skipped for now — which of their leaves this def must drive depends on
/// direction folding (the flatten-time pairing, with Q5d).
#[salsa::tracked(returns(ref))]
pub fn completeness<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Vec<DriverDiagnostic> {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Vec::new();
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return Vec::new();
    }
    let body = body(db, krate, def);
    if body.verilog().is_some() {
        // An inline-verilog body is trusted to drive what its signature
        // declares (planning/inline_verilog.md; verilator is the backstop).
        return Vec::new();
    }
    let inf = crate::hir::infer::infer(db, krate, def);
    let sig = sig_of(db, krate, def);

    let mut drives: HashMap<LocalId, Vec<Vec<String>>> = HashMap::new();
    count_block(body, body.block(), &mut drives);

    let mut out = Vec::new();
    for (i, local) in body.locals().iter().enumerate() {
        let id = LocalId(i as u32);
        let span = body.local_span(id);
        let paths = drives.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        let ty = inf.local_type(id);
        let is_out_param = sig
            .params
            .iter()
            .any(|p| p.local == id && p.direction == Some(Direction::Out));

        if paths.is_empty() {
            // An undriven out param (vars are check_drivers' job; integer
            // values are compile-time only and need no driver).
            if is_out_param && !ty.is_some_and(is_integer_ty) {
                out.push(DriverDiagnostic {
                    span,
                    kind: DriverDiagnosticKind::UndrivenOut {
                        name: local.name.clone(),
                    },
                });
            }
            continue;
        }
        if paths.iter().any(|p| p.is_empty()) {
            continue; // a whole-local drive is complete by definition
        }
        // Field-driven: every leaf of the type must be covered. Structs only
        // — see the port note above. Only vars and out params owe a complete
        // drive (an in param's fields may be partially rewired).
        if local.kind != LocalKind::Var && !is_out_param {
            continue;
        }
        let Some(ty) = ty else { continue };
        for leaf in struct_leaf_paths(db, krate, ty, 0) {
            let covered = paths.iter().any(|p| leaf.starts_with(&p[..]));
            if !covered {
                out.push(DriverDiagnostic {
                    span,
                    kind: DriverDiagnosticKind::UndrivenField {
                        name: local.name.clone(),
                        field: leaf.join("."),
                    },
                });
            }
        }
    }
    out
}

/// The field paths of a struct type down to non-struct leaves; empty for
/// non-structs (ports deferred — direction folding decides their owed set).
fn struct_leaf_paths<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    ty: &crate::hir::types::Type<'db>,
    depth: u32,
) -> Vec<Vec<String>> {
    use crate::hir::types::{Type, ValueKind};
    if depth > 16 {
        return Vec::new();
    }
    let Type::Value {
        kind: ValueKind::Struct { def, .. },
        ..
    } = ty
    else {
        return Vec::new();
    };
    let sig = sig_of(db, krate, *def);
    let mut out = Vec::new();
    for f in &sig.fields {
        let subs = struct_leaf_paths(db, krate, &f.ty, depth + 1);
        if subs.is_empty() {
            out.push(vec![f.name.clone()]);
        } else {
            for mut sub in subs {
                sub.insert(0, f.name.clone());
                out.push(sub);
            }
        }
    }
    out
}

fn is_integer_ty(ty: &crate::hir::types::Type<'_>) -> bool {
    use crate::hir::types::{Type, ValueKind};
    matches!(
        ty,
        Type::Value {
            kind: ValueKind::Integer,
            ..
        }
    )
}

fn count_block(body: &Body, block: &Block, counts: &mut HashMap<LocalId, Vec<Vec<String>>>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Equation { lhs, rhs } => {
                if let Some((l, path)) = place_of(body, *lhs) {
                    counts.entry(l).or_default().push(path);
                }
                count_expr(body, *lhs, counts);
                count_expr(body, *rhs, counts);
            }
            Stmt::Let { value, .. } => count_expr(body, *value, counts),
            Stmt::Return { value } => count_expr(body, *value, counts),
            Stmt::Expr(e) => count_expr(body, *e, counts),
            Stmt::VarDecl { .. } => {}
        }
    }
    if let Some(tail) = block.tail {
        count_expr(body, tail, counts);
    }
}

/// Recurse into the blocks nested in an expression (`if`/`when`/`block`), and
/// count out-connection (`=>`) targets as drivers — an `out` arg wires the
/// callee's output *into* its target local, which is a driver of that local.
fn count_expr(body: &Body, expr: ExprId, counts: &mut HashMap<LocalId, Vec<Vec<String>>>) {
    match &body.expr(expr).kind {
        ExprKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            count_block(body, then_branch, counts);
            count_block(body, else_branch, counts);
        }
        ExprKind::When { body: b, .. } => count_block(body, b, counts),
        ExprKind::Block(b) => count_block(body, b, counts),
        ExprKind::Record { fields, .. } => {
            for f in fields {
                if f.out
                    && let Some((l, path)) = place_of(body, f.value)
                {
                    counts.entry(l).or_default().push(path);
                }
                count_expr(body, f.value, counts);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            count_expr(body, *receiver, counts);
            for a in args {
                if a.out
                    && let Some((l, path)) = place_of(body, a.expr)
                {
                    counts.entry(l).or_default().push(path);
                }
                count_expr(body, a.expr, counts);
            }
        }
        ExprKind::Call { args, named, .. } => {
            for a in args {
                if a.out
                    && let Some((l, path)) = place_of(body, a.expr)
                {
                    counts.entry(l).or_default().push(path);
                }
                count_expr(body, a.expr, counts);
            }
            for n in named {
                if n.out
                    && let Some((l, path)) = place_of(body, n.expr)
                {
                    counts.entry(l).or_default().push(path);
                }
                count_expr(body, n.expr, counts);
            }
        }
        _ => {}
    }
}

/// A connection whose operator disagrees with the callee param's direction,
/// with the connection's def-relative span.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct DirectionDiagnostic {
    pub span: Span,
    pub kind: DirectionDiagnosticKind,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum DirectionDiagnosticKind {
    /// A value connection (`name = v` / positional / shorthand) to an `out` param.
    ValueToOut { param: String },
    /// An out-connection (`=>`) to a param that is not `out`.
    OutToNonOut { param: String },
    /// A named argument with no matching named parameter on the callee.
    UnknownNamedArg { callee: String, name: String },
}

impl DirectionDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            DirectionDiagnosticKind::ValueToOut { param } => {
                format!("`{param}` is an `out` parameter — connect it with `=>`, not `=`")
            }
            DirectionDiagnosticKind::OutToNonOut { param } => {
                format!("`=>` cannot drive `{param}`; only an `out` parameter accepts it")
            }
            DirectionDiagnosticKind::UnknownNamedArg { callee, name } => {
                format!("`{callee}` has no named parameter `{name}`")
            }
        }
    }
}

/// QUERY: check that each call's connection operators agree with the callee's
/// parameter directions — a value (`=`) connects to an `in`/undirected param, an
/// out-connection (`=>`) to an `out` param. Mirrors the old `check_directions`'s
/// call-site rule. (Port-to-port equation field pairing is a flatten-time
/// concern, Q5d.) Per-def, over `body` + callee `sig_of`; no types.
#[salsa::tracked(returns(ref))]
pub fn directions<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Vec<DirectionDiagnostic> {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Vec::new();
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return Vec::new();
    }
    let body = body(db, krate, def);

    let mut out = Vec::new();
    for expr in body.exprs() {
        let ExprKind::Call {
            callee,
            args,
            named,
        } = &expr.kind
        else {
            continue;
        };
        let ExprKind::Def(callee) = body.expr(*callee).kind else {
            continue;
        };
        let sig = sig_of(db, krate, callee);
        // Positional args bind the positional value params (declared order).
        let positional: Vec<_> = sig
            .params
            .iter()
            .filter(|p| !p.from_named_section && !p.is_self)
            .collect();
        for (i, a) in args.iter().enumerate() {
            if let Some(p) = positional.get(i) {
                check_dir(
                    a.out,
                    p.direction,
                    &p.name,
                    body.expr_span(a.expr),
                    &mut out,
                );
            }
        }
        for n in named {
            if let Some(p) = sig
                .params
                .iter()
                .find(|p| p.from_named_section && p.name == n.name)
            {
                check_dir(
                    n.out,
                    p.direction,
                    &p.name,
                    body.expr_span(n.expr),
                    &mut out,
                );
            } else if !sig
                .generic_params
                .iter()
                .any(|g| g.from_named_section && g.name == n.name)
            {
                // Not a named value param and not a named-section generic
                // (`{clk}`/`{N}`) — there is no such named parameter.
                out.push(DirectionDiagnostic {
                    span: body.expr_span(n.expr),
                    kind: DirectionDiagnosticKind::UnknownNamedArg {
                        callee: map
                            .def_data(callee)
                            .map(|d| d.name.clone())
                            .unwrap_or_default(),
                        name: n.name.clone(),
                    },
                });
            }
        }
    }
    out
}

fn check_dir(
    is_out: bool,
    dir: Option<Direction>,
    name: &str,
    span: Span,
    out: &mut Vec<DirectionDiagnostic>,
) {
    let kind = if is_out && dir != Some(Direction::Out) {
        DirectionDiagnosticKind::OutToNonOut {
            param: name.to_owned(),
        }
    } else if !is_out && dir == Some(Direction::Out) {
        DirectionDiagnosticKind::ValueToOut {
            param: name.to_owned(),
        }
    } else {
        return;
    };
    out.push(DirectionDiagnostic { span, kind });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;
    use crate::nameres::ids::Namespace;

    fn load(db: &mut RootDatabase, vfs: &mut Vfs, text: &str) -> SourceRoot {
        vfs.set_file_text(db, "t.plr", text);
        vfs.source_root(db, "t.plr")
    }

    fn drivers<'db>(
        db: &'db RootDatabase,
        krate: SourceRoot,
        name: &str,
    ) -> &'db Vec<DriverDiagnostic> {
        let map = crate_def_map(db, krate);
        let def = map
            .resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("def");
        check_drivers(db, krate, def)
    }

    fn dirs<'db>(
        db: &'db RootDatabase,
        krate: SourceRoot,
        name: &str,
    ) -> &'db Vec<DirectionDiagnostic> {
        let map = crate_def_map(db, krate);
        let def = map
            .resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("def");
        directions(db, krate, def)
    }

    #[test]
    fn an_out_connection_to_an_out_param_is_fine() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn snk { out o: uint(8) } (i: uint(8)) { o = i; }\nfn t (i: uint(8), out r: uint(8)) { snk{o => r}(i); }",
        );
        assert!(
            dirs(&db, krate, "t").is_empty(),
            "{:?}",
            dirs(&db, krate, "t")
        );
    }

    #[test]
    fn a_value_connection_to_an_out_param_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // `o = r` connects a value to the `out` param `o` — wrong operator.
        let krate = load(
            &mut db,
            &mut vfs,
            "fn snk { out o: uint(8) } (i: uint(8)) { o = i; }\nfn t (i: uint(8), r: uint(8)) { snk{o = r}(i); }",
        );
        assert!(dirs(&db, krate, "t").iter().any(
            |d| matches!(&d.kind, DirectionDiagnosticKind::ValueToOut { param } if param == "o")
        ));
    }

    #[test]
    fn an_out_connection_to_an_in_param_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // positional `out => x` to `f`'s `in`/undirected param.
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (i: uint(8)) -> uint(8) { return i; }\nfn t (out x: uint(8)) { f(out => x); }",
        );
        assert!(dirs(&db, krate, "t").iter().any(
            |d| matches!(&d.kind, DirectionDiagnosticKind::OutToNonOut { param } if param == "i")
        ));
    }

    #[test]
    fn an_unknown_named_argument_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // `target` has no named parameter `typo`.
        let krate = load(
            &mut db,
            &mut vfs,
            "fn target { in a: uint(8) } (x: uint(8)) { a = x; }\nfn t (x: uint(8)) { target{typo = 5}(x); }",
        );
        assert!(
            dirs(&db, krate, "t")
                .iter()
                .any(|d| matches!(&d.kind, DirectionDiagnosticKind::UnknownNamedArg { callee, name } if callee == "target" && name == "typo")),
            "{:?}",
            dirs(&db, krate, "t")
        );
    }

    #[test]
    fn a_named_section_generic_arg_is_not_unknown() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // `{clk}` is a named-section `dom` generic, not an unknown param.
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f { dom clk: Clock } (x: uint(8) @clk) -> uint(8) @clk { return x; }\nfn t { dom clk: Clock } (x: uint(8) @clk) -> uint(8) @clk { return f{clk}(x); }",
        );
        assert!(
            dirs(&db, krate, "t").is_empty(),
            "{:?}",
            dirs(&db, krate, "t")
        );
    }

    #[test]
    fn a_driven_var_is_fine() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn g (a: uint(8)) -> uint(8) { var x; x = a; return x; }",
        );
        assert!(drivers(&db, krate, "g").is_empty());
    }

    #[test]
    fn a_var_initialiser_counts_as_its_driver() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn k (a: uint(8)) -> uint(8) { var x = a; return x; }",
        );
        assert!(drivers(&db, krate, "k").is_empty());
    }

    #[test]
    fn an_undriven_var_is_reported() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(&mut db, &mut vfs, "fn f () -> uint(8) { var x; return 0; }");
        assert!(
            drivers(&db, krate, "f")
                .iter()
                .any(|d| matches!(&d.kind, DriverDiagnosticKind::Undriven { name } if name == "x"))
        );
    }

    #[test]
    fn a_doubly_driven_var_is_reported() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn h (a: uint(8)) -> uint(8) { var x; x = a; x = a; return x; }",
        );
        assert!(drivers(&db, krate, "h").iter().any(
            |d| matches!(&d.kind, DriverDiagnosticKind::MultipleDrivers { name } if name == "x")
        ));
    }

    #[test]
    fn a_register_var_has_exactly_one_driver() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // The idiomatic `when`-register: `count` is driven once by the assignment
        // whose RHS is the `when` value (the example `when_counter.plr`).
        let krate = load(
            &mut db,
            &mut vfs,
            "fn counter { dom clk: Clock } () -> uint(8) @clk { var count: uint(8) @clk; count = when clk.posedge() { count + 1 }; count }",
        );
        assert!(
            drivers(&db, krate, "counter").is_empty(),
            "{:?}",
            drivers(&db, krate, "counter")
        );
    }
}
