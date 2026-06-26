//! Monomorphisation-time checking (`planning/mono_check.md`) — first slice.
//!
//! The HIR/`infer` layer is deliberately loose on const-parameter-dependent
//! facts: a width mismatch between two symbolic widths (`uint(n)` vs `uint(m)`),
//! or a literal that may not fit a symbolic width, survives inference as a
//! *residual* rather than an error. `infer` records these (`const_residuals`,
//! `fit_residuals`); the backend's fallback is to emit them as elaboration-time
//! `initial assert`s.
//!
//! This pass pays that debt down at the **ground** boundary. A call site that
//! instantiates a generic callee with **literal** const args makes each of the
//! callee's residuals concrete — `n == m` becomes `8 == 4` — so we can simply
//! evaluate it and turn a violation into a real compile-time diagnostic at the
//! call, instead of deferring it to a silent sim-time assert.
//!
//! Obligations checked, each under the call's recorded subst (`MExpr`
//! `Call.substs`, the same per-call instantiation the backend renders as
//! `#(.n(8))`): width-equality residuals (`n == m`), literal-fit residuals
//! (`value` fits `width`), and width positivity (a parametric `uint(n - m)`
//! grounding `< 1`).
//!
//! Composition is **transitive (bounded N-level)**: the walk recurses through a
//! call chain, composing each call's subst into the next, so an obligation buried
//! N levels down that only grounds once the root's literals flow through is still
//! decided (`outer → inner → foo`). Soundness of the recursion: it dedups
//! instantiations (diamonds) by `(callee, composed subst)` — type args included,
//! since a type arg can drive a width via an assoc const — and bounds genuine
//! recursion (`f(n)` → `f(n-1)`) with a fuel cap. Inner calls already ground on
//! their own are left to the walk over the callee as a def (no double-report).
//! This is the correct-but-unfactored ground check; the assertion-map /
//! support-factoring design in `planning/mono_check.md` later cuts its *cost*
//! (loop-axis factoring, family dedup) **without changing the diagnostics**.
//!
//! Negative space: a residual/width that stays symbolic after substitution simply
//! does not fire (not a ground decision) — never a silent wrong pass, because the
//! symbolic `initial assert` still guards equality residuals downstream.

use std::collections::HashSet;

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::hir::const_eval::eval_const;
use crate::hir::infer::infer;
use crate::hir::sig::sig_of;
use crate::hir::types::{
    ConstArg, Folder, Term, Type, ValueKind, subst_const_opt, subst_type_opt, super_fold_const,
    super_fold_type,
};
use crate::mir::ir::MExprKind;
use crate::mir::lower::mir_of;
use crate::nameres::def_map::crate_def_map;
use crate::nameres::ids::{DefId, DefKind};

/// A monomorphisation-time diagnostic: a ground obligation decided false at an
/// instantiation. `def` is the **caller** (where the call site lives) and `span`
/// is the call's def-relative span, so the reporter lifts it like a body
/// diagnostic.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct MonoDiagnostic<'db> {
    pub def: DefId<'db>,
    pub span: Span,
    pub message: String,
}

