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
use crate::hir::types::{ConstArg, Direction, LocalId, Type, ValueKind};
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
    // A trait method DECLARATION has no body to check.
    if crate_def_map(db, krate).is_trait_method_decl(def) {
        return Vec::new();
    }

    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Vec::new();
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return Vec::new();
    }
    let body = body(db, krate, def);
    // An inline-verilog body has no equations — it's trusted to drive what its
    // signature declares (like `completeness`). Its synthetic `return` place
    // is never driven by HIR, so don't demand it.
    if body.verilog().is_some() {
        return Vec::new();
    }

    // Collect drive *paths* per local: `x = …` drives the whole var (empty
    // path), `x.f = …` drives the leaf `f`. Per-leaf accounting accepts a var
    // wired field-by-field, and rejects overlapping drives (whole + field, or
    // the same path twice).
    let mut drives: HashMap<LocalId, Vec<Vec<String>>> = HashMap::new();
    count_block(body, body.block(), &mut drives);

    let mut out = Vec::new();
    for (i, local) in body.locals().iter().enumerate() {
        let span = body.local_span(LocalId(i as u32));
        let paths = drives
            .get(&LocalId(i as u32))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
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
        // A `let mut` is reassigned in place — sequential, not an equation
        // system — so repeated whole-place "drives" are expected, not a
        // conflict (proposals/compile_mutable.md). Skip the overlap check.
        if local.mutable {
            continue;
        }
        // Two drives conflict when one path is a prefix of the other
        // (equality included): `x` + `x.a`, or `x.a` twice — for every local
        // kind (a param's fields are drivable too: `downstream.valid = …`).
        // Disjoint field paths are fine — that's per-field wiring. Two slice/index
        // segments at the same position conflict when their ranges OVERLAP
        // (`v[0..2]` + `v[1..3]` both drive index 1), not just when one prefixes
        // the other. (Whether the field drives *cover* the type needs type info —
        // the typed completeness pass.)
        let overlap = paths
            .iter()
            .enumerate()
            .any(|(i, a)| paths[i + 1..].iter().any(|b| paths_conflict(a, b)));
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
        // Indexed places: a GROUND index is an element path (`"[2]"` — a
        // distinct drive from `"[1]"`, conflicting only with itself or the
        // whole); a `for`-bound index covers the WHOLE place (the loop
        // spans every index); anything else is not a valid drive target
        // (flagged in the walk).
        ExprKind::Index { base, index } => {
            let (l, mut path) = place_of(body, *base)?;
            match &body.expr(*index).kind {
                ExprKind::Number(k, _) => {
                    path.push(format!("[{k}]"));
                    Some((l, path))
                }
                ExprKind::Local(i) if matches!(body.local(*i).kind, LocalKind::ForBound) => {
                    Some((l, path))
                }
                // A compound index that references a `for`-bound genvar
                // (`b[i*a + j]`) covers the WHOLE place: the loop(s) span every
                // index. Like the bare-genvar case, coverage is assumed, not
                // proven (rigorous range checking is deferred) — the unit of a
                // per-bit `bits` construction (planning/pack_resize.md).
                _ if index_uses_forbound(body, *index) => Some((l, path)),
                _ => None,
            }
        }
        // A slice-set `x[a..b] = …` partially drives its base over the slice's
        // run — a DISTINCT partial-drive path per range, so tiling slices don't
        // false-conflict. Range coverage is not verified (deferred, like the
        // genvar-index case) — planning/slicing.md.
        ExprKind::Slice {
            base,
            lo,
            hi,
            width,
        } => {
            let (l, mut path) = place_of(body, *base)?;
            path.push(slice_seg(body, *lo, *hi, *width));
            Some((l, path))
        }
        _ => None,
    }
}

/// A syntactic segment identifying a slice-set's range (`[8..0]`, `[?..+4]` for a
/// runtime base) — distinguishes tiling slices for conflict detection.
fn slice_seg(body: &Body, lo: Option<ExprId>, hi: Option<ExprId>, width: Option<ExprId>) -> String {
    let part = |o: Option<ExprId>| match o.map(|e| &body.expr(e).kind) {
        Some(ExprKind::Number(v, _)) => v.to_string(),
        Some(_) => "?".to_owned(),
        None => String::new(),
    };
    match width {
        Some(_) => format!("[{}..+{}]", part(lo), part(width)),
        None => format!("[{}..{}]", part(lo), part(hi)),
    }
}

