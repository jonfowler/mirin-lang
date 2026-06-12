//! Sum-of-monomials normal form for compile-time integer expressions.
//!
//! A linear-arithmetic canonical form for widths and other const-typed
//! expressions: a single `i64` constant plus a sorted, deduplicated list
//! of `(coefficient, variable)` terms. Two normalised expressions are
//! equal iff their fields are identically equal — sort order makes the
//! comparison structural, so `M + N` and `N + M` both normalise to the
//! same value.
//!
//! What this gets us:
//!
//! - `M + N` and `N + M` collapse to one form.
//! - `N + N` and `2 * N` collapse to one form.
//! - `N + 1 + 1` and `N + 2` collapse to one form.
//! - Phase B's `unify_widths` ground equality check goes through this so
//!   the relations above are detected as equal even though their
//!   `HirExpr` trees differ.
//!
//! What it doesn't get us:
//!
//! - Multiplication by non-constants (`M * N`) stays as an opaque term —
//!   we'd need a different representation (e.g. multivariate
//!   polynomials). Falls out of the linear-form assumption.
//! - Division, modulo, comparisons. Out of scope.
//!
//! Operations on a width that the normaliser can't fold (e.g. `M * N`)
//! produce a `None` result; callers fall back to treating the expression
//! as opaque (a single term with coefficient 1 and an opaque variable
//! reference).

use crate::hir::{ConstValue, HirExpr, HirExprKind, LocalId};

/// One generic-param-shaped variable in a normalised expression. Each
/// kind matches an `HirExprKind` variant that can appear in a width slot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NormalVar {
    /// Reference to the enclosing item's `i`-th generic param (Const-kind).
    Param(u32),
    /// A const inference variable from typeck's `const_vars` pool.
    ConstVar(u32),
    /// A regular local binding referenced in a width (rare; typically a
    /// fn `param N: usize` slot before lowering rewrites it to `Param`).
    Local(LocalId),
}

/// Sum-of-monomials canonical form: `constant + Σ(coeff_i · var_i)`.
/// The `terms` vec is sorted by `(NormalVar)` and deduplicated by var
/// (coefficients on the same var fold together); zero-coefficient terms
/// are dropped. Equality is structural — `==` says the two values are
/// algebraically equal in the linear-arithmetic theory we support.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NormalConst {
    pub constant: i64,
    pub terms: Vec<(i64, NormalVar)>,
}

impl NormalConst {
    pub fn constant(c: i64) -> Self {
        Self {
            constant: c,
            terms: Vec::new(),
        }
    }

    pub fn var(v: NormalVar) -> Self {
        Self {
            constant: 0,
            terms: vec![(1, v)],
        }
    }

    /// `self + other`. Always succeeds (linear sum stays linear).
    pub fn add(mut self, other: Self) -> Self {
        self.constant = self.constant.wrapping_add(other.constant);
        for (coeff, var) in other.terms {
            self.add_term(coeff, var);
        }
        self.canonicalise();
        self
    }

    /// `self - other`. Always succeeds.
    pub fn sub(mut self, other: Self) -> Self {
        self.constant = self.constant.wrapping_sub(other.constant);
        for (coeff, var) in other.terms {
            self.add_term(coeff.wrapping_neg(), var);
        }
        self.canonicalise();
        self
    }

    /// `self * other`. Succeeds only when one side is a pure constant
    /// (linear arithmetic: constant times polynomial). Returns `None`
    /// when both sides have variable terms — e.g. `M * N` — since the
    /// result is no longer linear.
    pub fn mul(self, other: Self) -> Option<Self> {
        let (scalar, poly) = if self.terms.is_empty() {
            (self.constant, other)
        } else if other.terms.is_empty() {
            (other.constant, self)
        } else {
            return None;
        };
        let mut out = NormalConst {
            constant: poly.constant.wrapping_mul(scalar),
            terms: poly
                .terms
                .into_iter()
                .map(|(c, v)| (c.wrapping_mul(scalar), v))
                .collect(),
        };
        out.canonicalise();
        Some(out)
    }

    fn add_term(&mut self, coeff: i64, var: NormalVar) {
        // Linear scan suffices — typical width expressions have ≤ a few
        // distinct vars. Fold into an existing entry if present.
        for entry in self.terms.iter_mut() {
            if entry.1 == var {
                entry.0 = entry.0.wrapping_add(coeff);
                return;
            }
        }
        self.terms.push((coeff, var));
    }

    fn canonicalise(&mut self) {
        self.terms.retain(|(c, _)| *c != 0);
        self.terms.sort_by(|a, b| a.1.cmp(&b.1));
    }