impl<'db> MonoDiagnostic<'db> {
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// QUERY: ground-regime monomorphisation checks across the crate. Independent of
/// `sv_file`: it does NOT gate emission (a ground violation is a hard error to
/// the user, but the other modules still render). The caller (CLI/LSP) reports
/// these alongside the front-end diagnostics.
///
/// Coverage: **transitive (bounded N-level)** call composition. For each call
/// site `D → C(args)`, [`compose_check`] checks `C`'s obligations under `args`,
/// then recurses through `C`'s inner calls that `args` grounds further, composing
/// substs down the chain (`outer → inner → foo`). Diamonds dedup by
/// `(callee, composed subst)`; a fuel cap bounds recursion. The cost-factoring
/// (assertion maps, support factoring) remains the destination in
/// `planning/mono_check.md` — it lowers cost, not the diagnostics.
#[salsa::tracked(returns(ref))]
pub fn mono_check<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
) -> Vec<MonoDiagnostic<'db>> {
    let map = crate_def_map(db, krate);
    let mut out = Vec::new();
    for def in map.defs().collect::<Vec<_>>() {
        if !matches!(
            map.def_data(def).map(|d| d.kind),
            Some(DefKind::Fn | DefKind::Method)
        ) {
            continue;
        }
        for mexpr in mir_of(db, krate, def).exprs() {
            let MExprKind::Call { callee, substs, .. } = &mexpr.kind else {
                continue;
            };
            let span = mexpr.span;
            // Walk the ground instantiation tree rooted at this call: check the
            // callee's obligations under this subst, then recurse into its inner
            // calls that this subst grounds further (transitive composition).
            let subst: Vec<Option<Term<'db>>> = substs.iter().cloned().map(Some).collect();
            let mut budget = COMPOSE_BUDGET;
            compose_check(
                db,
                krate,
                *callee,
                &subst,
                def,
                span,
                COMPOSE_FUEL,
                &mut budget,
                &mut out,
            );
        }
    }
    // Distinct paths can reach the same obligation at the same call; dedup.
    out.sort_by(|a, b| {
        (a.span.start, a.span.end, &a.message).cmp(&(b.span.start, b.span.end, &b.message))
    });
    out.dedup();
    out
}

/// Per-path depth bound for transitive composition — turns an accidental
/// recursive chain (`f(n)` → `f(n - 1)` → …) into a bounded descent rather than a
/// hang. Real generic call trees are far shallower.
const COMPOSE_FUEL: u32 = 16;
/// Shared total-work bound per root call — caps a diamond (many paths to the same
/// instantiation) without needing a hashable dedup key. Huge relative to any real
/// generic call graph; if ever exhausted, the uncovered deep obligations simply
/// fall back to the symbolic `initial assert` (sound — incompleteness, not a
/// wrong pass).
const COMPOSE_BUDGET: u32 = 10_000;

/// Walk the ground instantiation tree rooted at a call `→ callee(subst)`: check
/// `callee`'s own obligations under `subst`, then recurse into each inner call
/// `callee → inner(iargs)` whose `iargs` are *not* already self-ground (those are
/// covered when `callee` is walked as a def — recursing them would double-report),
/// composing `subst` into `iargs` so an obligation buried N levels down that only
/// grounds once this root's literals flow through is still decided.
///
/// Bounds: `fuel` caps descent depth (recursion), `budget` caps total work
/// (diamonds) — together they make the walk terminate on any graph without a
/// per-instantiation dedup key. A still-symbolic composition decides nothing; a
/// duplicate diagnostic from two paths is removed by the caller's final dedup.
#[allow(clippy::too_many_arguments)]
fn compose_check<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    callee: DefId<'db>,
    subst: &[Option<Term<'db>>],
    report_def: DefId<'db>,
    span: Span,
    fuel: u32,
    budget: &mut u32,
    out: &mut Vec<MonoDiagnostic<'db>>,
) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    check_obligations(db, krate, callee, subst, report_def, span, out);
    if fuel == 0 {
        return;
    }
    for inner in mir_of(db, krate, callee).exprs() {
        let MExprKind::Call {
            callee: inner_callee,
            substs: inner_substs,
            ..
        } = &inner.kind
        else {
            continue;
        };
        // Already ground in `callee`'s own frame → handled when `callee` is walked
        // as a def; composing here would only double-report.
        if consts_closed(inner_substs) {
            continue;
        }
        let composed: Vec<Option<Term<'db>>> = inner_substs
            .iter()
            .map(|t| Some(compose_term(t, subst)))
            .collect();
        compose_check(
            db,
            krate,
            *inner_callee,
            &composed,
            report_def,
            span,
            fuel - 1,
            budget,
            out,
        );
    }
}