/// Does a constant vec slice-set segment (`[lo..hi]` or `[lo..+w]`, as produced
/// by [`slice_seg`]) cover element index `k` of a length-`n` vector? An elided
/// endpoint defaults (lo→0, hi→n); a runtime endpoint (`?`) can't be credited,
/// so it returns `false` (the index stays uncovered — coverage must be provable).
fn vec_slice_covers(seg: &str, k: i128, n: i128) -> bool {
    let parse = |s: &str, default: i128| -> Option<i128> {
        match s {
            "" => Some(default),
            "?" => None,
            v => v.parse().ok(),
        }
    };
    let Some(inner) = seg.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    if let Some((lo_s, w_s)) = inner.split_once("..+") {
        match (parse(lo_s, 0), parse(w_s, 0).filter(|_| w_s != "")) {
            (Some(lo), Some(w)) => k >= lo && k < lo + w,
            _ => false,
        }
    } else if let Some((lo_s, hi_s)) = inner.split_once("..") {
        match (parse(lo_s, 0), parse(hi_s, n)) {
            (Some(lo), Some(hi)) => k >= lo && k < hi,
            _ => false,
        }
    } else {
        false
    }
}

/// Do two drive paths conflict? Walk segment by segment: equal segments agree;
/// at the first differing pair, if both are concrete index/slice ranges they
/// conflict iff the ranges OVERLAP, otherwise they are disjoint (different fields,
/// or a range we can't prove — conservatively NOT a conflict, to avoid false
/// positives). If no segment differs within the shorter path, one is a prefix of
/// the other (`x` + `x.a`, or identical) — a conflict.
fn paths_conflict(a: &[String], b: &[String]) -> bool {
    for (sa, sb) in a.iter().zip(b.iter()) {
        if sa == sb {
            continue;
        }
        return match (seg_range(sa), seg_range(sb)) {
            (Some((lo_a, hi_a)), Some((lo_b, hi_b))) => lo_a < hi_b && lo_b < hi_a,
            _ => false,
        };
    }
    true
}

/// The half-open integer range `[lo, hi)` a constant index/slice path segment
/// covers, normalised so it is **direction-agnostic** — bits slices are written
/// high-first (`[8..0]`) and vec slices low-first (`[0..2]`), but both cover the
/// same integer set, and no type info is available here. `None` for an index
/// using a genvar, an elided endpoint, or a runtime (`?`) endpoint — those can't
/// be proven to overlap, so the caller treats them as non-conflicting.
fn seg_range(seg: &str) -> Option<(i128, i128)> {
    let inner = seg.strip_prefix('[')?.strip_suffix(']')?;
    if let Some((lo_s, w_s)) = inner.split_once("..+") {
        // Offset form `[off..+w]` — always low-first.
        let (lo, w) = (lo_s.parse::<i128>().ok()?, w_s.parse::<i128>().ok()?);
        Some((lo, lo + w))
    } else if let Some((lo_s, hi_s)) = inner.split_once("..") {
        let (a, b) = (lo_s.parse::<i128>().ok()?, hi_s.parse::<i128>().ok()?);
        Some((a.min(b), a.max(b)))
    } else {
        // A bare index `[k]` covers the single element `[k, k+1)`.
        let k = inner.parse::<i128>().ok()?;
        Some((k, k + 1))
    }
}

