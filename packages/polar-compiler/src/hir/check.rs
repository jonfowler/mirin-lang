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
use crate::hir::body::{Block, Body, ExprId, ExprKind, LocalKind, Stmt, body};
use crate::hir::sig::sig_of;
use crate::hir::types::{Direction, LocalId};
use crate::nameres::def_map::crate_def_map;
use crate::nameres::ids::{DefId, DefKind};

/// A `var`-driver violation.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum DriverDiagnostic {
    /// A `var` with no driving equation.
    Undriven { name: String },
    /// A `var` with two or more driving equations.
    MultipleDrivers { name: String },
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

    // Count driving equations per local.
    let mut counts: HashMap<LocalId, usize> = HashMap::new();
    count_block(body, body.block(), &mut counts);

    // Report each `var` that isn't driven exactly once, in declaration order.
    let mut out = Vec::new();
    for (i, local) in body.locals().iter().enumerate() {
        if local.kind != LocalKind::Var {
            continue;
        }
        match counts.get(&LocalId(i as u32)).copied().unwrap_or(0) {
            1 => {}
            0 => out.push(DriverDiagnostic::Undriven {
                name: local.name.clone(),
            }),
            _ => out.push(DriverDiagnostic::MultipleDrivers {
                name: local.name.clone(),
            }),
        }
    }
    out
}

fn count_block(body: &Body, block: &Block, counts: &mut HashMap<LocalId, usize>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Equation { lhs, rhs } => {
                if let ExprKind::Local(l) = body.expr(*lhs).kind {
                    *counts.entry(l).or_default() += 1;
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
fn count_expr(body: &Body, expr: ExprId, counts: &mut HashMap<LocalId, usize>) {
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
        ExprKind::Call { args, named, .. } => {
            for a in args {
                if a.out
                    && let ExprKind::Local(l) = body.expr(a.expr).kind
                {
                    *counts.entry(l).or_default() += 1;
                }
                count_expr(body, a.expr, counts);
            }
            for n in named {
                if n.out
                    && let ExprKind::Local(l) = body.expr(n.expr).kind
                {
                    *counts.entry(l).or_default() += 1;
                }
                count_expr(body, n.expr, counts);
            }
        }
        _ => {}
    }
}

/// A connection whose operator disagrees with the callee param's direction.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum DirectionDiagnostic {
    /// A value connection (`name = v` / positional / shorthand) to an `out` param.
    ValueToOut { param: String },
    /// An out-connection (`=>`) to a param that is not `out`.
    OutToNonOut { param: String },
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
                check_dir(a.out, p.direction, &p.name, &mut out);
            }
        }
        for n in named {
            if let Some(p) = sig
                .params
                .iter()
                .find(|p| p.from_named_section && p.name == n.name)
            {
                check_dir(n.out, p.direction, &p.name, &mut out);
            }
        }
    }
    out
}

fn check_dir(is_out: bool, dir: Option<Direction>, name: &str, out: &mut Vec<DirectionDiagnostic>) {
    if is_out && dir != Some(Direction::Out) {
        out.push(DirectionDiagnostic::OutToNonOut {
            param: name.to_owned(),
        });
    } else if !is_out && dir == Some(Direction::Out) {
        out.push(DirectionDiagnostic::ValueToOut {
            param: name.to_owned(),
        });
    }
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
        assert!(
            dirs(&db, krate, "t")
                .iter()
                .any(|d| matches!(d, DirectionDiagnostic::ValueToOut { param } if param == "o"))
        );
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
        assert!(
            dirs(&db, krate, "t")
                .iter()
                .any(|d| matches!(d, DirectionDiagnostic::OutToNonOut { param } if param == "i"))
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
                .any(|d| matches!(d, DriverDiagnostic::Undriven { name } if name == "x"))
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
        assert!(
            drivers(&db, krate, "h")
                .iter()
                .any(|d| matches!(d, DriverDiagnostic::MultipleDrivers { name } if name == "x"))
        );
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