    /// `true` iff this normal form has no variable terms — purely a
    /// constant integer. Use to gate the immediate-error check in
    /// obligation discharge (both sides ground + unequal → false).
    pub fn is_ground(&self) -> bool {
        self.terms.is_empty()
    }

    /// Substitute each variable through the supplied callback, then
    /// re-normalise. `resolve` returns `Some(replacement)` to substitute
    /// or `None` to leave the variable in place. Idempotent: a fully
    /// resolved expression returns itself unchanged.
    pub fn simplify(&self, resolve: &mut dyn FnMut(&NormalVar) -> Option<NormalConst>) -> Self {
        let mut out = NormalConst::constant(self.constant);
        for (coeff, var) in &self.terms {
            match resolve(var) {
                Some(replacement) => {
                    // Multiply replacement by coeff and add into out.
                    // `mul` returns None only for multivar * multivar; we
                    // build `coeff * replacement` which is constant * poly,
                    // so it always succeeds.
                    let scaled = NormalConst::constant(*coeff)
                        .mul(replacement)
                        .expect("constant * polynomial is always linear");
                    out = out.add(scaled);
                }
                None => out.add_term(*coeff, var.clone()),
            }
        }
        out.canonicalise();
        out
    }
}

/// Try to convert an `HirExpr` to a `NormalConst`. Recognises bare
/// literals, `Param`, `ConstVar`, and `Local` references; everything
/// else (calls, field accesses, etc.) returns `None`. When width
/// arithmetic (`+`, `-`, `*` in widths) reaches HIR, this function
/// extends to recurse on those structures.
pub fn normalise(expr: &HirExpr) -> Option<NormalConst> {
    match &expr.kind {
        HirExprKind::Const(ConstValue::Integer(n)) => Some(NormalConst::constant(*n as i64)),
        HirExprKind::Param(i) => Some(NormalConst::var(NormalVar::Param(*i))),
        HirExprKind::ConstVar(i) => Some(NormalConst::var(NormalVar::ConstVar(*i))),
        HirExprKind::Local(l) => Some(NormalConst::var(NormalVar::Local(*l))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceSpan;
    use crate::hir::HirId;

    fn span() -> SourceSpan {
        SourceSpan {
            start_byte: 0,
            end_byte: 0,
            start: crate::SourcePosition { row: 0, column: 0 },
            end: crate::SourcePosition { row: 0, column: 0 },
        }
    }

    fn lit(n: u64) -> HirExpr {
        HirExpr {
            kind: HirExprKind::Const(ConstValue::Integer(n)),
            ty: None,
            span: span(),
            id: HirId(0),
        }
    }

    fn param(i: u32) -> HirExpr {
        HirExpr {
            kind: HirExprKind::Param(i),
            ty: None,
            span: span(),
            id: HirId(0),
        }
    }

    #[test]
    fn constant_round_trip() {
        let nc = normalise(&lit(5)).unwrap();
        assert_eq!(nc, NormalConst::constant(5));
    }

    #[test]
    fn param_round_trip() {
        let nc = normalise(&param(2)).unwrap();
        assert_eq!(nc, NormalConst::var(NormalVar::Param(2)));
    }

    #[test]
    fn sum_commutes() {
        let a = NormalConst::var(NormalVar::Param(0)).add(NormalConst::var(NormalVar::Param(1)));
        let b = NormalConst::var(NormalVar::Param(1)).add(NormalConst::var(NormalVar::Param(0)));
        assert_eq!(a, b);
    }

    #[test]
    fn coefficient_folds() {
        let a = NormalConst::var(NormalVar::Param(0)).add(NormalConst::var(NormalVar::Param(0)));
        // 2 * Param(0) is a single term with coefficient 2.
        assert_eq!(
            a,
            NormalConst {
                constant: 0,
                terms: vec![(2, NormalVar::Param(0))],
            }
        );
        let two = NormalConst::constant(2);
        let scaled = two.mul(NormalConst::var(NormalVar::Param(0))).unwrap();
        assert_eq!(a, scaled);
    }

    #[test]
    fn constant_folds() {
        let a = NormalConst::var(NormalVar::Param(0))
            .add(NormalConst::constant(1))
            .add(NormalConst::constant(1));
        assert_eq!(
            a,
            NormalConst {
                constant: 2,
                terms: vec![(1, NormalVar::Param(0))],
            }
        );
    }

    #[test]
    fn zero_coefficient_drops() {
        let a = NormalConst::var(NormalVar::Param(0)).sub(NormalConst::var(NormalVar::Param(0)));
        assert_eq!(a, NormalConst::default());
    }

    #[test]
    fn mul_nonlinear_fails() {
        let p0 = NormalConst::var(NormalVar::Param(0));
        let p1 = NormalConst::var(NormalVar::Param(1));
        assert!(p0.mul(p1).is_none());
    }
}
