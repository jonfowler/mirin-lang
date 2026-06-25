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

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::hir::const_eval::eval_const;
use crate::hir::infer::infer;
use crate::hir::types::{Term, subst_const_opt};
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
    if residuals.is_empty() && fits.is_empty() {
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
        if let (Some(va), Some(vb)) = (
            eval_const(db, krate, callee, &a),
            eval_const(db, krate, callee, &b),
        ) && va != vb
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
        if let Some(w) = eval_const(db, krate, callee, &width)
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
}