/// Does an index expression reference a `for`-bound genvar? (`i`, `i*a + j`,
/// …) — used to recognise a loop-spanning partial drive whose index is a
/// genvar *expression*, not a bare genvar.
fn index_uses_forbound(body: &Body, expr: ExprId) -> bool {
    match &body.expr(expr).kind {
        ExprKind::Local(i) => matches!(body.local(*i).kind, LocalKind::ForBound),
        ExprKind::MethodCall { receiver, args, .. } => {
            index_uses_forbound(body, *receiver)
                || args.iter().any(|a| index_uses_forbound(body, a.expr))
        }
        ExprKind::Index { base, index } => {
            index_uses_forbound(body, *base) || index_uses_forbound(body, *index)
        }
        ExprKind::Field { receiver, .. } => index_uses_forbound(body, *receiver),
        _ => false,
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
    // A trait method DECLARATION has no body to check.
    if crate_def_map(db, krate).is_trait_method_decl(def) {
        return Vec::new();
    }

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
        // Element-driven vector: every index 0..N must be covered
        // (`v[0] = …; v[1] = …;` — a missing element is an undriven leaf).
        if let Type::Vec { len, .. } = ty
            && paths
                .iter()
                .all(|p| p.first().is_some_and(|s| s.starts_with('[')))
        {
            if let ConstArg::Lit(n) = len {
                for k in 0..*n {
                    let seg = format!("[{k}]");
                    // An index is covered by an exact element drive (`v[k] = …`)
                    // or by a constant slice-set whose range spans it
                    // (`v[lo..hi] = …`). A runtime-endpoint slice can't be
                    // credited for a specific index (stays uncovered).
                    let covered = paths.iter().any(|p| {
                        p.first()
                            .is_some_and(|f| *f == seg || vec_slice_covers(f, k, *n))
                    });
                    if !covered {
                        out.push(DriverDiagnostic {
                            span,
                            kind: DriverDiagnosticKind::UndrivenField {
                                name: local.name.clone(),
                                field: seg,
                            },
                        });
                    }
                }
            }
            continue;
        }
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

/// The field paths of a struct/tuple type down to non-aggregate leaves; empty
/// for other types (ports deferred — direction folding decides their owed set).
fn struct_leaf_paths<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    ty: &crate::hir::types::Type<'db>,
    depth: u32,
) -> Vec<Vec<String>> {
    use crate::hir::types::Type;
    if depth > 16 {
        return Vec::new();
    }
    // A tuple's "fields" are its element indices: `r.0 = …` covers leaf "0"
    // (planning/tuples.md).
    if let Type::Tuple(elems) = ty {
        let mut out = Vec::new();
        for (i, ety) in elems.iter().enumerate() {
            let subs = struct_leaf_paths(db, krate, ety, depth + 1);
            if subs.is_empty() {
                out.push(vec![i.to_string()]);
            } else {
                for mut sub in subs {
                    sub.insert(0, i.to_string());
                    out.push(sub);
                }
            }
        }
        return out;
    }
    // Only a *struct* record contributes positive leaf paths; a `port`'s owed
    // set is decided by direction folding (deferred). Both are `Type::Port`
    // now, so distinguish by the def's `DefKind` (structs_as_ports.md).
    let Type::Port { def, .. } = ty else {
        return Vec::new();
    };
    if crate_def_map(db, krate).def_data(*def).map(|d| d.kind) != Some(DefKind::Struct) {
        return Vec::new();
    }
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
            Stmt::For { iter, body: b, .. } => {
                count_expr(body, *iter, counts);
                count_block(body, b, counts);
            }
            // A statement-form `when` is ONE clocked binding per driven `var`:
            // all its body drives (disjoint leaves, possibly dynamic-index)
            // form that var's next-state — a register holds unwritten parts, so
            // the var counts as WHOLE-place driven (complete, and conflicting
            // with any drive outside the `when`). Internal drives don't conflict
            // with each other. `init` is power-on, not a competing driver
            // (proposals/when_binding.md rule 2).
            Stmt::When { event, body: b, .. } => {
                count_expr(body, *event, counts);
                let mut driven = std::collections::HashSet::new();
                collect_when_drives(body, b, &mut driven, counts);
                for l in driven {
                    counts.entry(l).or_default().push(Vec::new());
                }
            }
        }
    }
    if let Some(tail) = block.tail {
        count_expr(body, tail, counts);
    }
}

/// The base local of a place expression (`ram`, `v.valid`, `ram[addr]` all root
/// at the same local) — unlike `place_of`, this also succeeds for a dynamic
/// index, since a clocked `when` drive of `ram[addr]` still binds `ram`.
fn root_local(body: &Body, expr: ExprId) -> Option<LocalId> {
    match &body.expr(expr).kind {
        ExprKind::Local(l) => Some(*l),
        ExprKind::Field { receiver, .. } => root_local(body, *receiver),
        ExprKind::Index { base, .. } => root_local(body, *base),
        _ => None,
    }
}