/// Check a callee's deferred obligations under a (possibly composed) subst,
/// reporting any ground violation at `report_def`/`span`. Each check self-filters
/// via [`is_closed`], so a still-symbolic instantiation simply produces nothing.
fn check_obligations<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    callee: DefId<'db>,
    subst: &[Option<Term<'db>>],
    report_def: DefId<'db>,
    span: Span,
    out: &mut Vec<MonoDiagnostic<'db>>,
) {
    let inf = infer(db, krate, callee);
    let residuals = inf.const_residuals();
    let fits = inf.fit_residuals();
    let any_bound = subst.iter().any(Option::is_some);
    if residuals.is_empty() && fits.is_empty() && !any_bound {
        return;
    }

    let name = crate_def_map(db, krate)
        .def_data(callee)
        .map(|d| d.name.clone())
        .unwrap_or_default();
    let mut report = |message: String| {
        out.push(MonoDiagnostic {
            def: report_def,
            span,
            message,
        });
    };

    // Width-equality residuals (`uint(a)` vs `uint(b)`): ground both, compare.
    for (a, b) in residuals {
        let a = subst_const_opt(a, subst);
        let b = subst_const_opt(b, subst);
        if is_closed(&a)
            && is_closed(&b)
            && let (Some(va), Some(vb)) = (
                eval_const(db, krate, callee, &a),
                eval_const(db, krate, callee, &b),
            )
            && va != vb
        {
            report(format!(
                "instantiating `{name}`: mismatched widths ({va} != {vb})"
            ));
        }
    }

    // Literal-fit residuals (`value` must fit `width` bits): ground the width,
    // check the literal fits — sign-aware, mirroring infer's ground bounds
    // (`sint`: `-2^(w-1) ..< 2^(w-1)`; `uint`/`bits`: `0 ..< 2^w`). A width `< 1`
    // is the positivity check's job; a width `>= 127` is left to the fallback (no
    // i128 shift to decide it with).
    for fit in fits {
        let width = subst_const_opt(&fit.width, subst);
        if is_closed(&width)
            && let Some(w) = eval_const(db, krate, callee, &width)
            && (1..127).contains(&w)
        {
            let fits = if fit.signed {
                let half = 1i128 << (w - 1);
                fit.value >= -half && fit.value < half
            } else {
                fit.value >= 0 && fit.value < (1i128 << w)
            };
            if !fits {
                let ty = if fit.signed { "sint" } else { "uint" };
                report(format!(
                    "instantiating `{name}`: literal {} does not fit in {ty}({w})",
                    fit.value
                ));
            }
        }
    }

    // Width positivity. A parametric width/len (`uint(n - m)`, `Vec(k, …)`) that
    // grounds to `< 0` at this instantiation is invalid SV (`logic [-5:0]`; width
    // 0 is the legal effective-0-bit). infer defers parametric widths, so this is
    // their ground decision. Covers the
    // callee signature's own widths (param/return, nested through Vec/Tuple) —
    // struct/port *field* widths resolve elsewhere and are not walked here yet.
    // Dedup by value so repeated widths report once per call.
    if any_bound {
        let sig = sig_of(db, krate, callee);
        let tys = sig
            .params
            .iter()
            .map(|p| &p.ty)
            .chain(sig.return_type.as_ref());
        let mut reported: HashSet<i128> = HashSet::new();
        let mut failed_reported = false;
        for ty in tys {
            let mut collector = WidthCollector(Vec::new());
            collector.fold_type(&subst_type_opt(ty, subst));
            for w in &collector.0 {
                if !is_closed(w) {
                    continue;
                }
                match eval_const(db, krate, callee, w) {
                    // Width 0 is legal (the effective-0-bit `[-1:0]`, the basis of
                    // the zero-width guards — planning/slicing.md); only a negative
                    // width is invalid. Matches infer's `check_widths` (`< 0`).
                    Some(v) if v < 0 && reported.insert(v) => report(format!(
                        "instantiating `{name}`: width evaluates to {v} (must be >= 0)"
                    )),
                    Some(_) => {}
                    // Closed but unevaluable ⇒ a genuine arithmetic failure
                    // (divide-by-zero or i128 overflow) — `arith` only returns
                    // `None` for those, and a symbolic part would fail `is_closed`.
                    None if !failed_reported => {
                        failed_reported = true;
                        report(format!(
                            "instantiating `{name}`: width is not a valid constant \
                             (division by zero or overflow)"
                        ));
                    }
                    None => {}
                }
            }
        }
    }
}

