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
//! First slice (deliberately naive — see the doc's scaling design for the
//! assertion-map/support-factoring that optimises this later *without changing
//! the diagnostics*):
//! - **Direct call sites only.** Each `Call` is checked against its immediate
//!   callee's residuals under the call's recorded subst (`MExpr` `Call.substs`,
//!   the same per-call instantiation the backend renders as `#(.n(8))`). A
//!   transitive obligation (callee calls a generic with the caller's param) only
//!   grounds once the enclosing call is itself literal — that cross-module
//!   *composition* is future work.
//! - **Ground only.** A residual that stays symbolic after substitution (a
//!   non-literal arg) is left to the existing `initial assert` fallback.
//!
//! Negative space: a residual shape we cannot evaluate after substitution simply
//! does not fire here (it is not a ground decision) — never a silent wrong pass,
//! because the symbolic `initial assert` still guards it downstream.

use std::collections::HashSet;

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::hir::const_eval::eval_const;
use crate::hir::infer::infer;
use crate::hir::sig::sig_of;
use crate::hir::types::{
    ConstArg, Folder, Term, Type, ValueKind, subst_const_opt, subst_type_opt, super_fold_type,
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
#[salsa::tracked(returns(ref))]
pub fn mono_check<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
) -> Vec<MonoDiagnostic<'db>> {
    let map = crate_def_map(db, krate);
    let mut out = Vec::new();
    for def in map.defs().collect::<Vec<_>>() {
        match map.def_data(def).map(|d| d.kind) {
            Some(DefKind::Fn | DefKind::Method) => {}
            _ => continue,
        }
        let mir = mir_of(db, krate, def);
        for mexpr in mir.exprs() {
            if let MExprKind::Call { callee, substs, .. } = &mexpr.kind {
                check_call(db, krate, def, *callee, substs, mexpr.span, &mut out);
            }
        }
    }
    out
}

/// Check one call site's immediate callee residuals under the call's subst.
fn check_call<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    caller: DefId<'db>,
    callee: DefId<'db>,
    substs: &[Term<'db>],
    span: Span,
    out: &mut Vec<MonoDiagnostic<'db>>,
) {
    let inf = infer(db, krate, callee);
    let residuals = inf.const_residuals();
    let fits = inf.fit_residuals();
    // Nothing to ground: a monomorphic callee (no subst) whose widths are already
    // literal and infer-checked, with no deferred obligations.
    if substs.is_empty() && residuals.is_empty() && fits.is_empty() {
        return;
    }
    // `Call.substs` is in the callee's declared generic-param order, the same
    // index `ConstArg::Param(i)` uses. Lift to the `Option`-subst the folder
    // wants; a Param with no entry stays symbolic and simply will not ground.
    let subst: Vec<Option<Term<'db>>> = substs.iter().cloned().map(Some).collect();

    let name = crate_def_map(db, krate)
        .def_data(callee)
        .map(|d| d.name.clone())
        .unwrap_or_default();

    // Width-equality residuals (`uint(a)` vs `uint(b)`): ground both, compare.
    for (a, b) in residuals {
        let a = subst_const_opt(a, &subst);
        let b = subst_const_opt(b, &subst);
        if is_closed(&a)
            && is_closed(&b)
            && let (Some(va), Some(vb)) = (
                eval_const(db, krate, callee, &a),
                eval_const(db, krate, callee, &b),
            )
            && va != vb
        {
            out.push(MonoDiagnostic {
                def: caller,
                span,
                message: format!("instantiating `{name}`: mismatched widths ({va} != {vb})"),
            });
        }
    }

    // Literal-fit residuals (`value` must fit in `width` bits): ground the
    // width, check the literal fits. A width outside `[0, 127)` is left to the
    // symbolic fallback (no i128 shift to decide it with).
    for fit in fits {
        let width = subst_const_opt(&fit.width, &subst);
        if is_closed(&width)
            && let Some(w) = eval_const(db, krate, callee, &width)
            && (0..127).contains(&w)
            && fit.value >= (1i128 << w)
        {
            out.push(MonoDiagnostic {
                def: caller,
                span,
                message: format!(
                    "instantiating `{name}`: literal {} does not fit in width {w}",
                    fit.value
                ),
            });
        }
    }

    // Width positivity. A parametric width/len (`uint(n - m)`, `Vec(k, …)`) that
    // grounds to `< 1` at this instantiation is invalid SV (`logic [-5:0]`).
    // infer defers parametric widths, so this is their ground decision; a
    // monomorphic callee's widths are already literal and infer-checked, so skip.
    // Covers the callee signature's own widths (param/return, nested through
    // Vec/Tuple) — struct/port *field* widths resolve elsewhere and are not
    // walked here yet. Dedup by value so repeated widths report once per call.
    if !substs.is_empty() {
        let sig = sig_of(db, krate, callee);
        let tys = sig
            .params
            .iter()
            .map(|p| &p.ty)
            .chain(sig.return_type.as_ref());
        let mut reported: HashSet<i128> = HashSet::new();
        for ty in tys {
            let mut collector = WidthCollector(Vec::new());
            collector.fold_type(&subst_type_opt(ty, &subst));
            for w in &collector.0 {
                if is_closed(w)
                    && let Some(v) = eval_const(db, krate, callee, w)
                    && v < 1
                    && reported.insert(v)
                {
                    out.push(MonoDiagnostic {
                        def: caller,
                        span,
                        message: format!(
                            "instantiating `{name}`: width evaluates to {v} (must be >= 1)"
                        ),
                    });
                }
            }
        }
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