/// Collect the set of `var`s driven inside a `when` body (descending guarded
/// `if`-drives), and count any nested out-connections in the rhs/conditions.
fn collect_when_drives(
    body: &Body,
    block: &Block,
    driven: &mut std::collections::HashSet<LocalId>,
    counts: &mut HashMap<LocalId, Vec<Vec<String>>>,
) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Equation { lhs, rhs } => {
                if let Some(l) = root_local(body, *lhs) {
                    driven.insert(l);
                }
                count_expr(body, *rhs, counts);
            }
            Stmt::Expr(e) => match &body.expr(*e).kind {
                ExprKind::If {
                    cond,
                    then_branch,
                    else_branch,
                }
                | ExprKind::ConstIf {
                    cond,
                    then_branch,
                    else_branch,
                } => {
                    count_expr(body, *cond, counts);
                    collect_when_drives(body, then_branch, driven, counts);
                    collect_when_drives(body, else_branch, driven, counts);
                }
                _ => count_expr(body, *e, counts),
            },
            Stmt::Let { value, .. } => count_expr(body, *value, counts),
            _ => {}
        }
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
        }
        | ExprKind::ConstIf {
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

/// A Mirin-bodied `#[inline]` fn outside the v1 splice scope
/// (planning/inline_bodies.md "v1 scope and deferrals").
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct InlineDiagnostic {
    pub span: Span,
    pub kind: InlineDiagnosticKind,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum InlineDiagnosticKind {
    /// Clocked state (`when` / `.reg`) in an inline body — its domain/clock
    /// generics would have to bind to the caller's clocks (deferred).
    Clocked,
    /// An `out` parameter on an inline body (no instance to carry the
    /// out-connection; deferred).
    OutParam { name: String },
    /// A `var` (cyclic-equation node) in an inline body — only `let`-style
    /// combinational bodies splice in v1.
    Var { name: String },
    /// An `integer`-typed value parameter — compile-time only, not a wire; v1
    /// binds value params as caller-side wires, so an integer param is deferred.
    IntegerParam { name: String },
    /// The inline fn calls itself (directly or transitively through other inline
    /// fns) — splicing would not terminate. Rejected cleanly here rather than at
    /// the backend splice's depth guard.
    Recursive,
}

impl InlineDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            InlineDiagnosticKind::Clocked => {
                "an `#[inline]` fn must be combinational: clocked state \
                 (`when` / `.reg`) in an inline body is not supported yet"
                    .to_owned()
            }
            InlineDiagnosticKind::OutParam { name } => {
                format!("an `#[inline]` fn cannot have an `out` parameter (`{name}`) yet")
            }
            InlineDiagnosticKind::Var { name } => {
                format!(
                    "an `#[inline]` fn cannot declare `var {name}` yet \
                     (only `let`-style combinational bodies splice)"
                )
            }
            InlineDiagnosticKind::IntegerParam { name } => {
                format!(
                    "an `#[inline]` fn cannot take an `integer` parameter (`{name}`) yet \
                     (value params splice as wires; an `integer` is compile-time only)"
                )
            }
            InlineDiagnosticKind::Recursive => {
                "an `#[inline]` fn cannot call itself (directly or through other \
                 inline fns) — splicing it would not terminate"
                    .to_owned()
            }
        }
    }
}