/// Is a recorded inner call already ground on its own (independent of any
/// enclosing instantiation)? If so, depth-1 skips it — the walk over the callee
/// as a def covers it, so composing would only double-report. A const entry must
/// be closed; a *type* entry must carry no `Param` (a type param can drive a
/// width via an assoc const, so a type-grounding wrapper must NOT be skipped).
fn consts_closed(substs: &[Term<'_>]) -> bool {
    substs.iter().all(|t| match t {
        Term::Const(c) => is_closed(c),
        Term::Type(ty) => !type_has_param(ty),
        Term::Domain(_) => true,
    })
}

/// Does a type carry any generic `Param` (type- or const-position)? Used to tell
/// a self-ground type arg from one that an enclosing instantiation still grounds.
fn type_has_param(ty: &Type<'_>) -> bool {
    struct Scan(bool);
    impl<'db> Folder<'db> for Scan {
        fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
            if matches!(
                t,
                Type::Value {
                    kind: ValueKind::Param(_),
                    ..
                }
            ) {
                self.0 = true;
            }
            super_fold_type(self, t)
        }
        fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
            if matches!(c, ConstArg::Param(_)) {
                self.0 = true;
            }
            super_fold_const(self, c)
        }
    }
    let mut s = Scan(false);
    s.fold_type(ty);
    s.0
}

/// Substitute an enclosing instantiation's `subst` into one of a callee's
/// recorded subst terms (composition for depth-1). Const/Type terms fold through;
/// a Domain term is irrelevant to const obligations and passes through.
fn compose_term<'db>(t: &Term<'db>, subst: &[Option<Term<'db>>]) -> Term<'db> {
    match t {
        Term::Const(c) => Term::Const(subst_const_opt(c, subst)),
        Term::Type(ty) => Term::Type(subst_type_opt(ty, subst)),
        Term::Domain(d) => Term::Domain(d.clone()),
    }
}

/// Is a `ConstArg` closed — evaluable to a literal with **no** frame (only
/// `Lit` and arithmetic over closed operands)? After substituting a call's args,
/// only a closed expr is safely ground: anything still carrying a `Param` /
/// `Local` / `Assoc` / `Field` needs a frame to resolve, and a substituted-in
/// *caller* `Local` would resolve against the wrong (callee) frame — so we defer
/// it rather than risk a misframed eval. This is exactly the ground-literal
/// regime mono_check decides; the rest stays the `initial assert` fallback's job.
fn is_closed(c: &ConstArg<'_>) -> bool {
    match c {
        ConstArg::Lit(_) => true,
        ConstArg::Op(_, a, b) => is_closed(a) && is_closed(b),
        _ => false,
    }
}

/// Collects the width/length [`ConstArg`]s at every scalar/vec position of a
/// type (recursing through `Vec`/`Tuple`/`Port` args via `super_fold_type`).
struct WidthCollector<'db>(Vec<ConstArg<'db>>);

impl<'db> Folder<'db> for WidthCollector<'db> {
    fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
        match t {
            Type::Value { kind, .. } => match kind {
                ValueKind::UInt { width }
                | ValueKind::SInt { width }
                | ValueKind::Bits { width } => self.0.push(width.clone()),
                _ => {}
            },
            Type::Vec { len, .. } => self.0.push(len.clone()),
            _ => {}
        }
        super_fold_type(self, t)
    }
}
