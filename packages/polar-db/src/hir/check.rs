//! Per-def **check** queries over the body HIR (`planning/q3_typed_hir.md`,
//! pass #8). Diagnostics that need the lowered body but little else.
//!
//! Q3e ships [`check_drivers`]: every `var` must have exactly one driving
//! equation. The other Q3-plan checks are blocked on later slices and land with
//! them:
//!
//! - **`check_directions`** (connection operator `=`/`=>` vs port-field
//!   direction) needs the named-argument / out-argument connections that `body`
//!   currently defers — so it lands with that lowering (Q5).
//! - **ground width checks** need `const_eval` (Q4).

use std::collections::HashMap;

use crate::base::db::SourceRoot;
use crate::hir::body::{Block, Body, ExprId, ExprKind, LocalKind, Stmt, body};
use crate::hir::types::LocalId;
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

/// Recurse into the blocks nested inside an expression (`if`/`when`/`block`),
/// so an equation in a `when` body or an `if` branch is counted too.
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
        _ => {}
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