/// QUERY: validate that a Mirin-bodied `#[inline]` fn is within the v1 splice
/// scope — combinational, value-returning (planning/inline_bodies.md). This is
/// the home for the "not yet" restrictions: emission runs only on a
/// diagnostic-free crate, so a rejected shape never reaches the backend splice
/// (which would otherwise mis-thread a clock or panic). A verilog-bodied inline
/// (its body is trusted, contract = signature) and a non-inline fn are unchecked.
#[salsa::tracked(returns(ref))]
pub fn inline_check<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Vec<InlineDiagnostic> {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Vec::new();
    };
    if !data.inline || !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return Vec::new();
    }
    let body = body(db, krate, def);
    // A verilog-bodied inline splices its trusted template — no body shape to
    // validate (the signature is the contract).
    if body.verilog().is_some() {
        return Vec::new();
    }
    let sig = sig_of(db, krate, def);
    let mut out = Vec::new();

    // out-params: an inline body has no instance to carry an out-connection.
    for p in &sig.params {
        if p.direction == Some(Direction::Out) {
            out.push(InlineDiagnostic {
                span: body.local_span(p.local),
                kind: InlineDiagnosticKind::OutParam {
                    name: p.name.clone(),
                },
            });
        }
        // An `integer` value param is compile-time only — not a spliceable wire.
        if matches!(
            p.ty,
            Type::Value {
                kind: ValueKind::Integer,
                ..
            }
        ) {
            out.push(InlineDiagnostic {
                span: body.local_span(p.local),
                kind: InlineDiagnosticKind::IntegerParam {
                    name: p.name.clone(),
                },
            });
        }
    }

    // `var` nodes: only `let`-style combinational bodies splice in v1. The
    // synthetic result place (a `var`-kind local carrying `result_base`, from a
    // desugared whole-result equation) is not a user `var` — skip it.
    for (i, l) in body.locals().iter().enumerate() {
        if l.kind == LocalKind::Var && l.result_base.is_none() {
            out.push(InlineDiagnostic {
                span: body.local_span(LocalId(i as u32)),
                kind: InlineDiagnosticKind::Var {
                    name: l.name.clone(),
                },
            });
        }
    }

    // Clocked state: a value-form `when` or a `.reg` is an expr in the arena; a
    // statement-form `when` is a `Stmt::When` reached by walking the block tree.
    // (The span anchors at the def start — the body is small and the message is
    // unambiguous.)
    let clocked = body.exprs().any(|e| {
        matches!(&e.kind, ExprKind::When { .. })
            || matches!(&e.kind, ExprKind::MethodCall { method, .. } if method == "reg")
    }) || block_has_stmt_when(body.block());
    if clocked {
        out.push(InlineDiagnostic {
            span: Span::default(),
            kind: InlineDiagnosticKind::Clocked,
        });
    }

    // NB: a `const if` in an inline body is NOT rejected — whether it grounds is
    // a property of the *call site* (its const args), not the def, so a per-def
    // check cannot classify it (planning/slice_guards.md, decision 4). It folds at
    // the splice when the call grounds it; a still-symbolic one is the generate-if
    // case (Phase 4).

    // Inline recursion: a fn that calls itself (directly or transitively through
    // other inline fns) would splice forever. Reject up front.
    if inline_recurses(db, krate, def) {
        out.push(InlineDiagnostic {
            span: Span::default(),
            kind: InlineDiagnosticKind::Recursive,
        });
    }

    out
}

/// The direct, inline, Mirin-bodied callees of `def` (the calls that the backend
/// would splice). Only `Call { callee: Def(d) }` is followed — a method-dispatched
/// callee is exotic enough to leave to the backend's depth-guard backstop.
fn inline_mirin_callees<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Vec<DefId<'db>> {
    let map = crate_def_map(db, krate);
    let body = body(db, krate, def);
    let mut out = Vec::new();
    for e in body.exprs() {
        let ExprKind::Call { callee, .. } = &e.kind else {
            continue;
        };
        let ExprKind::Def(d) = body.expr(*callee).kind else {
            continue;
        };
        if map.def_data(d).is_some_and(|data| data.inline) && body_is_mirin(db, krate, d) {
            out.push(d);
        }
    }
    out
}

fn body_is_mirin<'db>(db: &'db dyn salsa::Database, krate: SourceRoot, def: DefId<'db>) -> bool {
    body(db, krate, def).verilog().is_none()
}

/// Does following inline-callee edges from `start` lead back to `start` (a splice
/// cycle)? A bounded DFS over the inline call graph.
fn inline_recurses<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    start: DefId<'db>,
) -> bool {
    let mut seen: std::collections::HashSet<DefId<'db>> = std::collections::HashSet::new();
    let mut stack = inline_mirin_callees(db, krate, start);
    while let Some(d) = stack.pop() {
        if d == start {
            return true;
        }
        if seen.insert(d) {
            stack.extend(inline_mirin_callees(db, krate, d));
        }
    }
    false
}

/// Is there a statement-form `when` anywhere in `block`'s tree?
fn block_has_stmt_when(block: &Block) -> bool {
    block.stmts.iter().any(|stmt| match stmt {
        Stmt::When { .. } => true,
        Stmt::For { body, .. } => block_has_stmt_when(body),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;
    use crate::nameres::ids::Namespace;

    fn load(db: &mut RootDatabase, vfs: &mut Vfs, text: &str) -> SourceRoot {
        vfs.set_file_text(db, "t.mrn", text);
        vfs.source_root(db, "t.mrn")
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
    fn slice_seg_range_is_direction_agnostic() {
        // vec low-first and bits high-first normalise to the same integer set.
        assert_eq!(seg_range("[0..2]"), Some((0, 2)));
        assert_eq!(seg_range("[8..0]"), Some((0, 8))); // bits high-first
        assert_eq!(seg_range("[3]"), Some((3, 4))); // bare index
        assert_eq!(seg_range("[2..+3]"), Some((2, 5))); // offset form
        assert_eq!(seg_range("[?..4]"), None); // runtime endpoint — not provable
        assert_eq!(seg_range("[2..]"), None); // elided endpoint — not provable
    }

    #[test]
    fn overlapping_slice_paths_conflict_disjoint_dont() {
        let p = |s: &str| vec![s.to_string()];
        assert!(paths_conflict(&p("[0..2]"), &p("[1..3]"))); // overlap at index 1
        assert!(!paths_conflict(&p("[0..2]"), &p("[2..4]"))); // disjoint (vec tiling)
        assert!(!paths_conflict(&p("[8..0]"), &p("[16..8]"))); // disjoint (bits tiling)
        assert!(paths_conflict(&p("[0..4]"), &p("[1]"))); // slice covers an index
        assert!(paths_conflict(&[], &p("[0..2]"))); // whole-place vs a slice (prefix)
        assert!(!paths_conflict(&["a".into()], &["b".into()])); // disjoint fields
        assert!(!paths_conflict(&p("[?..2]"), &p("[1..3]"))); // runtime → not provable
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
        // whose RHS is the `when` value (the example `when_counter.mrn`).
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

    fn inline_diags<'db>(
        db: &'db RootDatabase,
        krate: SourceRoot,
        name: &str,
    ) -> &'db Vec<InlineDiagnostic> {
        let map = crate_def_map(db, krate);
        let def = map
            .resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("def");
        inline_check(db, krate, def)
    }

    #[test]
    fn inline_check_passes_combinational_value_body() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(&mut db, &mut vfs, "#[inline]\nfn id(a: uint(8)) -> uint(8) { a }");
        assert!(inline_diags(&db, krate, "id").is_empty());
    }

    #[test]
    fn inline_check_rejects_var_and_integer_param() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "#[inline]\nfn f(a: uint(8), n: integer) -> uint(8) { var x: uint(8); x = a; x }",
        );
        let ds = inline_diags(&db, krate, "f");
        assert!(
            ds.iter()
                .any(|d| matches!(&d.kind, InlineDiagnosticKind::Var { name } if name == "x")),
            "{ds:?}"
        );
        assert!(
            ds.iter()
                .any(|d| matches!(&d.kind, InlineDiagnosticKind::IntegerParam { name } if name == "n")),
            "{ds:?}"
        );
    }

    #[test]
    fn inline_check_rejects_direct_and_indirect_recursion() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // direct: `f` calls `f`; indirect: `g` -> `h` -> `g`.
        let krate = load(
            &mut db,
            &mut vfs,
            "#[inline]\nfn f(a: uint(8)) -> uint(8) { f(a) }\n\
             #[inline]\nfn g(a: uint(8)) -> uint(8) { h(a) }\n\
             #[inline]\nfn h(a: uint(8)) -> uint(8) { g(a) }",
        );
        for name in ["f", "g", "h"] {
            assert!(
                inline_diags(&db, krate, name)
                    .iter()
                    .any(|d| matches!(d.kind, InlineDiagnosticKind::Recursive)),
                "{name}: {:?}",
                inline_diags(&db, krate, name)
            );
        }
    }

    #[test]
    fn inline_check_skips_non_inline_and_verilog_bodies() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // A non-inline fn with a `var` is fine; a verilog-bodied inline is trusted.
        let krate = load(
            &mut db,
            &mut vfs,
            "fn g(a: uint(8)) -> uint(8) { var x: uint(8); x = a; x }\n\
             #[inline]\nfn h(a: uint(8)) -> uint(8) = verilog { assign ${result} = ${a}; }",
        );
        assert!(inline_diags(&db, krate, "g").is_empty());
        assert!(inline_diags(&db, krate, "h").is_empty());
    }
}
