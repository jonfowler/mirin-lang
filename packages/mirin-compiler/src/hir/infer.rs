//! `infer(def)` — type + domain inference (`planning/q3_typed_hir.md` §2, §6.4).
//!
//! An eager-unification walk over a function's [`body`](crate::hir::body), the
//! per-fn `InferCtxt` of the old `typeck` lifted onto a query. Produces a type
//! for every expression and local, the resolved callee of every method call, and
//! diagnostics. Depends on `body(self)`, `sig_of(self)`, and `sig_of` of the
//! callees/structs/ports it touches — **never their bodies**, so a caller
//! re-infers only when a callee's *signature* changes (the firewall).
//!
//! Per `domain_checking.md`, the **domain is a component of the type**,
//! inferred by the same walk: `unify` is strict on domains; the lattice's one
//! edge (`@const` below every clock) applies only through `subsume` at the
//! coercion sites and through the join in `merge_branch`. Domain variables are
//! sorted (`Clock` vs `Domain` — registers demand `Clock`); an unconstrained
//! domain variable defaults to `@const`. It is not a parallel solve.
//!
//! **Scope:** structural-kind + domain inference for the monomorphic core, with
//! generic callees instantiated by substituting their `Param`s with fresh
//! variables. **Widths are checked** (Q4a): a literal's width and a Const-kind
//! generic both infer through the one kinded variable table, and two ground literal widths
//! that disagree are a `WidthMismatch`. Symbolic widths — generic params,
//! arithmetic, anon-consts — are accepted here and deferred to the residual +
//! `const_eval` machinery (Q4b/c, `planning/q4_const_eval.md`).

use std::collections::HashMap;

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::hir::body::{Body, ConnArg, ExprId, ExprKind, NamedArg, Stmt, body};
use crate::hir::sig::sig_of;
use crate::hir::types::{
    ConstArg, ConstOp, Direction, Domain, DomainSort, Folder, GenericArgs, GenericParam, InferVar,
    LocalId, Predicate, Term, TermKind, Type, ValueKind, super_fold_const, super_fold_type,
    type_has_infer,
};
use crate::nameres::def_map::{CrateDefMap, crate_def_map};
use crate::nameres::ids::{DefId, DefKind, Namespace};

/// A type/domain mismatch or unresolved method, with the def-relative span of
/// the expression under inference when it was found.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct InferDiagnostic {
    pub span: Span,
    pub kind: InferDiagnosticKind,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum InferDiagnosticKind {
    /// A `const if` whose condition carries a clock domain — i.e. depends on
    /// runtime data. The condition is resolved at elaboration, so it must be a
    /// constant (planning/comptime_if.md).
    ConstIfRuntimeCond,
    /// A slice expression (`x[lo..hi]` / `x[off..+w]`) — parsed and lowered, but
    /// the semantics (planning/slicing.md) are not yet implemented. Rejected
    /// cleanly here so a slice never silently lowers to its base.
    SliceNotImplemented,
    /// A slice whose constant high endpoint (or `offset + width`) exceeds the
    /// base length `N` — out of range. (Symbolic-but-grounding bounds defer to
    /// `mono_check`; this is the eager const check, planning/slicing.md.)
    SliceOutOfBounds {
        high: i128,
        len: i128,
    },
    /// Two types that had to be equal could not be unified.
    TypeMismatch,
    /// Two `uint` widths that had to be equal are different (`uint(8)` vs
    /// `uint(16)`, or two distinct width params).
    WidthMismatch,
    /// Two concrete clock domains that had to match did not.
    DomainMismatch,
    /// A `@const` value where an edge-bearing clock domain is required (the
    /// domain variable has sort `Clock` — e.g. a register's clock).
    RequiresClock,
    /// A `.reg` call that doesn't match the builtin's signature
    /// `(rstn: Reset @ D, init: T @const)`.
    RegForm,
    /// A clocked value used in const position (a `uint(n)` width).
    ClockedWidth,
    /// A `recv.method(…)` whose method did not resolve on the receiver's type.
    UnresolvedMethod {
        name: String,
    },
    AmbiguousMethod {
        name: String,
        traits: Vec<String>,
    },
    UnsatisfiedBound {
        ty_name: String,
        trait_name: String,
    },
    AmbiguousImpls {
        ty_name: String,
        trait_name: String,
    },
    BoundOverflow {
        trait_name: String,
    },
    CannotInferBound {
        trait_name: String,
    },
    LiteralDoesNotFit {
        value: i128,
        width: i128,
    },
    LiteralDoesNotFitS {
        value: i128,
        width: i128,
    },
    LiteralBadType {
        ty_name: String,
    },
    WidthNotInteger {
        ty_name: String,
    },
    NotIndexable {
        ty_name: String,
    },
    NotIterable {
        ty_name: String,
    },
    ForConstVec,
    BadIndexType {
        ty_name: String,
    },
    IndexOutOfBounds {
        index: i128,
        len: i128,
    },
    /// A uint width whose const evaluation came out negative.
    NegativeWidth {
        value: i128,
    },
    /// A *closed* width (no generic params / inference vars left) that still has
    /// no value — e.g. divide-by-zero or overflow in the width expression.
    UnevaluableWidth,
    /// A width whose expression is outside the representable const fragment —
    /// a call (or other non-const construct) written directly in a type
    /// position (`uint(f(n))`). Bind it first: `let w = f(n); uint(w)`.
    DeferredWidth,
    /// A record-constructor connector that disagrees with the field's
    /// declared direction (`=` for supplied fields, `=>` for `in` fields).
    RecordConnector {
        name: String,
        needs_arrow: bool,
    },
    /// A call whose positional argument count can't match the callee's
    /// positional params (`expected` is the count that failed: the total when
    /// over-supplied, the no-default minimum when under-supplied).
    PositionalArity {
        callee: String,
        expected: usize,
        found: usize,
    },
    /// A record constructor wrote a field its struct/port doesn't declare, or
    /// a field access named no field of the receiver's type.
    UnknownField {
        name: String,
    },
    /// A record constructor omitted a declared field.
    MissingField {
        name: String,
    },
    /// A record constructor wrote the same field twice.
    DuplicateField {
        name: String,
    },
    /// Field access on a (resolved) type that has no fields.
    FieldOnNonAggregate {
        name: String,
    },
    /// `p.2` on a 2-tuple — the projection index is past the arity.
    TupleIndexOutOfBounds {
        index: usize,
        arity: usize,
    },
    /// Tuples of different arities met at a unification site.
    TupleArityMismatch {
        expected: usize,
        found: usize,
    },
}

impl InferDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            InferDiagnosticKind::ConstIfRuntimeCond => {
                "a `const if` condition must be constant, but this one depends on \
                 runtime data (it carries a clock domain)"
                    .to_owned()
            }
            InferDiagnosticKind::SliceNotImplemented => {
                "unsupported slice: the width must be a positive compile-time constant \
                 (a literal, const generic, or const-folding expression) over a `bits`/`Vec` \
                 base — for a runtime offset use `x[off +: width]` (planning/slicing.md)"
                    .to_owned()
            }
            InferDiagnosticKind::SliceOutOfBounds { high, len } => {
                format!(
                    "slice out of range: the high endpoint {high} exceeds the base length \
                     {len} (a slice is `x[low..high]` with `high <= N`)"
                )
            }
            InferDiagnosticKind::TypeMismatch => "type mismatch".to_owned(),
            InferDiagnosticKind::WidthMismatch => "mismatched `uint` widths".to_owned(),
            InferDiagnosticKind::DomainMismatch => "mismatched clock domains".to_owned(),
            InferDiagnosticKind::RequiresClock => {
                "a clock domain is required here, but the domain is `@const`".to_owned()
            }
            InferDiagnosticKind::RegForm => "`.reg` expects `(reset, init)` arguments".to_owned(),
            InferDiagnosticKind::ClockedWidth => {
                "a width must be a compile-time constant, but this value is clocked".to_owned()
            }
            InferDiagnosticKind::UnsatisfiedBound {
                ty_name,
                trait_name,
            } => {
                format!("`{ty_name}` does not implement `{trait_name}`")
            }
            InferDiagnosticKind::AmbiguousImpls {
                ty_name,
                trait_name,
            } => {
                format!("multiple impls of `{trait_name}` match `{ty_name}`")
            }
            InferDiagnosticKind::BoundOverflow { trait_name } => {
                format!("overflow checking `{trait_name}` bound (recursion limit)")
            }
            InferDiagnosticKind::LiteralDoesNotFit { value, width } => {
                format!("`{value}` does not fit `uint({width})`")
            }
            InferDiagnosticKind::LiteralDoesNotFitS { value, width } => {
                format!("`{value}` does not fit `sint({width})`")
            }
            InferDiagnosticKind::LiteralBadType { ty_name } => {
                format!("a numeric literal cannot have type `{ty_name}`")
            }
            InferDiagnosticKind::WidthNotInteger { ty_name } => {
                format!(
                    "widths take `integer` values, found `{ty_name}` \
                     (hardware arithmetic wraps; compile-time arithmetic must not)"
                )
            }
            InferDiagnosticKind::ForConstVec => {
                "only `range(n)` iterates compile-time integers (a general const \
                 vector's values cannot drive the genvar)"
                    .to_owned()
            }
            InferDiagnosticKind::NotIterable { ty_name } => {
                format!("`{ty_name}` cannot be iterated — `for` takes a Vec or bits")
            }
            InferDiagnosticKind::NotIndexable { ty_name } => {
                format!("`{ty_name}` cannot be indexed")
            }
            InferDiagnosticKind::BadIndexType { ty_name } => {
                format!("an index must be a uint or an integer, found `{ty_name}`")
            }
            InferDiagnosticKind::IndexOutOfBounds { index, len } => {
                format!("index `{index}` is out of bounds (length {len})")
            }
            InferDiagnosticKind::CannotInferBound { trait_name } => {
                format!("cannot infer the type for a `{trait_name}` bound — add an annotation")
            }
            InferDiagnosticKind::AmbiguousMethod { name, traits } => {
                format!(
                    "multiple applicable methods `{name}`: implemented by traits {}",
                    traits.join(", ")
                )
            }
            InferDiagnosticKind::UnresolvedMethod { name } => {
                format!("no method `{name}` on this type")
            }
            InferDiagnosticKind::NegativeWidth { value } => {
                format!("uint width evaluates to {value}, but a width must be non-negative")
            }
            InferDiagnosticKind::UnevaluableWidth => {
                "this width is not a constant (its expression has no value, \
                 e.g. divide-by-zero or overflow)"
                    .to_owned()
            }
            InferDiagnosticKind::DeferredWidth => {
                "a call in a type position is not supported here — bind it to a \
                 const first: `let w = f(n); uint(w)`"
                    .to_owned()
            }
            InferDiagnosticKind::RecordConnector { name, needs_arrow } => {
                if *needs_arrow {
                    format!("`{name}` is an `in` field — bind it with `{name} => target`")
                } else {
                    format!(
                        "`{name}` is supplied by this constructor — use `{name} = value`, not `=>`"
                    )
                }
            }
            InferDiagnosticKind::PositionalArity {
                callee,
                expected,
                found,
            } => format!(
                "`{callee}` takes {expected} positional argument{}, but {found} {} supplied",
                if *expected == 1 { "" } else { "s" },
                if *found == 1 { "was" } else { "were" },
            ),
            InferDiagnosticKind::UnknownField { name } => {
                format!("no field `{name}` on this type")
            }
            InferDiagnosticKind::MissingField { name } => {
                format!("missing field `{name}` in record constructor")
            }
            InferDiagnosticKind::DuplicateField { name } => {
                format!("field `{name}` supplied more than once")
            }
            InferDiagnosticKind::FieldOnNonAggregate { name } => {
                format!("no field `{name}`: this type has no fields")
            }
            InferDiagnosticKind::TupleIndexOutOfBounds { index, arity } => {
                format!("no element `{index}` on a {arity}-tuple")
            }
            InferDiagnosticKind::TupleArityMismatch { expected, found } => {
                format!("expected a {expected}-tuple, found a {found}-tuple")
            }
        }
    }
}

/// A literal-fit check that survived inference against a still-symbolic
/// width: `value` must fit a `width`-bit integer — the backend emits an
/// elaboration-time assert. `signed` distinguishes `sint` (two's-complement
/// range `-2^(w-1) ..< 2^(w-1)`) from `uint`/`bits` (`0 ..< 2^w`); without it a
/// ground check (`mono_check`) cannot decide the bound.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct FitResidual<'db> {
    pub value: i128,
    pub width: ConstArg<'db>,
    pub signed: bool,
}

/// A slice-bounds obligation that could not be decided here because an endpoint
/// or the base length is still symbolic (a const generic): the high endpoint (or
/// `offset + width`) must be `<= len`. `infer`'s eager check decides the
/// all-literal case; this defers the symbolic-but-grounding case to `mono_check`,
/// which grounds both against the instantiation's subst (planning/slice_guards.md).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct SliceBoundsResidual<'db> {
    pub high: ConstArg<'db>,
    pub len: ConstArg<'db>,
}

/// The result of inferring one def: a type per expression and per local, the
/// resolved method calls, and the diagnostics.
#[derive(Clone, PartialEq, Eq, Default, salsa::Update)]
pub struct Inference<'db> {
    expr_types: HashMap<ExprId, Type<'db>>,
    local_types: HashMap<LocalId, Type<'db>>,
    method_resolutions: HashMap<ExprId, DefId<'db>>,
    /// Per call-site instantiation of the callee's generic params (rustc's
    /// node substs): deep-resolved Terms in declared-param order. The backend
    /// renders Const-kind entries as SV parameter bindings (`#(.n(8))`).
    call_substs: HashMap<ExprId, Vec<Term<'db>>>,
    diagnostics: Vec<InferDiagnostic>,
    /// Const equalities that could not be decided here — symbolic widths
    /// (`uint(n)` vs `uint(m)`, deferred arithmetic, locals in const position).
    /// Not errors: obligations that survived the end-of-body fixpoint. The
    /// back end discharges Param-Param pairs as `initial assert (n == m)`;
    /// `const_eval` (Q4c) takes the rest.
    const_residuals: Vec<(ConstArg<'db>, ConstArg<'db>)>,
    /// Literal-fit checks against still-symbolic widths: `(value, width)` —
    /// the backend emits each as an elaboration-time assert.
    fit_residuals: Vec<FitResidual<'db>>,
    /// Slice-bounds checks (`high <= len`) deferred because an endpoint or the
    /// base length is symbolic — `mono_check` grounds them at each instantiation.
    slice_residuals: Vec<SliceBoundsResidual<'db>>,
}

impl<'db> Inference<'db> {
    pub fn expr_type(&self, e: ExprId) -> Option<&Type<'db>> {
        self.expr_types.get(&e)
    }

    pub fn call_subst(&self, e: ExprId) -> Option<&[Term<'db>]> {
        self.call_substs.get(&e).map(Vec::as_slice)
    }

    pub fn local_type(&self, l: LocalId) -> Option<&Type<'db>> {
        self.local_types.get(&l)
    }

    pub fn method_resolution(&self, e: ExprId) -> Option<DefId<'db>> {
        self.method_resolutions.get(&e).copied()
    }

    pub fn diagnostics(&self) -> &[InferDiagnostic] {
        &self.diagnostics
    }

    /// Unresolved const equalities (residual obligations), for the back end's
    /// `initial assert` and, later, `const_eval`.
    pub fn const_residuals(&self) -> &[(ConstArg<'db>, ConstArg<'db>)] {
        &self.const_residuals
    }

    pub fn slice_residuals(&self) -> &[SliceBoundsResidual<'db>] {
        &self.slice_residuals
    }

    pub fn fit_residuals(&self) -> &[FitResidual<'db>] {
        &self.fit_residuals
    }
}

/// QUERY: infer a function/method's body. Non-fn defs yield an empty inference.
#[salsa::tracked(returns(ref))]
pub fn infer<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Inference<'db> {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Inference::default();
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return Inference::default();
    }
    let sig = sig_of(db, krate, def);
    let body = body(db, krate, def);

    let mut cx = InferCtx::new(db, krate, def, body, map, &sig.generic_params);

    // Seed locals: params get their signature type (own generic `Param`s stay
    // rigid; only unspecified domains become fresh vars). Other locals start as
    // a fresh type var, refined by `let`/equation; a declared `var` type unifies.
    for (i, p) in sig.params.iter().enumerate() {
        let ty = cx.freshen_domains(&p.ty);
        cx.local_types.insert(LocalId(i as u32), ty);
    }
    // A result place (`return`, a named result, or a tuple part) carries its
    // type as `declared_ty`, so the normal declared-type seeding below freshens
    // and unifies it like any `var` — no special case (planning/return_variable.md).
    let ret = sig.return_type.as_ref().map(|t| cx.freshen_domains(t));
    for (i, local) in body.locals().iter().enumerate() {
        let id = LocalId(i as u32);
        if cx.local_types.contains_key(&id) {
            continue; // a param, already seeded
        }
        let var = cx.fresh_type();
        cx.local_types.insert(id, var.clone());
        if let Some(declared) = &local.declared_ty {
            // A width naming a local (`uint(n)`) obligates that local's domain
            // to be `@const` — hardware can't have a runtime-varying width.
            for l in width_locals(declared) {
                cx.obligations.push(Obligation {
                    span: Span::default(),
                    kind: ObligationKind::ConstDomain(l),
                });
            }
            let declared = cx.freshen_domains(declared);
            cx.unify(&var, &declared);
        }
    }

    cx.infer_block(body, body.block(), ret.as_ref());
    cx.finish()
}

/// One union-find table over a **single variable space**, kind-annotated —
/// chalk's `InferenceTable` shape. Variables are merged by redirect (so
/// `unify(v, v)` is structurally a no-op) and bound only at their root.
struct InferenceTable<'db> {
    vars: Vec<VarData<'db>>,
}

struct VarData<'db> {
    /// What kind of term this variable ranges over. Recorded at mint time;
    /// consumed once consts carry their type and domains their sort (Q7 B/C).
    #[allow(dead_code)]
    kind: TermKind,
    value: VarValue<'db>,
}

enum VarValue<'db> {
    Unbound,
    /// Union-find: merged into another variable; follow to the root.
    Redirect(InferVar),
    /// Bound to a (shallow-resolved, non-variable) term.
    Bound(Term<'db>),
}

impl<'db> InferenceTable<'db> {
    fn new() -> Self {
        Self { vars: Vec::new() }
    }

    fn fresh(&mut self, kind: TermKind) -> InferVar {
        self.vars.push(VarData {
            kind,
            value: VarValue::Unbound,
        });
        InferVar(self.vars.len() as u32 - 1)
    }

    /// The root of `v`'s redirect chain, with path compression.
    fn find(&mut self, v: InferVar) -> InferVar {
        match self.vars[v.0 as usize].value {
            VarValue::Redirect(next) => {
                let root = self.find(next);
                self.vars[v.0 as usize].value = VarValue::Redirect(root);
                root
            }
            _ => v,
        }
    }

    /// The term bound at `v`'s root, if any.
    fn probe(&mut self, v: InferVar) -> Option<Term<'db>> {
        let root = self.find(v);
        match &self.vars[root.0 as usize].value {
            VarValue::Bound(t) => Some(t.clone()),
            _ => None,
        }
    }

    /// The kind recorded for `v` (at its root).
    fn kind_of(&mut self, v: InferVar) -> TermKind {
        let root = self.find(v);
        self.vars[root.0 as usize].kind
    }

    /// Merge two variables (callers resolve first, so both roots are unbound).
    /// Domain sorts merge upward: if either side requires `Clock`, the merged
    /// root does.
    fn union(&mut self, a: InferVar, b: InferVar) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if let (TermKind::Domain(sa), TermKind::Domain(sb)) =
            (self.vars[ra.0 as usize].kind, self.vars[rb.0 as usize].kind)
            && (sa == DomainSort::Clock || sb == DomainSort::Clock)
        {
            self.vars[ra.0 as usize].kind = TermKind::Domain(DomainSort::Clock);
        }
        self.vars[rb.0 as usize].value = VarValue::Redirect(ra);
    }

    /// Bind `v`'s root to a (shallow-resolved, non-variable) term.
    fn bind(&mut self, v: InferVar, t: Term<'db>) {
        let root = self.find(v);
        self.vars[root.0 as usize].value = VarValue::Bound(t);
    }

    /// Follow `ty` through the table until its head is not a bound variable.
    fn resolve_type_shallow(&mut self, ty: &Type<'db>) -> Type<'db> {
        let mut cur = ty.clone();
        while let Type::Infer(v) = cur {
            match self.probe(v) {
                Some(Term::Type(t)) => cur = t,
                _ => return Type::Infer(self.find(v)),
            }
        }
        cur
    }

    fn resolve_const_shallow(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
        let mut cur = c.clone();
        while let ConstArg::Infer(v) = cur {
            match self.probe(v) {
                Some(Term::Const(t)) => cur = t,
                _ => return ConstArg::Infer(self.find(v)),
            }
        }
        cur
    }

    fn resolve_domain_shallow(&mut self, d: Domain) -> Domain {
        let mut cur = d;
        while let Domain::Infer(v) = cur {
            match self.probe(v) {
                Some(Term::Domain(t)) => cur = t,
                _ => return Domain::Infer(self.find(v)),
            }
        }
        cur
    }
}

/// A short name for a type in literal/width diagnostics.
fn describe_kind(ty: &Type<'_>) -> String {
    match ty {
        Type::Value { kind, .. } => match kind {
            ValueKind::UInt { .. } => "uint".to_owned(),
            ValueKind::SInt { .. } => "sint".to_owned(),
            ValueKind::Bits { .. } => "bits".to_owned(),
            ValueKind::Bool => "bool".to_owned(),
            ValueKind::Reset => "Reset".to_owned(),
            ValueKind::Event => "Event".to_owned(),
            ValueKind::Integer => "integer".to_owned(),
            ValueKind::Param(_) => "a type parameter".to_owned(),
        },
        Type::Vec { .. } => "a vector".to_owned(),
        Type::Tuple(_) => "a tuple".to_owned(),
        Type::Port { .. } => "a record".to_owned(),
        Type::Clock => "Clock".to_owned(),
        _ => "this type".to_owned(),
    }
}

struct InferCtx<'a, 'db> {
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    /// The def under inference — the root frame for `const_eval`.
    def: DefId<'db>,
    body: &'a Body<'db>,
    map: &'a CrateDefMap<'db>,
    /// The inferred def's own generic params (to map a clock local back to its
    /// `dom` generic index for `when`).
    own_generics: &'a [GenericParam],
    table: InferenceTable<'db>,
    expr_types: HashMap<ExprId, Type<'db>>,
    local_types: HashMap<LocalId, Type<'db>>,
    method_resolutions: HashMap<ExprId, DefId<'db>>,
    /// Per call-site instantiation of the callee's generic params (rustc's
    /// node substs): deep-resolved Terms in declared-param order. The backend
    /// renders Const-kind entries as SV parameter bindings (`#(.n(8))`).
    call_substs: HashMap<ExprId, Vec<Term<'db>>>,
    diagnostics: Vec<InferDiagnostic>,
    /// Literal inference vars (in creation order): unresolved ones fall
    /// back to `integer` when the obligation fixpoint stalls.
    literal_vars: Vec<InferVar>,
    /// Literal-fit checks that survived against symbolic widths.
    fit_residuals: Vec<FitResidual<'db>>,
    /// Slice-bounds checks deferred because an endpoint/length is symbolic.
    slice_residuals: Vec<SliceBoundsResidual<'db>>,
    /// Non-`range` for-loop elems, re-checked at finish: a compile-time
    /// integer element can only be the genvar (ForConstVec).
    for_elem_checks: Vec<(LocalId, Span)>,
    /// `const if` conditions, re-checked at finish (after deep-resolve, so a
    /// domain bound by a *later* equation has propagated): the condition must
    /// resolve to a compile-time constant, never a clock-domain (runtime) value.
    const_if_checks: Vec<(ExprId, Span)>,
    /// Undecided constraints, retried at end-of-body (`discharge_obligations`).
    obligations: Vec<Obligation<'db>>,
    /// Def-relative span of the expression currently under inference — attached
    /// to any diagnostic raised while unifying it.
    current_span: Span,
}

/// A constraint that could not be decided eagerly — queued with the span of
/// the expression that raised it, retried at the end-of-body fixpoint, and
/// surviving as a signature residual (the OutsideIn split).
struct Obligation<'db> {
    span: Span,
    kind: ObligationKind<'db>,
}

enum ObligationKind<'db> {
    /// Two consts that must be equal (symbolic widths).
    ConstEq(ConstArg<'db>, ConstArg<'db>),
    /// The local appears in const position (a width): its domain must resolve
    /// to `@const` (an unbound domain defaults const, so unbound passes).
    ConstDomain(LocalId),
    /// `self_ty: trait_def` — instantiated from a callee's predicates; solved
    /// against the param env and the trait's impls (planning/traits.md).
    Trait {
        trait_def: DefId<'db>,
        self_ty: Type<'db>,
        depth: u32,
    },
    /// A literal's value must fit the type its variable resolves to
    /// (planning/numeric_literals.md L2).
    LiteralFits { ty: Type<'db>, value: i128 },
}

/// The body locals referenced in const (width) position anywhere in `ty`.
/// Is `expr` a *whole* result-place binding — a bare result local (`return`, a
/// named result, or a tuple part), as opposed to a per-leaf `name.f`? Such a
/// drive joins against the declared type like a return, not a plain equation.
fn is_whole_result_place(body: &Body<'_>, expr: ExprId) -> bool {
    matches!(&body.expr(expr).kind, ExprKind::Local(l)
        if body.local(*l).result_base.is_some())
}

/// The outcome of typing a slice: a valid result type, a provable const
/// out-of-bounds, or a shape not handled (rejected as `SliceNotImplemented`).
enum SliceTy<'db> {
    Ok(Type<'db>),
    Oob { high: i128, len: i128 },
    NotImpl,
}

impl<'db> From<Option<Type<'db>>> for SliceTy<'db> {
    fn from(o: Option<Type<'db>>) -> Self {
        match o {
            Some(t) => SliceTy::Ok(t),
            None => SliceTy::NotImpl,
        }
    }
}

fn width_locals(ty: &Type<'_>) -> Vec<LocalId> {
    struct Collect(Vec<LocalId>);
    impl<'db> Folder<'db> for Collect {
        fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
            if let ConstArg::Local(l) = c {
                self.0.push(*l);
            }
            super_fold_const(self, c)
        }
    }
    let mut c = Collect(Vec::new());
    let _ = c.fold_type(ty);
    c.0
}

/// Every width `ConstArg` appearing in a type (UInt slots), tree included.
fn collect_widths<'db>(ty: &Type<'db>) -> Vec<ConstArg<'db>> {
    struct Collect<'db>(Vec<ConstArg<'db>>);
    impl<'db> Folder<'db> for Collect<'db> {
        fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
            // Only top-level width slots: a *sub*tree of a width may be
            // negative while the whole is fine (`uint(n - 3 + 10)`).
            if let Type::Value {
                kind:
                    ValueKind::UInt { width } | ValueKind::SInt { width } | ValueKind::Bits { width },
                ..
            } = t
            {
                self.0.push(width.clone());
            }
            super_fold_type(self, t)
        }
    }
    let mut c = Collect(Vec::new());
    let _ = c.fold_type(ty);
    c.0
}

/// True if a width tree contains a `ConstArg::Deferred` node (a call or other
/// expression outside the representable const fragment, written in a type).
fn width_is_deferred(c: &ConstArg<'_>) -> bool {
    match c {
        ConstArg::Deferred => true,
        ConstArg::Op(_, a, b) => width_is_deferred(a) || width_is_deferred(b),
        ConstArg::Field(b, _) => width_is_deferred(b),
        _ => false,
    }
}

impl<'a, 'db> InferCtx<'a, 'db> {
    fn new(
        db: &'db dyn salsa::Database,
        krate: SourceRoot,
        def: DefId<'db>,
        body: &'a Body<'db>,
        map: &'a CrateDefMap<'db>,
        own_generics: &'a [GenericParam],
    ) -> Self {
        Self {
            db,
            krate,
            def,
            body,
            map,
            own_generics,
            table: InferenceTable::new(),
            expr_types: HashMap::new(),
            local_types: HashMap::new(),
            method_resolutions: HashMap::new(),
            call_substs: HashMap::new(),
            diagnostics: Vec::new(),
            literal_vars: Vec::new(),
            fit_residuals: Vec::new(),
            slice_residuals: Vec::new(),
            for_elem_checks: Vec::new(),
            const_if_checks: Vec::new(),
            obligations: Vec::new(),
            current_span: Span::default(),
        }
    }

    /// Try to const-evaluate a width tree in this def's body (soft failure).
    fn try_eval(&self, c: &ConstArg<'db>) -> Option<i128> {
        crate::hir::const_eval::eval_const(self.db, self.krate, self.def, c)
    }

    /// Const-evaluate a width tree, distinguishing "still symbolic, defer" from
    /// "closed but has no value" (the latter is a hard error in `check_widths`).
    fn eval_width(&self, c: &ConstArg<'db>) -> crate::hir::const_eval::WidthEval {
        crate::hir::const_eval::eval_width(self.db, self.krate, self.def, c)
    }

    fn finish(mut self) -> Inference<'db> {
        let const_residuals = self.discharge_obligations();
        // Deep-resolve every recorded type: bind out inference vars, default
        // unconstrained domain vars to `@const` (MLsub: a var with no lower
        // bound simplifies to top).
        let exprs: Vec<ExprId> = self.expr_types.keys().copied().collect();
        for e in exprs {
            let t = self.expr_types[&e].clone();
            let t = self.deep_resolve(&t);
            self.expr_types.insert(e, t);
        }
        let locals: Vec<LocalId> = self.local_types.keys().copied().collect();
        for l in locals {
            let t = self.local_types[&l].clone();
            let t = self.deep_resolve(&t);
            self.local_types.insert(l, t);
        }
        for (elem, span) in std::mem::take(&mut self.for_elem_checks) {
            let t = self
                .local_types
                .get(&elem)
                .cloned()
                .map(|t| self.resolve_ty(&t));
            if matches!(
                t,
                Some(Type::Value {
                    kind: ValueKind::Integer,
                    ..
                })
            ) {
                self.current_span = span;
                self.diag(InferDiagnosticKind::ForConstVec);
            }
        }
        self.check_const_ifs();
        // A Mirin-bodied `#[inline]` fn now splices at the call site
        // (planning/inline_bodies.md); the v1 shape restrictions (clocked / `var`
        // / out-param / `const if` / integer params) live in `inline_check`.
        self.check_widths();
        let substs: Vec<(ExprId, Vec<Term<'db>>)> = self
            .call_substs
            .iter()
            .map(|(e, ts)| (*e, ts.clone()))
            .collect();
        for (e, ts) in substs {
            let resolved = ts
                .iter()
                .map(|t| {
                    let mut r = DeepResolver {
                        table: &mut self.table,
                    };
                    crate::hir::types::super_fold_term(&mut r, t)
                })
                .collect();
            self.call_substs.insert(e, resolved);
        }
        Inference {
            expr_types: self.expr_types,
            local_types: self.local_types,
            method_resolutions: self.method_resolutions,
            call_substs: self.call_substs,
            diagnostics: self.diagnostics,
            const_residuals,
            fit_residuals: self.fit_residuals,
            slice_residuals: self.slice_residuals,
        }
    }

    /// `const if` conditions (checked post deep-resolve): the condition must
    /// reduce to a compile-time constant. A clock-domain condition is runtime
    /// data (rejected always — even in an `#[inline]` fn). A condition that does
    /// not fold in a non-`#[inline]` def is either a symbolic const generic
    /// (needs the `generate if` lowering, not yet built) or a runtime value —
    /// both rejected here rather than panicking the backend. Inside an
    /// `#[inline]` fn a symbolic-const condition is fine: it grounds when the
    /// body is spliced at a call with concrete const generics (planning/inline_bodies.md).
    fn check_const_ifs(&mut self) {
        for (cond, span) in std::mem::take(&mut self.const_if_checks) {
            self.current_span = span;
            let dom = match self.expr_types.get(&cond).cloned() {
                Some(Type::Value { domain, .. }) => self.table.resolve_domain_shallow(domain),
                _ => continue,
            };
            // A runtime (clocked / domain-param) condition is still rejected — a
            // `const if` must be compile-time. A *symbolic const generic* cond
            // (`W == 8` with `W` a `#()` param) is now fine: it grounds when an
            // inline body is spliced, or lowers to an SV `generate if` otherwise
            // (planning/slice_guards.md Phase 4).
            if matches!(dom, Domain::Clock(_) | Domain::Param(_)) {
                self.diag(InferDiagnosticKind::ConstIfRuntimeCond);
            }
        }
    }

    /// Evaluate every ground width in the final types; a negative result is a
    /// hard error (`integer` maths may go negative in intermediates, a uint
    /// width may not come out negative). One diagnostic per distinct tree.
    fn check_widths(&mut self) {
        use crate::hir::const_eval::WidthEval;
        let mut seen: std::collections::HashSet<ConstArg> = Default::default();
        // A closed width with no value (divide-by-zero, overflow) or a negative
        // one — both at the local's span. Symbolic widths defer to mono.
        let mut bad: Vec<(Span, InferDiagnosticKind)> = Vec::new();
        let locals: Vec<(LocalId, Type<'db>)> = self
            .local_types
            .iter()
            .map(|(l, t)| (*l, t.clone()))
            .collect();
        for (l, t) in locals {
            for w in collect_widths(&t) {
                if !seen.insert(w.clone()) {
                    continue;
                }
                let span = self.body.local_span(l);
                // A call (or other non-representable expr) written directly in a
                // type — `uint(f(n))` — lowers to `ConstArg::Deferred`, which the
                // backend cannot render. Reject it cleanly (a hard error, not a
                // backend panic) pointing at the `let w = f(n); uint(w)` form,
                // which IS supported (const_net_duality.md).
                if width_is_deferred(&w) {
                    bad.push((span, InferDiagnosticKind::DeferredWidth));
                    continue;
                }
                match self.eval_width(&w) {
                    WidthEval::Value(v) if v < 0 => {
                        bad.push((span, InferDiagnosticKind::NegativeWidth { value: v }));
                    }
                    WidthEval::Failed => {
                        bad.push((span, InferDiagnosticKind::UnevaluableWidth));
                    }
                    WidthEval::Value(_) | WidthEval::Symbolic => {}
                }
            }
        }
        for (span, kind) in bad {
            self.current_span = span;
            self.diag(kind);
        }
    }

    /// End-of-body fixpoint: retry every queued obligation against the final
    /// bindings until none makes progress. Ground-and-unequal pairs become
    /// diagnostics (at the span that raised them); still-symbolic survivors
    /// are returned as the def's residuals.
    fn discharge_obligations(&mut self) -> Vec<(ConstArg<'db>, ConstArg<'db>)> {
        let mut residuals = Vec::new();
        loop {
            let mut progress = false;
            let pending = std::mem::take(&mut self.obligations);
            for ob in pending {
                match &ob.kind {
                    ObligationKind::ConstDomain(l) => {
                        // L7 wrap guard: the width-position local must be
                        // `integer`-typed (an unresolved literal binds here).
                        let lt = self.local_types.get(l).cloned();
                        if let Some(lt) = lt {
                            if self.is_literal_ty(&lt) {
                                let int = Type::Value {
                                    kind: ValueKind::Integer,
                                    domain: Domain::Const,
                                };
                                self.unify(&lt, &int);
                            }
                            let r = self.resolve_ty(&lt);
                            match &r {
                                // Hardware scalars are the wrap hazard; a
                                // struct local is fine (only its integer
                                // FIELDS are projected into widths).
                                Type::Value {
                                    kind:
                                        ValueKind::UInt { .. }
                                        | ValueKind::SInt { .. }
                                        | ValueKind::Bits { .. }
                                        | ValueKind::Bool
                                        | ValueKind::Reset
                                        | ValueKind::Event,
                                    ..
                                } => {
                                    let other = &r;
                                    self.current_span = ob.span;
                                    let ty_name = describe_kind(other);
                                    self.diag(InferDiagnosticKind::WidthNotInteger { ty_name });
                                    progress = true;
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        let dom = self
                            .local_types
                            .get(l)
                            .cloned()
                            .and_then(|t| self.domain_of(&t))
                            .map(|d| self.resolve_domain(d));
                        match dom {
                            // Unbound defaults to @const at finish — fine.
                            None | Some(Domain::Const) | Some(Domain::Infer(_)) => {
                                progress = true;
                            }
                            Some(Domain::Unspecified) => progress = true,
                            Some(_) => {
                                self.current_span = ob.span;
                                self.diag(InferDiagnosticKind::ClockedWidth);
                                progress = true;
                            }
                        }
                    }
                    ObligationKind::Trait {
                        trait_def,
                        self_ty,
                        depth,
                    } => {
                        let t = self.resolve_ty(self_ty);
                        if type_has_infer(&t) {
                            self.obligations.push(Obligation {
                                span: ob.span,
                                kind: ObligationKind::Trait {
                                    trait_def: *trait_def,
                                    self_ty: t,
                                    depth: *depth,
                                },
                            });
                        } else {
                            self.solve_trait(*trait_def, &t, *depth, ob.span);
                            progress = true;
                        }
                    }
                    ObligationKind::LiteralFits { ty, value } => {
                        let t = self.resolve_ty(ty);
                        let value = *value;
                        match &t {
                            Type::Infer(_) => self.obligations.push(Obligation {
                                span: ob.span,
                                kind: ObligationKind::LiteralFits { ty: t, value },
                            }),
                            Type::Error
                            | Type::Value {
                                kind: ValueKind::Integer,
                                ..
                            } => progress = true,
                            Type::Value {
                                kind: ValueKind::SInt { width },
                                ..
                            } => {
                                let w = self.resolve_const(width);
                                match self.try_eval(&w) {
                                    Some(n) => {
                                        // Two's complement: -2^(n-1) ≤ v < 2^(n-1).
                                        let half = 1i128 << (n - 1).clamp(0, 126);
                                        let fits =
                                            n >= 128 || (n >= 1 && value >= -half && value < half);
                                        if !fits {
                                            self.current_span = ob.span;
                                            self.diag(InferDiagnosticKind::LiteralDoesNotFitS {
                                                value,
                                                width: n,
                                            });
                                        }
                                        progress = true;
                                    }
                                    None => self.obligations.push(Obligation {
                                        span: ob.span,
                                        kind: ObligationKind::LiteralFits {
                                            ty: t.clone(),
                                            value,
                                        },
                                    }),
                                }
                            }
                            Type::Value {
                                kind: ValueKind::UInt { width } | ValueKind::Bits { width },
                                ..
                            } => {
                                let w = self.resolve_const(width);
                                match self.try_eval(&w) {
                                    Some(n) => {
                                        let fits = value >= 0
                                            && (n >= 127
                                                || (n >= 0 && value < (1i128 << n.clamp(0, 126))));
                                        if !fits {
                                            self.current_span = ob.span;
                                            self.diag(InferDiagnosticKind::LiteralDoesNotFit {
                                                value,
                                                width: n,
                                            });
                                        }
                                        progress = true;
                                    }
                                    // Width still symbolic/unresolved: retry;
                                    // the drain turns symbolic survivors into
                                    // elaboration-time fit residuals.
                                    None => self.obligations.push(Obligation {
                                        span: ob.span,
                                        kind: ObligationKind::LiteralFits {
                                            ty: t.clone(),
                                            value,
                                        },
                                    }),
                                }
                            }
                            other => {
                                self.current_span = ob.span;
                                let ty_name = describe_kind(other);
                                self.diag(InferDiagnosticKind::LiteralBadType { ty_name });
                                progress = true;
                            }
                        }
                    }
                    ObligationKind::ConstEq(a, b) => {
                        let a = self.resolve_const(a);
                        let b = self.resolve_const(b);
                        if a == b {
                            progress = true;
                            continue;
                        }
                        // Ground both sides through the evaluator if possible
                        // (a symbolic side — free Param — stays None).
                        match (self.try_eval(&a), self.try_eval(&b)) {
                            (Some(x), Some(y)) if x == y => progress = true,
                            (Some(_), Some(_)) => {
                                self.current_span = ob.span;
                                self.diag(InferDiagnosticKind::WidthMismatch);
                                progress = true;
                            }
                            _ => self.obligations.push(Obligation {
                                span: ob.span,
                                kind: ObligationKind::ConstEq(a, b),
                            }),
                        }
                    }
                }
            }
            if !progress {
                // The stall: bind every still-unresolved literal var to
                // `integer` (rustc's fallback placement — after the main
                // fixpoint, before error reporting) and run once more.
                if self.fallback_literals() {
                    continue;
                }
                break;
            }
        }
        for ob in std::mem::take(&mut self.obligations) {
            match ob.kind {
                ObligationKind::ConstEq(a, b) => residuals.push((a, b)),
                ObligationKind::ConstDomain(_) => {}
                ObligationKind::LiteralFits { ty, value } => {
                    // A fit against a still-symbolic width survives as a
                    // residual (→ elaboration-time assert); a never-resolved
                    // width means the value never reaches hardware.
                    if let Type::Value { kind, .. } = &ty
                        && let ValueKind::UInt { width }
                        | ValueKind::SInt { width }
                        | ValueKind::Bits { width } = kind
                    {
                        let signed = matches!(kind, ValueKind::SInt { .. });
                        let w = self.resolve_const(width);
                        if !matches!(w, ConstArg::Infer(_)) {
                            self.fit_residuals.push(FitResidual {
                                value,
                                width: w,
                                signed,
                            });
                        }
                    }
                }
                ObligationKind::Trait { trait_def, .. } => {
                    // Still here ⇒ the self type never resolved.
                    self.current_span = ob.span;
                    let trait_name = self.trait_name(trait_def);
                    self.diag(InferDiagnosticKind::CannotInferBound { trait_name });
                }
            }
        }
        residuals
    }

    /// Solve `ty: trait_def` for a fully-resolved `ty` (rustc's select+confirm,
    /// minimal form): a `Param` receiver checks the inferring def's own
    /// written bounds (the param env); a concrete head matches the trait's
    /// impl headers, enqueueing a matched impl's own bounds at `depth + 1`.
    /// Every failure mode is a diagnostic — this never returns pending.
    fn solve_trait(&mut self, trait_def: DefId<'db>, ty: &Type<'db>, depth: u32, span: Span) {
        self.current_span = span;
        let trait_name = self.trait_name(trait_def);
        if depth > 64 {
            self.diag(InferDiagnosticKind::BoundOverflow { trait_name });
            return;
        }
        match ty {
            Type::Error => {}
            Type::Value {
                kind: ValueKind::Param(i),
                ..
            } => {
                // Param env: the def's own written bounds.
                let own = sig_of(self.db, self.krate, self.def);
                let held = own.predicates.iter().any(|p| {
                    let Predicate::Trait(tr) = p;
                    tr.trait_def == trait_def
                        && matches!(
                            &tr.self_ty,
                            Type::Value { kind: ValueKind::Param(j), .. } if j == i
                        )
                });
                if !held {
                    let ty_name = own
                        .generic_params
                        .get(*i as usize)
                        .map(|g| g.name.clone())
                        .unwrap_or_else(|| "_".to_owned());
                    self.diag(InferDiagnosticKind::UnsatisfiedBound {
                        ty_name,
                        trait_name,
                    });
                }
            }
            _ => {
                let Some(head) = self.owner_of(ty) else {
                    self.diag(InferDiagnosticKind::UnsatisfiedBound {
                        ty_name: "this type".to_owned(),
                        trait_name,
                    });
                    return;
                };
                let candidates: Vec<DefId<'db>> = self
                    .map
                    .trait_impls(trait_def)
                    .iter()
                    .filter(|d| d.self_def == head)
                    .map(|d| d.impl_def)
                    .collect();
                let mut matched: Vec<(DefId<'db>, Vec<Option<Term<'db>>>)> = Vec::new();
                for impl_def in candidates {
                    let hsig = sig_of(self.db, self.krate, impl_def);
                    let Some(header) = &hsig.return_type else {
                        continue;
                    };
                    let mut binding = vec![None; hsig.generic_params.len()];
                    if crate::hir::types::match_header(ty, header, &mut binding) {
                        matched.push((impl_def, binding));
                    }
                }
                match matched.len() {
                    0 => {
                        let ty_name = self
                            .map
                            .def_data(head)
                            .map(|d| d.name.clone())
                            .unwrap_or_else(|| "this type".to_owned());
                        self.diag(InferDiagnosticKind::UnsatisfiedBound {
                            ty_name,
                            trait_name,
                        });
                    }
                    1 => {
                        // Confirm: the impl's own bounds become nested
                        // obligations, instantiated with the header binding.
                        let (impl_def, binding) = matched.pop().unwrap();
                        let hsig = sig_of(self.db, self.krate, impl_def);
                        for pred in hsig.predicates.clone() {
                            let Predicate::Trait(tr) = pred;
                            let nested = crate::hir::types::subst_type_opt(&tr.self_ty, &binding);
                            self.obligations.push(Obligation {
                                span,
                                kind: ObligationKind::Trait {
                                    trait_def: tr.trait_def,
                                    self_ty: nested,
                                    depth: depth + 1,
                                },
                            });
                        }
                    }
                    _ => {
                        let ty_name = self
                            .map
                            .def_data(head)
                            .map(|d| d.name.clone())
                            .unwrap_or_else(|| "this type".to_owned());
                        self.diag(InferDiagnosticKind::AmbiguousImpls {
                            ty_name,
                            trait_name,
                        });
                    }
                }
            }
        }
    }

    /// Bind every still-unresolved literal var to `integer @const`.
    /// Returns whether anything was bound (→ run the fixpoint once more).
    fn fallback_literals(&mut self) -> bool {
        let vars = self.literal_vars.clone();
        let mut bound = false;
        for v in vars {
            let t = self.resolve_ty(&Type::Infer(v));
            if matches!(t, Type::Infer(_)) {
                let int = Type::Value {
                    kind: ValueKind::Integer,
                    domain: Domain::Const,
                };
                self.unify(&Type::Infer(v), &int);
                bound = true;
            }
        }
        bound
    }

    /// Is this (resolved) type an unresolved literal variable?
    fn is_literal_ty(&mut self, ty: &Type<'db>) -> bool {
        let Type::Infer(root) = self.resolve_ty(ty) else {
            return false;
        };
        let vars = self.literal_vars.clone();
        vars.into_iter()
            .any(|v| matches!(self.resolve_ty(&Type::Infer(v)), Type::Infer(r) if r == root))
    }

    /// `v[3]` against `Vec(3, _)`: a ground-literal index out of a ground
    /// length errors now; anything symbolic waits (planning/vectors.md).
    fn check_index_bounds(&mut self, body: &Body<'db>, index: ExprId, len: &ConstArg<'db>) {
        let ExprKind::Number(i, _) = body.expr(index).kind else {
            return;
        };
        let len = self.resolve_const(len);
        if let Some(n) = self.try_eval(&len)
            && (i < 0 || i >= n)
        {
            self.diag(InferDiagnosticKind::IndexOutOfBounds { index: i, len: n });
        }
    }

    fn trait_name(&self, trait_def: DefId<'db>) -> String {
        self.map
            .def_data(trait_def)
            .map(|d| d.name.clone())
            .unwrap_or_default()
    }

    // ----- inference variables -----

    fn fresh_type(&mut self) -> Type<'db> {
        Type::Infer(self.table.fresh(TermKind::Type))
    }

    fn fresh_domain(&mut self) -> Domain {
        Domain::Infer(self.table.fresh(TermKind::Domain(DomainSort::Domain)))
    }

    fn fresh_domain_sorted(&mut self, sort: DomainSort) -> Domain {
        Domain::Infer(self.table.fresh(TermKind::Domain(sort)))
    }

    fn fresh_const(&mut self) -> ConstArg<'db> {
        ConstArg::Infer(self.table.fresh(TermKind::Const))
    }

    fn resolve_const(&mut self, w: &ConstArg<'db>) -> ConstArg<'db> {
        self.table.resolve_const_shallow(w)
    }

    /// Replace every `Unspecified` domain in `ty` with a fresh domain var, so an
    /// un-annotated type's domain can be inferred. Concrete/`Param` domains stay.
    fn freshen_domains(&mut self, ty: &Type<'db>) -> Type<'db> {
        match ty {
            Type::Value { kind, domain } => Type::Value {
                // A struct's `domain` stamps its fields; its own inner slots
                // are not independent. A leaf's domain is just its domain.
                kind: kind.clone(),
                domain: self.freshen_domain(*domain),
            },
            // An aggregate has no domain of its own — freshen its elements'
            // (planning/domain_checking.md): an un-annotated element domain
            // is a genuine inference variable now, not stamped from an
            // aggregate domain that no longer exists.
            Type::Vec { len, elem } => Type::Vec {
                len: len.clone(),
                elem: Box::new(self.freshen_domains(elem)),
            },
            Type::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.freshen_domains(e)).collect())
            }
            Type::Port { def, args, domain } => Type::Port {
                def: *def,
                args: args.clone(),
                domain: self.freshen_domain(*domain),
            },
            other => other.clone(),
        }
    }

    fn freshen_domain(&mut self, d: Domain) -> Domain {
        match d {
            Domain::Unspecified => self.fresh_domain(),
            other => other,
        }
    }

    fn resolve_ty(&mut self, ty: &Type<'db>) -> Type<'db> {
        self.table.resolve_type_shallow(ty)
    }

    fn resolve_domain(&mut self, d: Domain) -> Domain {
        self.table.resolve_domain_shallow(d)
    }

    /// Resolve every variable in `ty` (recursively), defaulting what is still
    /// unbound: types to `Error` (a hole), widths to `Deferred`, domains to
    /// `@const` (MLsub: a var with no lower bound simplifies to top).
    fn deep_resolve(&mut self, ty: &Type<'db>) -> Type<'db> {
        DeepResolver {
            table: &mut self.table,
        }
        .fold_type(ty)
    }

    // ----- unification -----

    fn unify(&mut self, a: &Type<'db>, b: &Type<'db>) {
        let a = self.resolve_ty(a);
        let b = self.resolve_ty(b);
        // Same term — nothing to do (the union-find also makes same-var
        // unification a structural no-op, but skipping equal ground terms here
        // is still the cheap path).
        if a == b {
            return;
        }
        match (&a, &b) {
            (Type::Infer(v), Type::Infer(w)) => self.table.union(*v, *w),
            (Type::Infer(v), _) => self.table.bind(*v, Term::Type(b.clone())),
            (_, Type::Infer(v)) => self.table.bind(*v, Term::Type(a.clone())),
            (Type::Error, _) | (_, Type::Error) => {}
            (
                Type::Value {
                    kind: ka,
                    domain: da,
                },
                Type::Value {
                    kind: kb,
                    domain: db,
                },
            ) => {
                self.unify_kind(ka, kb);
                self.unify_domain(*da, *db);
            }
            (
                Type::Port {
                    def: da,
                    args: aa,
                    domain: dda,
                },
                Type::Port {
                    def: dbb,
                    args: ab,
                    domain: ddb,
                },
            ) => {
                if da != dbb {
                    self.diag(InferDiagnosticKind::TypeMismatch);
                } else {
                    self.unify_args(aa, ab);
                }
                self.unify_domain(*dda, *ddb);
            }
            // Aggregates have no domain of their own — unify element-wise
            // (planning/domain_checking.md).
            (Type::Vec { len: la, elem: ea }, Type::Vec { len: lb, elem: eb }) => {
                self.unify_width(la.clone(), lb.clone());
                let (ea, eb) = ((**ea).clone(), (**eb).clone());
                self.unify(&ea, &eb);
            }
            (Type::Tuple(ea), Type::Tuple(eb)) => {
                if ea.len() != eb.len() {
                    self.diag(InferDiagnosticKind::TupleArityMismatch {
                        expected: eb.len(),
                        found: ea.len(),
                    });
                    return;
                }
                let (ea, eb) = (ea.clone(), eb.clone());
                for (x, y) in ea.iter().zip(&eb) {
                    self.unify(x, y);
                }
            }
            (Type::Clock, Type::Clock) => {}
            _ => self.diag(InferDiagnosticKind::TypeMismatch),
        }
    }

    fn unify_kind(&mut self, a: &ValueKind<'db>, b: &ValueKind<'db>) {
        match (a, b) {
            (ValueKind::UInt { width: wa }, ValueKind::UInt { width: wb })
            | (ValueKind::SInt { width: wa }, ValueKind::SInt { width: wb })
            | (ValueKind::Bits { width: wa }, ValueKind::Bits { width: wb }) => {
                self.unify_width(wa.clone(), wb.clone());
            }
            (ValueKind::Bool, ValueKind::Bool)
            | (ValueKind::Reset, ValueKind::Reset)
            | (ValueKind::Event, ValueKind::Event)
            | (ValueKind::Integer, ValueKind::Integer) => {}
            (ValueKind::Param(i), ValueKind::Param(j)) if i == j => {}
            _ => self.diag(InferDiagnosticKind::TypeMismatch),
        }
    }

    /// Unify two argument lists of the *same* parametric def pairwise, by kind.
    fn unify_args(&mut self, a: &GenericArgs<'db>, b: &GenericArgs<'db>) {
        for (x, y) in a.0.iter().zip(b.0.iter()) {
            match (x, y) {
                (Term::Type(tx), Term::Type(ty)) => self.unify(tx, ty),
                (Term::Const(cx), Term::Const(cy)) => self.unify_width(cx.clone(), cy.clone()),
                (Term::Domain(dx), Term::Domain(dy)) => self.unify_domain(*dx, *dy),
                _ => self.diag(InferDiagnosticKind::TypeMismatch),
            }
        }
    }

    /// Unify two `uint` widths. A const var binds to the other side. The only
    /// hard error Q4a can soundly raise is **two ground literals that differ**
    /// (`uint(8)` vs `uint(16)`). Anything symbolic — a generic-param width, or a
    /// `Deferred` arithmetic/anon-const width — *cannot be decided* without the
    /// residual + `const_eval` machinery (Q4b/c), so it is accepted here rather
    /// than producing a false mismatch. (`pair_add{n,m}` unifying `n ~ m` is a
    /// residual, not an error.)
    fn unify_width(&mut self, a: ConstArg<'db>, b: ConstArg<'db>) {
        let a = self.resolve_const(&a);
        let b = self.resolve_const(&b);
        if a == b {
            return;
        }
        match (&a, &b) {
            (ConstArg::Infer(v), ConstArg::Infer(w)) => self.table.union(*v, *w),
            (ConstArg::Infer(v), _) => self.table.bind(*v, Term::Const(b.clone())),
            (_, ConstArg::Infer(v)) => self.table.bind(*v, Term::Const(a.clone())),
            (ConstArg::Lit(x), ConstArg::Lit(y)) if x != y => {
                self.diag(InferDiagnosticKind::WidthMismatch)
            }
            // Anything symbolic — generic params, deferred arithmetic, locals
            // in const position — is undecidable here. Recorded, never
            // silently dropped: retried at the end-of-body fixpoint, then
            // surviving as a residual (`initial assert` / const_eval).
            _ => self.obligations.push(Obligation {
                span: self.current_span,
                kind: ObligationKind::ConstEq(a, b),
            }),
        }
    }

    fn unify_domain(&mut self, a: Domain, b: Domain) {
        let a = self.resolve_domain(a);
        let b = self.resolve_domain(b);
        if a == b {
            return;
        }
        match (a, b) {
            (Domain::Infer(v), Domain::Infer(w)) => self.table.union(v, w),
            (Domain::Infer(v), other) | (other, Domain::Infer(v)) => {
                // Sort check at the bind: a Clock-sorted variable (a register's
                // clock) cannot be `@const`.
                if other == Domain::Const
                    && self.table.kind_of(v) == TermKind::Domain(DomainSort::Clock)
                {
                    self.diag(InferDiagnosticKind::RequiresClock);
                }
                self.table.bind(v, Term::Domain(other));
            }
            (Domain::Clock(x), Domain::Clock(y)) if x == y => {}
            (Domain::Param(x), Domain::Param(y)) if x == y => {}
            // Surface fact, resolved by lifting/stamping; lenient until the
            // backend stops reading it (Q7 phase D).
            (Domain::Unspecified, _) | (_, Domain::Unspecified) => {}
            // NOTE: `@const` vs a concrete clock is now a mismatch under
            // *unification*. Coercion sites use `subsume`, where `@const`
            // coerces into anything.
            _ => self.diag(InferDiagnosticKind::DomainMismatch),
        }
    }

    // ----- subsumption (coercion sites) -----

    /// `a` usable where `b` is expected: structural equality, except the
    /// top-level domain may coerce (`@const <: @D`). Applied at the coercion
    /// sites — argument positions, ascribed `let`, equations, `return` — the
    /// rustc coercion-site set.
    fn subsume(&mut self, a: &Type<'db>, b: &Type<'db>) {
        let a = self.resolve_ty(a);
        let b = self.resolve_ty(b);
        if a == b {
            return;
        }
        match (&a, &b) {
            // Aggregates subsume ELEMENT-WISE (covariant along the const
            // edge): each element at a coercion site is itself at a coercion
            // site, so `(x, 5)` fits `(uint(8) @a, uint(4) @b)` and a const
            // vec fits a clocked one (planning/domain_checking.md).
            (Type::Tuple(ea), Type::Tuple(eb)) if ea.len() == eb.len() => {
                let (ea, eb) = (ea.clone(), eb.clone());
                for (x, y) in ea.iter().zip(&eb) {
                    self.subsume(x, y);
                }
            }
            (Type::Vec { len: la, elem: ea }, Type::Vec { len: lb, elem: eb }) => {
                self.unify_width(la.clone(), lb.clone());
                let (ea, eb) = ((**ea).clone(), (**eb).clone());
                self.subsume(&ea, &eb);
            }
            (
                Type::Value {
                    kind: ka,
                    domain: da,
                },
                Type::Value {
                    kind: kb,
                    domain: db,
                },
            ) => {
                self.unify_kind(ka, kb);
                self.subsume_domain(*da, *db);
            }
            (
                Type::Port {
                    def: pa,
                    args: aa,
                    domain: da,
                },
                Type::Port {
                    def: pb,
                    args: ab,
                    domain: db,
                },
            ) => {
                if pa != pb {
                    self.diag(InferDiagnosticKind::TypeMismatch);
                } else {
                    self.unify_args(aa, ab);
                }
                self.subsume_domain(*da, *db);
            }
            // An unresolved EXPECTED side takes the actual's kind at a fresh
            // join domain (so a `@const` actual doesn't pin an inference
            // variable const — generic args are invariant, the lattice edge
            // only coerces at the top level).
            (Type::Value { .. }, Type::Infer(_)) => self.merge_branch(&b, &a),
            // No coercion through an unresolved actual or non-value side.
            _ => self.unify(&a, &b),
        }
    }

    /// Domain coercion: `@const` fits any expected domain (the lattice's one
    /// edge); anything else must unify.
    fn subsume_domain(&mut self, a: Domain, b: Domain) {
        if self.resolve_domain(a) == Domain::Const {
            return;
        }
        self.unify_domain(a, b);
    }

    /// Merge a branch/operand type into a join target (an `if`'s arms, an
    /// operator's operands, a `return` against the declared type): structural
    /// kinds unify; domains JOIN — `@const` is absorbed, clocks must agree.
    fn merge_branch(&mut self, target: &Type<'db>, t: &Type<'db>) {
        let t = self.resolve_ty(t);
        let r = self.resolve_ty(target);
        match (&r, &t) {
            // First resolved branch seeds the target's kind at a fresh join
            // domain, so a `@const` branch doesn't pin the result const.
            (Type::Infer(_), Type::Value { kind, domain }) => {
                let dv = self.fresh_domain();
                let seeded = Type::Value {
                    kind: kind.clone(),
                    domain: dv,
                };
                self.unify(&r, &seeded);
                self.subsume_domain(*domain, dv);
            }
            (
                Type::Value {
                    kind: rk,
                    domain: rd,
                },
                Type::Value { kind, domain },
            ) => {
                self.unify_kind(kind, rk);
                self.subsume_domain(*domain, *rd);
            }
            _ => self.unify(&r, &t),
        }
    }

    // ----- the walk -----

    fn infer_block(
        &mut self,
        body: &Body<'db>,
        block: &crate::hir::body::Block,
        ret: Option<&Type<'db>>,
    ) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { local, value } => {
                    let vt = self.infer_expr(body, *value);
                    let lt = self.local_types[local].clone();
                    self.subsume(&vt, &lt);
                }
                Stmt::VarDecl { .. } => {}
                Stmt::Equation { lhs, rhs } => {
                    let l = self.infer_expr(body, *lhs);
                    let r = self.infer_expr(body, *rhs);
                    // A whole-result drive (`return EXPR;`/tail, desugared to
                    // `return = EXPR`) joins like a return against the declared
                    // type — domains JOIN, aggregates check invariantly — not
                    // the element-wise coercion a plain equation gets. Keeps
                    // tuple/aggregate return checking identical to the old
                    // `Stmt::Return` path. A per-leaf `return.f = …` is an
                    // ordinary equation (coerces at the leaf).
                    if is_whole_result_place(body, *lhs) {
                        self.merge_branch(&l, &r);
                    } else {
                        self.subsume(&r, &l);
                    }
                }
                Stmt::Return { value } => {
                    let v = self.infer_expr(body, *value);
                    if let Some(ret) = ret {
                        self.merge_branch(ret, &v);
                    }
                }
                Stmt::Expr(e) => {
                    self.infer_expr(body, *e);
                }
                Stmt::For {
                    index,
                    elem,
                    iter,
                    body: for_body,
                } => {
                    let it = self.infer_expr(body, *iter);
                    let it = self.resolve_ty(&it);
                    let elem_ty = match &it {
                        // The element carries its own domain (an aggregate has
                        // none — planning/domain_checking.md).
                        Type::Vec { elem, .. } => (**elem).clone(),
                        Type::Value {
                            kind: ValueKind::Bits { .. },
                            domain,
                        } => Type::Value {
                            kind: ValueKind::Bool,
                            domain: *domain,
                        },
                        Type::Error => Type::Error,
                        other => {
                            self.diag(InferDiagnosticKind::NotIterable {
                                ty_name: describe_kind(other),
                            });
                            Type::Error
                        }
                    };
                    // A compile-time integer vector only iterates as the
                    // genvar, which is only correct for `range(n)` itself
                    // (values 0..N-1 in order) — anything else ([3,1,2],
                    // a future .reverse()) is rejected until const vecs can
                    // drive per-iteration elaboration constants. The elem
                    // type may still be a literal var here (integer comes
                    // from the end-of-body fallback), so check at finish.
                    if !matches!(
                        body.local(*elem).kind,
                        crate::hir::body::LocalKind::ForBound
                    ) {
                        self.for_elem_checks.push((*elem, self.current_span));
                    }
                    if let Some(et) = self.local_types.get(elem).cloned() {
                        self.unify(&elem_ty, &et);
                    } else {
                        self.local_types.insert(*elem, elem_ty);
                    }
                    if let Some(i) = index {
                        self.local_types.insert(
                            *i,
                            Type::Value {
                                kind: ValueKind::Integer,
                                domain: Domain::Const,
                            },
                        );
                    }
                    let for_body = for_body.clone();
                    self.infer_block(body, &for_body, None);
                }
                Stmt::When {
                    event,
                    body: when_body,
                    init,
                } => {
                    // The event (`clk.posedge()`) types like any expression; the
                    // init and body blocks are equation systems over the driven
                    // var(s) — recurse so each drive's lhs/rhs unify.
                    self.infer_expr(body, *event);
                    if let Some(init) = init {
                        let init = init.clone();
                        self.infer_block(body, &init, None);
                    }
                    let when_body = when_body.clone();
                    self.infer_block(body, &when_body, None);
                }
            }
        }
        if let Some(tail) = block.tail {
            let t = self.infer_expr(body, tail);
            if let Some(ret) = ret {
                self.merge_branch(ret, &t);
            }
        }
    }

    fn infer_expr(&mut self, body: &Body<'db>, expr: ExprId) -> Type<'db> {
        // Locate any diagnostic raised while inferring this expression here. (A
        // nested infer overwrites it, so a composite mismatch points at the
        // sub-expression nearest the failure — close enough to be useful.)
        self.current_span = body.expr_span(expr);
        let ty = self.infer_expr_inner(body, expr);
        self.current_span = body.expr_span(expr);
        self.expr_types.insert(expr, ty.clone());
        ty
    }

    /// A literal endpoint's value (a `Number`/typed literal), else `None`.
    /// A slice endpoint as a const arg: a literal or a const generic param. Other
    /// shapes (a runtime value, arithmetic) are not const endpoints here.
    fn const_arg_of(&self, body: &Body<'db>, e: ExprId) -> Option<ConstArg<'db>> {
        match &body.expr(e).kind {
            ExprKind::Number(v, _) => Some(ConstArg::Lit(*v)),
            ExprKind::TypedLiteral { value, .. } => Some(ConstArg::Lit(*value)),
            ExprKind::ConstParam(i) => Some(ConstArg::Param(*i)),
            // A const local (`let lo = …; v[lo..]`): a `ConstArg::Local` leaf —
            // `const_eval` resolves it, and infer's `@const` check rejects a
            // non-const local in this width position.
            ExprKind::Local(l) => Some(ConstArg::Local(*l)),
            ExprKind::Field { receiver, field } => Some(ConstArg::Field(
                Box::new(self.const_arg_of(body, *receiver)?),
                field.clone(),
            )),
            ExprKind::ConstAssoc { item, self_ty } => Some(ConstArg::Assoc {
                item: *item,
                self_ty: Box::new(self_ty.clone()),
            }),
            // Width arithmetic (`v[lo + 4 .. lo]`): operators desugar to method
            // calls, folded to a `ConstArg::Op`. A *plain* call stays unrepresentable
            // (`Deferred`) — bind it with a `let` first (planning/const_eval.md).
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                let op = match method.as_str() {
                    "add" => ConstOp::Add,
                    "sub" => ConstOp::Sub,
                    "mul" => ConstOp::Mul,
                    "div" => ConstOp::Div,
                    "rem" => ConstOp::Rem,
                    _ => return None,
                };
                let [a] = args.as_slice() else { return None };
                if a.out {
                    return None;
                }
                Some(ConstArg::Op(
                    op,
                    Box::new(self.const_arg_of(body, *receiver)?),
                    Box::new(self.const_arg_of(body, a.expr)?),
                ))
            }
            _ => None,
        }
    }

    /// Type a slice with two literal endpoints (the S4 cut so far). Both `bits`
    /// and `Vec` are written **low-first / ascending** (decision 2026-06-26):
    /// `x[low..high]` → `bits(high-low)` / `Vec(high-low, A)`. Returns `None` for
    /// any shape not yet handled (non-literal endpoints, non-bits/vec base, wrong
    /// order, zero width — the last awaits the prelude guard) so the caller
    /// rejects it cleanly.
    fn slice_literal(
        &mut self,
        body: &Body<'db>,
        bt: &Type<'db>,
        lo: Option<ExprId>,
        hi: Option<ExprId>,
        width: Option<ExprId>,
    ) -> SliceTy<'db> {
        // Base length `N` (for bounds + elision).
        let n = match bt {
            Type::Value {
                kind: ValueKind::Bits { width: n },
                ..
            } => n.clone(),
            Type::Vec { len: n, .. } => n.clone(),
            _ => return SliceTy::NotImpl,
        };
        // Ground a const arg to a literal, if it folds (else `None` — symbolic,
        // deferred to mono_check for bounds). Capture db/krate/def into locals so
        // the closure doesn't borrow `self` (we also push residuals to `self`).
        let (db, krate, def) = (self.db, self.krate, self.def);
        let eval = |c: &ConstArg<'db>| crate::hir::const_eval::eval_const(db, krate, def, c);

        // Offset form `x[off..+w]`: the base (`lo`) may be RUNTIME; only the
        // width must be a constant (literal or a const generic param).
        if let Some(w_e) = width {
            let Some(lo) = lo else {
                return SliceTy::NotImpl; // must have a base/offset
            };
            let Some(w) = self.const_arg_of(body, w_e) else {
                return SliceTy::NotImpl;
            };
            if matches!(&w, ConstArg::Lit(v) if *v < 0) {
                return SliceTy::NotImpl; // negative literal width is never a slice
            }
            // `w == 0` is allowed (like the two-endpoint `h == l` case): the prelude
            // guard's `const if w == 0` emits the effective-0-bit.
            // bounds: `off + w <= N`. A runtime base (`const_arg_of` → None) is a
            // sim-time concern, skipped. With a const base: decide eagerly when
            // `off`/`w`/`N` all fold; else record a residual for `mono_check` to
            // ground at each instantiation.
            if let Some(base) = self.const_arg_of(body, lo) {
                let high = ConstArg::Op(ConstOp::Add, Box::new(base), Box::new(w.clone()));
                match (eval(&high), eval(&n)) {
                    (Some(h), Some(nv)) if h > nv => {
                        return SliceTy::Oob { high: h, len: nv };
                    }
                    (Some(_), Some(_)) => {}
                    _ => self.slice_residuals.push(SliceBoundsResidual {
                        high,
                        len: n.clone(),
                    }),
                }
            }
            return SliceTy::from(self.sliced_ty(bt, w));
        }
        // Two-endpoint form `x[low..high]`, possibly elided. The base's length `N`
        // supplies an elided end (same rule for both types now): `x[low..]`→
        // `x[low..N]`, `x[..high]`→`x[0..high]`. Bare `x[..]` rejected.
        if lo.is_none() && hi.is_none() {
            return SliceTy::NotImpl; // bare `x[..]` is redundant
        }
        // `lo` is the low end (elided → 0), `hi` the high end (elided → N).
        let low = match lo {
            Some(e) => match self.const_arg_of(body, e) {
                Some(c) => c,
                None => return SliceTy::NotImpl,
            },
            None => ConstArg::Lit(0),
        };
        let high = match hi {
            Some(e) => match self.const_arg_of(body, e) {
                Some(c) => c,
                None => return SliceTy::NotImpl,
            },
            None => n.clone(),
        };
        // bounds: `high <= N` and `low >= 0`. Decide eagerly when they fold; a
        // symbolic high/N records a residual for `mono_check` to ground.
        match (eval(&high), eval(&n)) {
            (Some(h), Some(nv)) if h > nv => return SliceTy::Oob { high: h, len: nv },
            (Some(_), Some(_)) => {}
            _ => self.slice_residuals.push(SliceBoundsResidual {
                high: high.clone(),
                len: n.clone(),
            }),
        }
        if let Some(l) = eval(&low)
            && l < 0
        {
            return SliceTy::Oob { high: l, len: 0 };
        }
        // width = high - low. Fold when both endpoints are literal (so concrete
        // slices keep a `Lit` width); else build a symbolic `Op(Sub, …)` that
        // grounds at elaboration.
        let w = match (&high, &low) {
            (ConstArg::Lit(h), ConstArg::Lit(l)) => {
                if h < l {
                    return SliceTy::NotImpl; // descending (wrong order)
                }
                // `h == l` is a zero-width slice — allowed: the prelude guard's
                // `const if w == 0` emits the effective-0-bit (planning/slice_guards.md).
                ConstArg::Lit(h - l)
            }
            _ => ConstArg::Op(ConstOp::Sub, Box::new(high), Box::new(low)),
        };
        SliceTy::from(self.sliced_ty(bt, w))
    }

    /// The result type of a width-`w` slice of `bt` (`bits(w)` / `Vec(w, A)`).
    fn sliced_ty(&self, bt: &Type<'db>, w: ConstArg<'db>) -> Option<Type<'db>> {
        match bt {
            Type::Value {
                kind: ValueKind::Bits { .. },
                domain,
            } => Some(Type::Value {
                kind: ValueKind::Bits { width: w },
                domain: *domain,
            }),
            Type::Vec { elem, .. } => Some(Type::Vec {
                len: w,
                elem: elem.clone(),
            }),
            _ => None,
        }
    }

    /// Push an inference diagnostic at the current expression's span.
    fn diag(&mut self, kind: InferDiagnosticKind) {
        self.diagnostics.push(InferDiagnostic {
            span: self.current_span,
            kind,
        });
    }

    fn infer_expr_inner(&mut self, body: &Body<'db>, expr: ExprId) -> Type<'db> {
        match &body.expr(expr).kind {
            ExprKind::Missing => Type::Error,
            // A literal is a fresh LITERAL-flavored inference variable plus a
            // fit obligation (rustc's `{integer}` var, planning/numeric_literals.md
            // L2): it unifies with `uint(n)` or `integer` as context demands,
            // the fit check fires on resolution, and an unconstrained literal
            // falls back to `integer` when the fixpoint stalls.
            ExprKind::Number(v, _) => {
                let t = self.fresh_type();
                if let Type::Infer(var) = t {
                    self.literal_vars.push(var);
                }
                self.obligations.push(Obligation {
                    span: self.current_span,
                    kind: ObligationKind::LiteralFits {
                        ty: t.clone(),
                        value: *v,
                    },
                });
                t
            }
            // `uint(6)::4`: the type is written; the fit check is direct
            // (same obligation, concrete from birth). An elided domain is
            // `@const` — a constructed constant.
            ExprKind::TypedLiteral { value, ty, .. } => {
                let mut t = ty.clone();
                if let Type::Value { domain, .. } | Type::Port { domain, .. } = &mut t
                    && matches!(domain, Domain::Unspecified)
                {
                    *domain = Domain::Const;
                }
                self.obligations.push(Obligation {
                    span: self.current_span,
                    kind: ObligationKind::LiteralFits {
                        ty: t.clone(),
                        value: *value,
                    },
                });
                t
            }
            ExprKind::Bool(_) => Type::Value {
                kind: ValueKind::Bool,
                domain: Domain::Const,
            },
            // A Const-kind generic used as a value: an `integer` known at
            // compile time, hence `@const` (coerces into any clock via the one
            // subtyping edge). The concrete value rides the SV `#(…)` parameter.
            ExprKind::ConstParam(_) => Type::Value {
                kind: ValueKind::Integer,
                domain: Domain::Const,
            },
            // An associated-const projection as a value (`A::bit_size`): an
            // `integer @const`, like `ConstParam`; its value resolves through
            // `eval_assoc` once Self is concrete (monomorphisation).
            ExprKind::ConstAssoc { .. } => Type::Value {
                kind: ValueKind::Integer,
                domain: Domain::Const,
            },
            // `[a, b, c]`: the elements unify; the length is theirs to count.
            ExprKind::VecLit(elems) => {
                let elems = elems.clone();
                let elem_ty = self.fresh_type();
                for e in &elems {
                    let t = self.infer_expr(body, *e);
                    self.subsume(&t, &elem_ty);
                }
                Type::Vec {
                    len: ConstArg::Lit(elems.len() as i128),
                    elem: Box::new(elem_ty),
                }
            }
            // `(a, b)`: elements type independently — each keeps its own
            // domain (planning/tuples.md).
            ExprKind::TupleLit(elems) => {
                let elems = elems.clone();
                let tys = elems.iter().map(|e| self.infer_expr(body, *e)).collect();
                Type::Tuple(tys)
            }
            // `[e; N]`.
            ExprKind::VecRepeat { elem, len } => {
                let len = len.clone();
                let t = self.infer_expr(body, *elem);
                Type::Vec {
                    len,
                    elem: Box::new(t),
                }
            }
            // `v[i]`: Vec(N, A) → A; bits(N) → bool. The index is a literal,
            // an integer, or a uint (a hardware select); its domain joins the
            // base's via the result.
            // Slicing (planning/slicing.md): low-first/ascending for both `bits`
            // and `Vec` (`x[low..high]` → `bits(high-low)` / `Vec(high-low, A)`).
            // Endpoints must be elaboration-constant (offset form allows a runtime
            // base); a const out-of-bounds is rejected; unhandled shapes reject
            // cleanly — never fall through to the base.
            ExprKind::Slice {
                base,
                lo,
                hi,
                width,
            } => {
                let (base, lo, hi, width) = (*base, *lo, *hi, *width);
                let bt = self.infer_expr(body, base);
                let bt = self.resolve_ty(&bt);
                for e in [lo, hi, width].into_iter().flatten() {
                    self.infer_expr(body, e);
                }
                match self.slice_literal(body, &bt, lo, hi, width) {
                    // A slice's width must be elaboration-constant. A const generic
                    // `Param` leaf is fine (it renders as an SV `#()` expression);
                    // a `Local` leaf must fold to a literal (`let lo = 4`). A
                    // value/runtime local endpoint folds to nothing — reject it
                    // rather than emit an illegal runtime-width slice.
                    SliceTy::Ok(ty)
                        if width_locals(&ty).into_iter().all(|l| {
                            crate::hir::const_eval::eval_const(
                                self.db,
                                self.krate,
                                self.def,
                                &ConstArg::Local(l),
                            )
                            .is_some()
                        }) =>
                    {
                        // Resolve the prelude `Slice` method for the desugar — `mir_of`
                        // builds the call from this (the `[..]` → `slice`/`slice_from`
                        // lowering; planning/slice_guards.md). Recorded only; the
                        // typing above is unchanged.
                        let method = if width.is_some() {
                            "slice_from"
                        } else {
                            "slice"
                        };
                        if let Some(owner) = self.owner_of(&bt) {
                            // Inherent first (the Vec impl), then the `Slice`
                            // trait (the bits family) — the same order
                            // `infer_method` uses for a normal method call.
                            if let Some(method_def) = self.map.impl_method(owner, method) {
                                self.method_resolutions.insert(expr, method_def);
                            } else {
                                let cands = self
                                    .select_by_header(self.map.trait_dispatch(owner, method), &bt);
                                if let [(_, method_def)] = cands.as_slice() {
                                    self.method_resolutions.insert(expr, *method_def);
                                }
                            }
                        }
                        // A zero-width result is total for every base: a `bits` slice
                        // routes through the prelude `const if` guard; a `Vec` slice
                        // (and an elided `bits` form) flattens to the empty/effective-
                        // 0-bit value in the backend (`undefined_vec_leaves`, the
                        // read-side dual of the slice-set guard — slice_guards.md).
                        ty
                    }
                    SliceTy::Oob { high, len } => {
                        self.diag(InferDiagnosticKind::SliceOutOfBounds { high, len });
                        Type::Error
                    }
                    _ => {
                        self.diag(InferDiagnosticKind::SliceNotImplemented);
                        Type::Error
                    }
                }
            }
            ExprKind::Index { base, index } => {
                let (base, index) = (*base, *index);
                let bt = self.infer_expr(body, base);
                let it = self.infer_expr(body, index);
                // A literal index resolves to integer (a static select).
                if self.is_literal_ty(&it) {
                    let int = Type::Value {
                        kind: ValueKind::Integer,
                        domain: Domain::Const,
                    };
                    self.unify(&it, &int);
                }
                let it = self.resolve_ty(&it);
                if !matches!(
                    &it,
                    Type::Value {
                        kind: ValueKind::UInt { .. } | ValueKind::Integer,
                        ..
                    } | Type::Error
                ) {
                    self.diag(InferDiagnosticKind::BadIndexType {
                        ty_name: describe_kind(&it),
                    });
                }
                let bt_r = self.resolve_ty(&bt);
                match &bt_r {
                    // `v[i]` is the element type directly — it carries its own
                    // domain (the Vec has none — planning/domain_checking.md).
                    Type::Vec { len, elem } => {
                        self.check_index_bounds(body, index, len);
                        (**elem).clone()
                    }
                    Type::Value {
                        kind: ValueKind::Bits { width },
                        domain,
                    } => {
                        self.check_index_bounds(body, index, width);
                        Type::Value {
                            kind: ValueKind::Bool,
                            domain: *domain,
                        }
                    }
                    Type::Error => Type::Error,
                    other => {
                        self.diag(InferDiagnosticKind::NotIndexable {
                            ty_name: describe_kind(other),
                        });
                        Type::Error
                    }
                }
            }
            ExprKind::Local(l) => self.local_types.get(l).cloned().unwrap_or(Type::Error),
            ExprKind::Def(_) => Type::Error, // a bare item ref is not a value
            ExprKind::Call {
                callee,
                args,
                named,
            } => self.infer_call(body, expr, *callee, args, named),
            ExprKind::Field { receiver, field } => self.infer_field(body, *receiver, field),
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => self.infer_method(body, expr, *receiver, method, args),
            ExprKind::TypePathCall {
                self_ty,
                method,
                args,
            } => self.infer_type_path_call(body, expr, self_ty, method, args),
            ExprKind::Record { ctor, fields } => self.infer_record(body, *ctor, fields),
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.infer_expr(body, *cond);
                let then_branch = then_branch.clone();
                let else_branch = else_branch.clone();
                let result = self.fresh_type();
                self.infer_block(body, &then_branch, Some(&result));
                self.infer_block(body, &else_branch, Some(&result));
                result
            }
            // `const if`: typed like `If` — the condition is inferred and both
            // arms unify with the result (so the value type is well-defined
            // regardless of which arm a given instantiation selects). The
            // difference is purely at lowering: the condition must be a constant
            // and only the selected arm is elaborated (planning/comptime_if.md).
            ExprKind::ConstIf {
                cond,
                then_branch,
                else_branch,
            } => {
                self.infer_expr(body, *cond);
                // The condition must resolve to a compile-time constant. Defer
                // the check to `finish` (recorded here): the condition's domain
                // can be pinned by a *later* equation (`var flag; const if flag;
                // flag = clocked`), so an eager check here races the constraint
                // system and would false-accept (planning/comptime_if.md).
                self.const_if_checks.push((*cond, self.current_span));
                let then_branch = then_branch.clone();
                let else_branch = else_branch.clone();
                let result = self.fresh_type();
                self.infer_block(body, &then_branch, Some(&result));
                self.infer_block(body, &else_branch, Some(&result));
                result
            }
            ExprKind::When {
                event,
                body: inner,
                init,
            } => {
                self.infer_expr(body, *event);
                let (inner, init) = (inner.clone(), *init);
                let result = self.fresh_type();
                if let Some(init) = init {
                    // Power-on state: a CONSTANT of the produced type
                    // (like a reg init).
                    let it = self.infer_expr(body, init);
                    let want = self.with_domain(&result, Domain::Const);
                    self.subsume(&it, &want);
                }
                self.infer_block(body, &inner, Some(&result));
                // The registered value lives on the event's clock: the body's
                // domain coerces in (@const may register), the result is ON it.
                if let Some(clock) = self.event_clock(body, *event) {
                    if let Some(rd) = self.domain_of(&result) {
                        self.subsume_domain(rd, clock);
                    }
                    return self.with_domain(&result, clock);
                }
                result
            }
            ExprKind::Block(inner) => {
                let inner = inner.clone();
                let result = self.fresh_type();
                self.infer_block(body, &inner, Some(&result));
                result
            }
        }
    }

    fn infer_call(
        &mut self,
        body: &Body<'db>,
        at: ExprId,
        callee: ExprId,
        args: &[ConnArg],
        named: &[NamedArg],
    ) -> Type<'db> {
        // The callee is a `Def` (operators lower to `Call(Def(op), …)` too).
        let ExprKind::Def(def) = body.expr(callee).kind else {
            for a in args {
                self.infer_expr(body, a.expr);
            }
            for n in named {
                self.infer_expr(body, n.expr);
            }
            return Type::Error;
        };
        // The builtin `range(k) -> Vec(k, integer)`: the length IS the
        // argument, lifted into const position (planning/for_loops.md).
        // In a `for`, the backend never materialises it — the genvar is
        // the element.
        if self
            .map
            .def_data(def)
            .is_some_and(|d| d.module == self.map.prelude() && d.name == "range")
        {
            let len = match args.first().map(|a| &body.expr(a.expr).kind) {
                Some(ExprKind::Number(v, _)) => ConstArg::Lit(*v),
                Some(ExprKind::Local(l)) => ConstArg::Local(*l),
                Some(ExprKind::ConstParam(i)) => ConstArg::Param(*i),
                Some(ExprKind::ConstAssoc { item, self_ty }) => ConstArg::Assoc {
                    item: *item,
                    self_ty: Box::new(self_ty.clone()),
                },
                _ => ConstArg::Deferred,
            };
            for a in args {
                self.infer_expr(body, a.expr);
            }
            return Type::Vec {
                len,
                elem: Box::new(Type::Value {
                    kind: ValueKind::Integer,
                    domain: Domain::Const,
                }),
            };
        }
        self.call_def(body, at, def, args, named, None)
    }

    /// Match a call's args against the callee's (instantiated) signature and
    /// return its (instantiated) return type. Positional args bind the callee's
    /// positional value params in order; named args bind the named-section params
    /// by name. A value arg unifies its type with the param; an out-connection
    /// (`=>`) unifies the param's type with the *target* place (the callee's `out`
    /// value flows into it). Direction-correctness is `directions(def)`'s job.
    /// `skip_self` drops the leading `self` for a method call.
    fn call_def(
        &mut self,
        body: &Body<'db>,
        at: ExprId,
        def: DefId<'db>,
        args: &[ConnArg],
        named: &[NamedArg],
        self_arg: Option<&Type<'db>>,
    ) -> Type<'db> {
        let skip_self = self_arg.is_some();
        let call_span = self.current_span;
        let sig = sig_of(self.db, self.krate, def);
        let subst = self.fresh_subst(&sig.generic_params);
        self.call_substs.insert(at, subst.clone());
        // The callee's where-clauses, instantiated at this call (rustc's
        // add_required_obligations): one Trait obligation per predicate.
        for pred in &sig.predicates {
            let Predicate::Trait(tr) = pred;
            let self_ty = self.substitute(&tr.self_ty, &subst);
            self.obligations.push(Obligation {
                span: call_span,
                kind: ObligationKind::Trait {
                    trait_def: tr.trait_def,
                    self_ty,
                    depth: 0,
                },
            });
        }

        // A method call: the receiver coerces into the declared `self` type
        // (its `@domain` annotation included — this is what pins the
        // method's dom generics to the receiver's clock).
        if let Some(recv) = self_arg
            && let Some(sp) = sig.params.iter().find(|p| p.is_self)
        {
            let want = self.substitute(&sp.ty, &subst);
            self.subsume(recv, &want);
        }

        // Positional args bind the positional value params (in declared order),
        // skipping `self` for a method call.
        let positional: Vec<&super::sig::Param<'db>> = sig
            .params
            .iter()
            .filter(|p| !p.from_named_section && (!skip_self || !p.is_self))
            .collect();
        // Arity: surplus args bind nothing; missing args are allowed only for
        // params with a default (the backend wires the default at the
        // instance). Named-arg name checking is `directions(def)`'s job.
        let required = positional.iter().filter(|p| p.default.is_none()).count();
        if args.len() > positional.len() || args.len() < required {
            let callee = self
                .map
                .def_data(def)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "this function".to_owned());
            let expected = if args.len() > positional.len() {
                positional.len()
            } else {
                required
            };
            self.current_span = call_span;
            self.diag(InferDiagnosticKind::PositionalArity {
                callee,
                expected,
                found: args.len(),
            });
        }
        for (i, a) in args.iter().enumerate() {
            let at = self.infer_expr(body, a.expr);
            if let Some(p) = positional.get(i) {
                let pt = self.substitute(&p.ty, &subst);
                // Value args coerce into the param; an out-connection flows the
                // callee's out param into the caller place.
                if a.out {
                    self.subsume(&pt, &at);
                } else {
                    self.subsume(&at, &pt);
                }
            }
        }
        // Named args bind named-section params by name.
        for n in named {
            let at = self.infer_expr(body, n.expr);
            if let Some(p) = sig
                .params
                .iter()
                .find(|p| p.from_named_section && p.name == n.name)
            {
                let pt = self.substitute(&p.ty, &subst);
                if n.out {
                    self.subsume(&pt, &at);
                } else {
                    self.subsume(&at, &pt);
                }
            }
        }
        match &sig.return_type {
            Some(rt) => self.substitute(rt, &subst),
            None => Type::Error,
        }
    }

    fn infer_field(&mut self, body: &Body<'db>, receiver: ExprId, field: &str) -> Type<'db> {
        let recv = self.infer_expr(body, receiver);
        // Tuple projection `p.0` (planning/tuples.md): the numeric field
        // indexes the element list. The element carries its own domain — a
        // tuple has none of its own (planning/domain_checking.md).
        if let Type::Tuple(elems) = self.resolve_ty(&recv) {
            let Ok(i) = field.parse::<usize>() else {
                self.diag(InferDiagnosticKind::UnknownField {
                    name: field.to_owned(),
                });
                return Type::Error;
            };
            let Some(elem) = elems.get(i).cloned() else {
                self.diag(InferDiagnosticKind::TupleIndexOutOfBounds {
                    index: i,
                    arity: elems.len(),
                });
                return Type::Error;
            };
            return elem;
        }
        // The field type is declared in terms of the def's generic params; we
        // instantiate it with the receiver's type args (`EarlyBinder::instantiate`).
        let (def, args) = match self.resolve_ty(&recv) {
            Type::Port { def, args, .. } => (def, args),
            // An Error receiver is already diagnosed; an unbound var receiver
            // is legal mid-equation-system (cyclic `var` wiring) — projecting
            // through it would need a deferred field obligation, not a hard
            // error here.
            Type::Error | Type::Infer(_) => return Type::Error,
            _ => {
                self.diag(InferDiagnosticKind::FieldOnNonAggregate {
                    name: field.to_owned(),
                });
                return Type::Error;
            }
        };
        let sig = sig_of(self.db, self.krate, def);
        match sig.fields.iter().find(|f| f.name == field) {
            Some(f) => {
                // Project the receiver's domain over the field's unannotated
                // slots (incl. those inside substituted args): `p.a` on
                // `p: Pair @c1` is wholly on `c1`.
                match self.domain_of(&recv) {
                    Some(rd) => self.substitute_stamped(&f.ty, &args.0, rd),
                    None => self.substitute(&f.ty, &args.0),
                }
            }
            None => {
                self.diag(InferDiagnosticKind::UnknownField {
                    name: field.to_owned(),
                });
                Type::Error
            }
        }
    }

    /// `uint(8)::unpack(b)` — an associated function on an explicit Self type
    /// (planning/pack_resize.md). Resolves the method from the written type
    /// (inherent first, then trait — like `infer_method`), then pins the impl's
    /// generics from the Self type. Unlike a method call there is no receiver
    /// value, so a fn with no `self` (return-type dispatch, e.g. `unpack`) is
    /// callable: `call_def` binds the value args, and the Self header unify
    /// below supplies what a receiver normally would.
    fn infer_type_path_call(
        &mut self,
        body: &Body<'db>,
        expr: ExprId,
        self_ty: &Type<'db>,
        method: &str,
        args: &[ConnArg],
    ) -> Type<'db> {
        let self_ty = self.resolve_ty(self_ty);
        if let Some(owner) = self.owner_of(&self_ty) {
            // Inherent wins over trait, mirroring `infer_method`.
            if let Some(method_def) = self.map.impl_method(owner, method) {
                self.method_resolutions.insert(expr, method_def);
                let ret = self.call_def(body, expr, method_def, args, &[], Some(&self_ty));
                self.pin_impl_self(expr, method_def, None, &self_ty);
                return ret;
            }
            let cands = self.select_by_header(self.map.trait_dispatch(owner, method), &self_ty);
            match cands.as_slice() {
                [] => {}
                [(trait_def, method_def)] => {
                    let (trait_def, method_def) = (*trait_def, *method_def);
                    self.method_resolutions.insert(expr, method_def);
                    let ret = self.call_def(body, expr, method_def, args, &[], Some(&self_ty));
                    self.pin_impl_self(expr, method_def, Some(trait_def), &self_ty);
                    return ret;
                }
                many => {
                    let traits = many
                        .iter()
                        .filter_map(|(t, _)| self.map.def_data(*t).map(|d| d.name.clone()))
                        .collect();
                    self.diag(InferDiagnosticKind::AmbiguousMethod {
                        name: method.to_owned(),
                        traits,
                    });
                    return Type::Error;
                }
            }
        }
        // A bounded type param (`A::unpack(b)` with `A: BitPack`): call the
        // trait method DECL — monomorphisation re-selects the impl with the
        // concrete Self (Instance::resolve), exactly as a `T`-typed receiver
        // does in `infer_method`. Self (the decl's Param(0)) is pinned to the
        // param from the call subst.
        if let Type::Value {
            kind: ValueKind::Param(i),
            ..
        } = self_ty
        {
            let own = sig_of(self.db, self.krate, self.def);
            let mut cands: Vec<(DefId<'db>, DefId<'db>)> = Vec::new();
            for p in &own.predicates {
                let Predicate::Trait(tr) = p;
                let on_this = matches!(
                    &tr.self_ty,
                    Type::Value { kind: ValueKind::Param(j), .. } if *j == i
                );
                if on_this && let Some(m) = self.map.trait_method(tr.trait_def, method) {
                    cands.push((tr.trait_def, m));
                }
            }
            match cands.as_slice() {
                [] => {}
                [(_, m)] => {
                    let m = *m;
                    self.method_resolutions.insert(expr, m);
                    let ret = self.call_def(body, expr, m, args, &[], Some(&self_ty));
                    // The decl's Self is Param(0); pin it to this param.
                    if let Some(subst) = self.call_substs.get(&expr).cloned() {
                        let decl_self = Type::Value {
                            kind: ValueKind::Param(0),
                            domain: Domain::Unspecified,
                        };
                        let inst = self.substitute(&decl_self, &subst);
                        self.unify(&self_ty, &inst);
                    }
                    return ret;
                }
                many => {
                    let traits = many
                        .iter()
                        .filter_map(|(t, _)| self.map.def_data(*t).map(|d| d.name.clone()))
                        .collect();
                    self.diag(InferDiagnosticKind::AmbiguousMethod {
                        name: method.to_owned(),
                        traits,
                    });
                    return Type::Error;
                }
            }
        }
        self.diag(InferDiagnosticKind::UnresolvedMethod {
            name: method.to_owned(),
        });
        Type::Error
    }

    /// Filter trait-impl method candidates to those whose impl Self header
    /// matches `goal`. Multiple impls of one trait for one owner only arise for
    /// the per-arity tuple impls (`BitPack for (A, B)` vs `(A, B, C)`), all keyed
    /// to the synthetic `Tuple` owner; the header match (which compares arity)
    /// picks the right one. A no-op with 0 or 1 candidates — so a genuine
    /// two-traits-offer-the-method ambiguity still surfaces.
    fn select_by_header(
        &self,
        cands: &[(DefId<'db>, DefId<'db>)],
        goal: &Type<'db>,
    ) -> Vec<(DefId<'db>, DefId<'db>)> {
        if cands.len() < 2 {
            return cands.to_vec();
        }
        cands
            .iter()
            .filter(|(td, md)| {
                let Some(data) = self
                    .map
                    .trait_impls(*td)
                    .iter()
                    .find(|d| d.methods.iter().any(|(_, m)| m == md))
                else {
                    return false;
                };
                let sig = sig_of(self.db, self.krate, data.impl_def);
                let Some(header) = sig.return_type.clone() else {
                    return false;
                };
                let mut binding = vec![None; sig.generic_params.len()];
                crate::hir::types::match_header(goal, &header, &mut binding)
            })
            .copied()
            .collect()
    }

    /// Pin a type-path callee's impl generics from the Self type: unify the
    /// impl's Self header — instantiated under the call's fresh substitution —
    /// with the written `self_ty`. A receiver-less fn (`unpack`) has no `self`
    /// param to carry this, so the header unify is what binds the impl's `n` to
    /// `uint(8)`. (For a method WITH `self`, `call_def`'s self-param subsume
    /// already pins them, and this agrees.)
    fn pin_impl_self(
        &mut self,
        expr: ExprId,
        method_def: DefId<'db>,
        trait_def: Option<DefId<'db>>,
        self_ty: &Type<'db>,
    ) {
        let Some(subst) = self.call_substs.get(&expr).cloned() else {
            return;
        };
        let Some(td) = trait_def else {
            return; // inherent: the self param (if any) already pins it
        };
        let header = self
            .map
            .trait_impls(td)
            .iter()
            .find(|d| d.methods.iter().any(|(_, m)| *m == method_def))
            .and_then(|d| sig_of(self.db, self.krate, d.impl_def).return_type.clone());
        if let Some(header) = header {
            let inst = self.substitute(&header, &subst);
            self.unify(self_ty, &inst);
        }
    }

    fn infer_method(
        &mut self,
        body: &Body<'db>,
        expr: ExprId,
        receiver: ExprId,
        method: &str,
        args: &[ConnArg],
    ) -> Type<'db> {
        let recv = self.infer_expr(body, receiver);
        // The L3 rule (planning/numeric_literals.md): a LITERAL receiver
        // takes its type from the first concrete numeric argument
        // (`1 + x` adds at x's type), else `integer` (`-8`, `1 + 2`).
        // Literal-var-only — not general bidirectional inference.
        if self.is_literal_ty(&recv) {
            let from_arg = args.first().and_then(|a| {
                let at = self.infer_expr(body, a.expr);
                let at = self.resolve_ty(&at);
                matches!(
                    at,
                    Type::Value {
                        kind: ValueKind::UInt { .. }
                            | ValueKind::SInt { .. }
                            | ValueKind::Bits { .. }
                            | ValueKind::Integer,
                        ..
                    }
                )
                .then_some(at)
            });
            let target = from_arg.unwrap_or(Type::Value {
                kind: ValueKind::Integer,
                domain: Domain::Const,
            });
            self.unify(&recv, &target);
        }
        let owner = self.owner_of(&recv);

        if let Some(owner) = owner
            && let Some(method_def) = self.map.impl_method(owner, method)
        {
            self.method_resolutions.insert(expr, method_def);
            return self.call_def(body, expr, method_def, args, &[], Some(&recv));
        }
        // Trait-impl candidates for this receiver head (inherent wins above;
        // two traits offering the method is an ambiguity error).
        if let Some(owner) = owner {
            let recv_g = self.resolve_ty(&recv);
            let cands = self.select_by_header(self.map.trait_dispatch(owner, method), &recv_g);
            match cands.as_slice() {
                [] => {}
                [(_, method_def)] => {
                    let method_def = *method_def;
                    self.method_resolutions.insert(expr, method_def);
                    return self.call_def(body, expr, method_def, args, &[], Some(&recv));
                }
                many => {
                    let traits = many
                        .iter()
                        .filter_map(|(t, _)| self.map.def_data(*t).map(|d| d.name.clone()))
                        .collect();
                    self.diag(InferDiagnosticKind::AmbiguousMethod {
                        name: method.to_owned(),
                        traits,
                    });
                    return Type::Error;
                }
            }
        }
        // A `T`-typed receiver (T a bounded type param): trait methods from
        // the param env's bounds on T — the DECL is called; monomorphisation
        // re-selects the impl (Instance::resolve).
        let recv_r = self.resolve_ty(&recv);
        if let Type::Value {
            kind: ValueKind::Param(i),
            ..
        } = recv_r
        {
            let own = sig_of(self.db, self.krate, self.def);
            let mut cands: Vec<(DefId<'db>, DefId<'db>)> = Vec::new();
            for p in &own.predicates {
                let Predicate::Trait(tr) = p;
                let on_this = matches!(
                    &tr.self_ty,
                    Type::Value { kind: ValueKind::Param(j), .. } if *j == i
                );
                if on_this && let Some(m) = self.map.trait_method(tr.trait_def, method) {
                    cands.push((tr.trait_def, m));
                }
            }
            match cands.as_slice() {
                [] => {}
                [(_, m)] => {
                    let m = *m;
                    self.method_resolutions.insert(expr, m);
                    return self.call_def(body, expr, m, args, &[], Some(&recv));
                }
                many => {
                    let traits = many
                        .iter()
                        .filter_map(|(t, _)| self.map.def_data(*t).map(|d| d.name.clone()))
                        .collect();
                    self.diag(InferDiagnosticKind::AmbiguousMethod {
                        name: method.to_owned(),
                        traits,
                    });
                    return Type::Error;
                }
            }
        }
        // Builtin `Vec(N, A).replace(i, x) -> Vec(N, A)` — the FUNCTIONAL
        // single-element update (planning/when_ram.md): a copy with element
        // i swapped. RAM feedback composes it with the value-form `when`.
        {
            let recv_r = self.resolve_ty(&recv);
            if let Type::Vec { len, elem } = &recv_r
                && method == "replace"
            {
                let (len, elem) = (len.clone(), (**elem).clone());
                if let [i, x] = args {
                    let it = self.infer_expr(body, i.expr);
                    if self.is_literal_ty(&it) {
                        let int = Type::Value {
                            kind: ValueKind::Integer,
                            domain: Domain::Const,
                        };
                        self.unify(&it, &int);
                    }
                    let xt = self.infer_expr(body, x.expr);
                    // The element carries its own domain — `x` must fit it.
                    self.subsume(&xt, &elem);
                } else {
                    self.diag(InferDiagnosticKind::PositionalArity {
                        callee: "replace".to_owned(),
                        expected: 2,
                        found: args.len(),
                    });
                }
                return Type::Vec {
                    len,
                    elem: Box::new(elem),
                };
            }
            // Builtin `Vec(N, A).enumerate() -> Vec(N, (integer @const, A))` —
            // a REAL method (planning/tuples.md). Inside a `for` it is also
            // recognised syntactically so the index reuses the genvar.
            if let Type::Vec { len, elem } = &recv_r
                && method == "enumerate"
            {
                let (len, elem) = (len.clone(), (**elem).clone());
                if !args.is_empty() {
                    self.diag(InferDiagnosticKind::PositionalArity {
                        callee: "enumerate".to_owned(),
                        expected: 0,
                        found: args.len(),
                    });
                }
                let index_ty = Type::Value {
                    kind: ValueKind::Integer,
                    domain: Domain::Const,
                };
                return Type::Vec {
                    len,
                    elem: Box::new(Type::Tuple(vec![index_ty, elem])),
                };
            }
        }
        // Prelude methods not in the impl-method index yet (Q3a backfill pending).
        // These builtins are typed structurally and record NO `method_resolution`
        // — that absence is how MIR (`mir::lower`) and the backend tell a builtin
        // from a resolved dispatch. If you add another resolution-less builtin
        // here, add it to `mir::lower::builtin_method` / `mir::ir::BuiltinMethod`
        // too, or MIR lowering will panic on it.
        let args_inferred: Vec<Type<'db>> =
            args.iter().map(|a| self.infer_expr(body, a.expr)).collect();
        let recv = self.resolve_ty(&recv);
        match method {
            // `clk.posedge()` : Clock -> Event @const.
            "posedge" => Type::Value {
                kind: ValueKind::Event,
                domain: Domain::Const,
            },
            // The builtin register:
            //   reg : {dom D: Clock} (self: T @ D, rstn: Reset @ D, init: T @const) -> T @ D
            // One Clock-sorted domain covers the data AND its reset (a reset on
            // another clock is the CDC hazard this rejects). The receiver's
            // domain coerces into D — registering a constant is legal — and
            // the result is ON D regardless.
            "reg" => {
                let d = self.fresh_domain_sorted(DomainSort::Clock);
                if let Some(rd) = self.domain_of(&recv) {
                    self.subsume_domain(rd, d);
                }
                if let [rstn, init] = args_inferred.as_slice() {
                    let want_rst = Type::Value {
                        kind: ValueKind::Reset,
                        domain: d,
                    };
                    self.subsume(rstn, &want_rst);
                    let want_init = self.with_domain(&recv, Domain::Const);
                    self.subsume(init, &want_init);
                } else {
                    self.diag(InferDiagnosticKind::RegForm);
                }
                self.with_domain(&recv, d)
            }
            _ => {
                self.diag(InferDiagnosticKind::UnresolvedMethod {
                    name: method.to_owned(),
                });
                Type::Error
            }
        }
    }

    fn infer_record(
        &mut self,
        body: &Body<'db>,
        ctor: Option<DefId<'db>>,
        fields: &[crate::hir::body::RecordField],
    ) -> Type<'db> {
        let Some(ctor) = ctor else {
            for f in fields {
                self.infer_expr(body, f.value);
            }
            return Type::Error;
        };
        // The constructor is owned by its struct/port type. Instantiate the
        // struct's generic params with fresh vars, unify each field value against
        // the *instantiated* field type, and report the parametric struct type
        // `Struct { def, args }` carrying those (now-constrained) args.
        let owner = self.map.def_data(ctor).and_then(|d| d.owner);
        let field_sig = owner.map(|o| sig_of(self.db, self.krate, o));
        let args = field_sig
            .map(|s| self.fresh_subst(&s.generic_params))
            .unwrap_or_default();
        // A pure (single-domain) struct's unannotated field slots ARE the
        // record's domain — stamping them with `rd` is the head-known
        // discharge of `Ty @ D`. Mixing two clocks in one record then fails
        // at the second field's subsume.
        let rd = self.fresh_domain();
        let rec_span = self.current_span;
        let mut written: Vec<&str> = Vec::new();
        for f in fields {
            let vt = self.infer_expr(body, f.value);
            self.current_span = body.expr_span(f.value);
            if written.contains(&f.name.as_str()) {
                self.diag(InferDiagnosticKind::DuplicateField {
                    name: f.name.clone(),
                });
            }
            written.push(&f.name);
            if let Some(sig) = field_sig {
                match sig.fields.iter().find(|d| d.name == f.name) {
                    Some(decl) => {
                        // Connector must match the field's declared direction:
                        // `=` supplies a field the constructed value drives
                        // (`out`, or any struct field); `=> target` binds an
                        // `in` field flowing back into the constructor's scope.
                        let needs_arrow = decl.direction == Some(Direction::In);
                        if f.out != needs_arrow {
                            self.diag(InferDiagnosticKind::RecordConnector {
                                name: f.name.clone(),
                                needs_arrow,
                            });
                        }
                        let dt = self.substitute_stamped(&decl.ty, &args, rd);
                        if f.out {
                            // The field flows into the target place.
                            self.subsume(&dt, &vt);
                        } else {
                            self.subsume(&vt, &dt);
                        }
                    }
                    None => self.diag(InferDiagnosticKind::UnknownField {
                        name: f.name.clone(),
                    }),
                }
            }
        }
        // Every declared field must be written (an unwritten field would emit
        // as an undriven leaf).
        self.current_span = rec_span;
        if let Some(sig) = field_sig {
            for decl in &sig.fields {
                if !written.contains(&decl.name.as_str()) {
                    self.diag(InferDiagnosticKind::MissingField {
                        name: decl.name.clone(),
                    });
                }
            }
        }
        match owner {
            // A struct ctor and a port ctor yield the same record type; the
            // def's `DefKind` records which (structs_as_ports.md).
            Some(def) => Type::Port {
                def,
                args: GenericArgs(args),
                domain: rd,
            },
            None => Type::Error,
        }
    }

    // ----- helpers -----

    fn owner_of(&mut self, ty: &Type<'db>) -> Option<DefId<'db>> {
        match self.resolve_ty(ty) {
            Type::Value {
                kind: ValueKind::UInt { .. },
                ..
            } => self.prelude_def("uint"),
            Type::Value {
                kind: ValueKind::SInt { .. },
                ..
            } => self.prelude_def("sint"),
            Type::Value {
                kind: ValueKind::Bits { .. },
                ..
            } => self.prelude_def("bits"),
            Type::Value {
                kind: ValueKind::Bool,
                ..
            } => self.prelude_def("bool"),
            Type::Value {
                kind: ValueKind::Integer,
                ..
            } => self.prelude_def("integer"),
            Type::Port { def, .. } => Some(def),
            // `Vec(N, A)` dispatches through the `Vec` builtin owner, so
            // `v.pack()` / `Vec(..)::unpack(b)` find `impl BitPack for Vec(N, A)`
            // (planning/pack_resize.md).
            Type::Vec { .. } => self.prelude_def("Vec"),
            // Tuples dispatch through the synthetic `Tuple` owner, so
            // `t.pack()` / `(A, B)::unpack(b)` find the per-arity tuple impls
            // (planning/pack_resize.md). Arity is disambiguated by header match.
            Type::Tuple(_) => self.prelude_def("Tuple"),
            Type::Clock => self.prelude_def("Clock"),
            _ => None,
        }
    }

    fn prelude_def(&self, name: &str) -> Option<DefId<'db>> {
        self.map
            .resolve_local(self.map.prelude(), name, Namespace::Item)
    }

    /// Build a substitution mapping each of `params`'s indices to a fresh
    /// inference variable of the matching kind (Type/Domain), so a generic
    /// callee is instantiated at the call site. Const generics map to `Deferred`
    /// (widths are a Q4 concern).
    fn fresh_subst(&mut self, params: &[crate::hir::types::GenericParam]) -> Vec<Term<'db>> {
        params
            .iter()
            .map(|p| match p.kind {
                TermKind::Type => Term::Type(self.fresh_type()),
                TermKind::Domain(sort) => Term::Domain(self.fresh_domain_sorted(sort)),
                TermKind::Const => Term::Const(self.fresh_const()),
            })
            .collect()
    }

    /// Instantiate a def's `Param(i)` references from `subst` (rustc's
    /// `EarlyBinder::instantiate`). `Unspecified` domains become fresh
    /// variables on the way through.
    fn substitute(&mut self, ty: &Type<'db>, subst: &[Term<'db>]) -> Type<'db> {
        Substituter {
            table: &mut self.table,
            subst,
            unspecified_to: None,
        }
        .fold_type(ty)
    }

    /// Like [`substitute`](Self::substitute), but every `Unspecified` slot —
    /// including those inside substituted-in argument types — is stamped with
    /// `dom`. The head-known discharge of `Ty @ D`.
    fn substitute_stamped(
        &mut self,
        ty: &Type<'db>,
        subst: &[Term<'db>],
        dom: Domain,
    ) -> Type<'db> {
        Substituter {
            table: &mut self.table,
            subst,
            unspecified_to: Some(dom),
        }
        .fold_type(ty)
    }

    /// The top-level domain of a value/port type (after resolution).
    fn domain_of(&mut self, ty: &Type<'db>) -> Option<Domain> {
        match self.resolve_ty(ty) {
            Type::Value { domain, .. } | Type::Port { domain, .. } => Some(domain),
            _ => None,
        }
    }

    /// `ty` with its top-level domain replaced (resolved first). Non-value
    /// types pass through.
    fn with_domain(&mut self, ty: &Type<'db>, d: Domain) -> Type<'db> {
        match self.resolve_ty(ty) {
            Type::Value { kind, .. } => Type::Value { kind, domain: d },
            Type::Port { def, args, .. } => Type::Port {
                def,
                args,
                domain: d,
            },
            // An aggregate is "on domain d" when every element is — it has no
            // domain of its own (planning/domain_checking.md).
            Type::Vec { len, elem } => Type::Vec {
                len,
                elem: Box::new(self.with_domain(&elem, d)),
            },
            Type::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.with_domain(e, d)).collect())
            }
            other => other,
        }
    }

    /// The clock of a `when`'s event: `clk.posedge()` where `clk` is a body
    /// local backed by a `dom` generic → that generic's `Domain::Param`.
    fn event_clock(&mut self, body: &Body<'db>, event: ExprId) -> Option<Domain> {
        let ExprKind::MethodCall {
            receiver, method, ..
        } = &body.expr(event).kind
        else {
            return None;
        };
        if method != "posedge" {
            return None;
        }
        let ExprKind::Local(l) = body.expr(*receiver).kind else {
            return None;
        };
        let name = &body.locals().get(l.0 as usize)?.name;
        let idx = self
            .own_generics
            .iter()
            .position(|g| matches!(g.kind, TermKind::Domain(_)) && &g.name == name)?;
        Some(Domain::Param(idx as u32))
    }
}

// ----- folders -----------------------------------------------------------

/// Substitution: map every `Param(i)` to `subst[i]` by kind. An `Unspecified`
/// domain becomes a fresh variable — or, with `unspecified_to`, the given
/// domain (record/field stamping; applied inside substituted args too, but
/// WITHOUT re-substituting their `Param`s, which belong to another binder).
struct Substituter<'a, 's, 'db> {
    table: &'a mut InferenceTable<'db>,
    subst: &'s [Term<'db>],
    unspecified_to: Option<Domain>,
}

/// Stamp a fixed domain over every `Unspecified` slot.
struct Stamp {
    dom: Domain,
}

impl<'db> Folder<'db> for Stamp {
    fn fold_domain(&mut self, d: Domain) -> Domain {
        match d {
            Domain::Unspecified => self.dom,
            other => other,
        }
    }
}

impl<'db> Folder<'db> for Substituter<'_, '_, 'db> {
    fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
        match t {
            Type::Value {
                kind: ValueKind::Param(i),
                domain,
            } => match self.subst.get(*i as usize) {
                // The type arg replaces the param; the annotation domain travels
                // with the arg (a simplification — domain stamping is Q7 C/D).
                Some(Term::Type(t)) => match self.unspecified_to {
                    Some(dom) => Stamp { dom }.fold_type(t),
                    None => t.clone(),
                },
                _ => Type::Value {
                    kind: ValueKind::Param(*i),
                    domain: self.fold_domain(*domain),
                },
            },
            other => super_fold_type(self, other),
        }
    }

    fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
        match c {
            ConstArg::Param(i) => match self.subst.get(*i as usize) {
                Some(Term::Const(c)) => c.clone(),
                _ => ConstArg::Deferred,
            },
            other => super_fold_const(self, other),
        }
    }

    fn fold_domain(&mut self, d: Domain) -> Domain {
        match d {
            Domain::Param(i) => match self.subst.get(i as usize) {
                Some(Term::Domain(dom)) => *dom,
                _ => Domain::Unspecified,
            },
            Domain::Unspecified => match self.unspecified_to {
                Some(dom) => dom,
                None => Domain::Infer(self.table.fresh(TermKind::Domain(DomainSort::Domain))),
            },
            other => other,
        }
    }
}

/// End-of-inference resolution: chase every variable, defaulting what is still
/// unbound — types to `Error` (a hole), widths to `Deferred`, domains to
/// `@const`.
struct DeepResolver<'a, 'db> {
    table: &'a mut InferenceTable<'db>,
}

impl<'db> Folder<'db> for DeepResolver<'_, 'db> {
    fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
        match self.table.resolve_type_shallow(t) {
            Type::Infer(_) => Type::Error, // unconstrained — surfaces as a hole
            other => super_fold_type(self, &other),
        }
    }

    fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
        match self.table.resolve_const_shallow(c) {
            // An unbound const var has no determined width yet → `Deferred`.
            ConstArg::Infer(_) => ConstArg::Deferred,
            other => super_fold_const(self, &other),
        }
    }

    fn fold_domain(&mut self, d: Domain) -> Domain {
        match self.table.resolve_domain_shallow(d) {
            // Unconstrained domain var compacts to `@const` (MLsub top).
            Domain::Infer(_) => Domain::Const,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;
    use crate::hir::body::Stmt;

    fn load(db: &mut RootDatabase, vfs: &mut Vfs, text: &str) -> SourceRoot {
        vfs.set_file_text(db, "t.mrn", text);
        vfs.source_root(db, "t.mrn")
    }

    fn def_of<'db>(db: &'db RootDatabase, krate: SourceRoot, name: &str) -> DefId<'db> {
        let map = crate_def_map(db, krate);
        map.resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("def")
    }

    /// The expr typed by the function's whole-result drive — `return EXPR;`
    /// (a unit fn's bare `Stmt::Return`) or, when the fn has a return type, the
    /// desugared whole-result equation `return = EXPR`.
    fn return_ty<'db>(db: &'db RootDatabase, krate: SourceRoot, name: &str) -> Option<Type<'db>> {
        let def = def_of(db, krate, name);
        let b = body(db, krate, def);
        let inf = infer(db, krate, def);
        b.block().stmts.iter().find_map(|s| match s {
            Stmt::Return { value } => inf.expr_type(*value).cloned(),
            Stmt::Equation { lhs, rhs } if is_whole_result_place(b, *lhs) => {
                inf.expr_type(*rhs).cloned()
            }
            _ => None,
        })
    }

    fn kind_str(ty: &Type) -> &'static str {
        match ty {
            Type::Value { kind, .. } => match kind {
                ValueKind::UInt { .. } => "uint",
                ValueKind::SInt { .. } => "sint",
                ValueKind::Bits { .. } => "bits",
                ValueKind::Bool => "bool",
                ValueKind::Reset => "reset",
                ValueKind::Event => "event",
                ValueKind::Integer => "integer",
                ValueKind::Param(_) => "param",
            },
            Type::Vec { .. } => "Vec",
            Type::Tuple(_) => "tuple",
            Type::Port { .. } => "port",
            Type::Clock => "clock",
            Type::Infer(_) => "infer",
            Type::Error => "error",
        }
    }

    #[test]
    fn params_and_return_type_check() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8)) -> uint(8) { return a; }",
        );
        assert_eq!(kind_str(&return_ty(&db, krate, "f").unwrap()), "uint");
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    /// `v + v` unifies a fresh domain var with ITSELF. Regression test for the
    /// self-loop hang: the bind arm wrote `Infer(v) := Infer(v)` and every
    /// later `resolve_*` chased it forever. (Same-term early-out in `unify`,
    /// `unify_width`, `unify_domain`.)
    #[test]
    fn same_operand_unification_terminates() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn double(v: uint(8)) -> uint(8) { return v + v; }",
        );
        assert_eq!(kind_str(&return_ty(&db, krate, "double").unwrap()), "uint");
        let inf = infer(&db, krate, def_of(&db, krate, "double"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn let_binding_is_inferred_from_its_value() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn g (a: uint(8)) -> uint(8) { let b = a; return b; }",
        );
        assert_eq!(kind_str(&return_ty(&db, krate, "g").unwrap()), "uint");
        let inf = infer(&db, krate, def_of(&db, krate, "g"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn binary_op_result_unifies_operands() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn h (a: uint(8)) -> uint(8) { return a + 1; }",
        );
        // `a + 1`: a is uint@<var>, 1 is uint@const → unifies, result uint.
        assert_eq!(kind_str(&return_ty(&db, krate, "h").unwrap()), "uint");
        let inf = infer(&db, krate, def_of(&db, krate, "h"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn returning_the_wrong_type_is_a_mismatch() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(&mut db, &mut vfs, "fn m (a: uint(8)) -> bool { return a; }");
        let inf = infer(&db, krate, def_of(&db, krate, "m"));
        assert!(
            inf.diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, InferDiagnosticKind::TypeMismatch)),
            "expected a type mismatch, got {:?}",
            inf.diagnostics()
        );
    }

    #[test]
    fn mismatched_literal_widths_are_caught() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // Returning a uint(8) where uint(16) is declared — a width mismatch.
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8)) -> uint(16) { return a; }",
        );
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(
            inf.diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, InferDiagnosticKind::WidthMismatch)),
            "expected a width mismatch, got {:?}",
            inf.diagnostics()
        );
    }

    #[test]
    fn matching_and_symbolic_widths_do_not_error() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // Equal literal widths: fine. Two distinct generic widths constrained
        // equal by `a + b` is a residual (Q4b), not a Q4a error.
        let krate = load(
            &mut db,
            &mut vfs,
            "fn ok (a: uint(8), b: uint(8)) -> uint(8) { return a + b; }\nfn sym { const n: integer, const m: integer } (a: uint(n), b: uint(m)) -> uint(n) { return a + b; }",
        );
        assert!(
            infer(&db, krate, def_of(&db, krate, "ok"))
                .diagnostics()
                .is_empty()
        );
        assert!(
            infer(&db, krate, def_of(&db, krate, "sym"))
                .diagnostics()
                .is_empty(),
            "n ~ m is a residual, not a width mismatch"
        );
    }

    #[test]
    fn closed_unevaluable_width_errors_symbolic_one_defers() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // `uint(8 / 0)` is closed but has no value → UnevaluableWidth. The
        // parametric `uint(n / 2)` is symbolic → defers, no error
        // (planning/operators.md, planning/const_eval.md).
        let krate = load(
            &mut db,
            &mut vfs,
            "fn bad () -> uint(1) { let x: uint(8 / 0) = 0; return x; }\n\
             fn sym { const n: integer } (x: uint(n)) -> uint(n / 2) { return x; }",
        );
        assert!(
            infer(&db, krate, def_of(&db, krate, "bad"))
                .diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, InferDiagnosticKind::UnevaluableWidth)),
            "expected UnevaluableWidth for uint(8 / 0)"
        );
        assert!(
            !infer(&db, krate, def_of(&db, krate, "sym"))
                .diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, InferDiagnosticKind::UnevaluableWidth)),
            "a symbolic width uint(n / 2) must defer, not error"
        );
    }

    /// A width naming a body local (`let y: uint(n) = …`) lowers to
    /// `ConstArg::Local`: a `@const` local survives as a recorded residual
    /// (const_eval's job), while a clocked local is a `ClockedWidth` error —
    /// the old `Deferred` path silently unified both with anything.
    #[test]
    fn local_width_const_is_residual_clocked_is_error() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn ok (x: uint(8)) -> uint(8) { let n = 8; let y: uint(n) = x; return y; }\n\
             fn wrong (x: uint(8)) -> uint(8) { let n = 4; let y: uint(n) = x; return y; }\n\
             fn bad {dom clk: Clock} (x: uint(8) @clk, n: uint(4) @clk) -> uint(8) @clk { let y: uint(n) @clk = x; return y; }",
        );
        // const_eval grounds the local: uint(n)=uint(8) discharges with no
        // residual; n=4 against uint(8) is a hard WidthMismatch (both were
        // undecidable Local residuals before Q4c).
        let inf = infer(&db, krate, def_of(&db, krate, "ok"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
        assert!(
            inf.const_residuals().is_empty(),
            "an evaluable local width should discharge, got {:?}",
            inf.const_residuals()
        );
        let inf = infer(&db, krate, def_of(&db, krate, "wrong"));
        assert!(
            inf.diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, InferDiagnosticKind::WidthMismatch)),
            "n=4 against uint(8) must mismatch, got {:?}",
            inf.diagnostics()
        );
        let inf = infer(&db, krate, def_of(&db, krate, "bad"));
        assert!(
            inf.diagnostics().iter().any(|d| matches!(
                &d.kind,
                // The L7 wrap guard rejects the hardware-typed width before
                // the domain check even looks: uint values can't be widths
                // at all (planning/numeric_literals.md).
                InferDiagnosticKind::WidthNotInteger { .. }
            )),
            "a clocked uint width must be rejected, got {:?}",
            inf.diagnostics()
        );
    }

    #[test]
    fn call_uses_the_callees_signature() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn callee (x: uint(8)) -> bool { return x; }\nfn caller (a: uint(8)) -> bool { return callee(a); }",
        );
        // caller's return type is the callee's return type (bool).
        assert_eq!(kind_str(&return_ty(&db, krate, "caller").unwrap()), "bool");
        let inf = infer(&db, krate, def_of(&db, krate, "caller"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn parametric_struct_field_is_instantiated_at_access() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // `b: Bus(uint(8))`, field `data: A` → `b.data` instantiates A = uint(8).
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Bus(type A) = bus { valid: bool, data: A }\nfn f (b: Bus(uint(8))) -> uint(8) { return b.data; }",
        );
        assert_eq!(kind_str(&return_ty(&db, krate, "f").unwrap()), "uint");
        assert!(
            infer(&db, krate, def_of(&db, krate, "f"))
                .diagnostics()
                .is_empty()
        );
    }

    #[test]
    fn a_parametric_field_type_mismatch_is_caught() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // `b: Bus(bool)` → `b.data` is bool, but the fn returns uint(8): mismatch.
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Bus(type A) = bus { valid: bool, data: A }\nfn f (b: Bus(bool)) -> uint(8) { return b.data; }",
        );
        assert!(
            infer(&db, krate, def_of(&db, krate, "f"))
                .diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, InferDiagnosticKind::TypeMismatch)),
            "instantiating A=bool should make `b.data` (bool) clash with uint(8)"
        );
    }

    #[test]
    fn constructing_a_parametric_struct_infers_its_arg() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // `bus { valid: true, data: 0 }` infers A from the `data` value; the
        // result unifies cleanly with the declared `Bus(uint(8))` return.
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Bus(type A) = bus { valid: bool, data: A }\nfn f () -> Bus(uint(8)) { return bus { valid = true, data = 0 }; }",
        );
        assert!(
            infer(&db, krate, def_of(&db, krate, "f"))
                .diagnostics()
                .is_empty(),
            "{:?}",
            infer(&db, krate, def_of(&db, krate, "f")).diagnostics()
        );
    }

    #[test]
    fn caller_inference_survives_a_callee_body_edit() {
        // THE firewall at the type layer: infer(caller) depends on the callee's
        // signature, not its body, so editing the callee body leaves it unchanged.
        fn summary(db: &RootDatabase, krate: SourceRoot) -> (String, usize) {
            let def = def_of(db, krate, "caller");
            let inf = infer(db, krate, def);
            let ret = return_ty(db, krate, "caller")
                .map(|t| kind_str(&t).to_owned())
                .unwrap_or_default();
            (ret, inf.diagnostics().len())
        }
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(
            &mut db,
            "t.mrn",
            "fn callee (x: uint(8)) -> bool { return x; }\nfn caller (a: uint(8)) -> bool { return callee(a); }",
        );
        let krate = vfs.source_root(&mut db, "t.mrn");
        let before = summary(&db, krate);
        // Edit only the callee's BODY (signature unchanged).
        vfs.set_file_text(
            &mut db,
            "t.mrn",
            "fn callee (x: uint(8)) -> bool { return x + x; }\nfn caller (a: uint(8)) -> bool { return callee(a); }",
        );
        let after = summary(&db, krate);
        assert_eq!(
            before, after,
            "a callee body edit must not change caller inference"
        );
        assert_eq!(
            before.0, "bool",
            "the summary should observe the return type"
        );
    }

    #[test]
    fn tuple_construction_projection_and_destructuring_type_check() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8), b: bool) -> uint(8) {
                 let p = (a, b);
                 let (x, y) = p;
                 return p.0 + x;
             }",
        );
        assert_eq!(kind_str(&return_ty(&db, krate, "f").unwrap()), "uint");
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn tuple_return_type_checks_element_wise() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8), b: bool) -> (uint(8), bool) { return (a, b); }",
        );
        assert_eq!(kind_str(&return_ty(&db, krate, "f").unwrap()), "tuple");
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn tuple_projection_past_arity_is_diagnosed() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8), b: bool) -> uint(8) { return (a, b).2; }",
        );
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(
            inf.diagnostics().iter().any(|d| matches!(
                d.kind,
                InferDiagnosticKind::TupleIndexOutOfBounds { index: 2, arity: 2 }
            )),
            "{:?}",
            inf.diagnostics()
        );
    }

    #[test]
    fn tuple_arity_mismatch_is_diagnosed() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8), b: bool) -> (uint(8), bool) { return (a, b, a); }",
        );
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(
            inf.diagnostics()
                .iter()
                .any(|d| matches!(d.kind, InferDiagnosticKind::TupleArityMismatch { .. })),
            "{:?}",
            inf.diagnostics()
        );
    }

    #[test]
    fn const_elements_coerce_into_clocked_tuple_slots() {
        // Element-wise subsumption: a literal element fits a clocked slot.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f {dom clk: Clock} (a: uint(8) @clk) -> (uint(8), uint(4)) @clk {
                 return (a, 5);
             }",
        );
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn enumerate_yields_index_data_pairs_consumed_by_projection() {
        // The real value-form usage: bind enumerate's result and project
        // elements. The index element is `@const` (from enumerate's own
        // type), the data element keeps the receiver's domain — no special
        // integer rule is involved.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (v: Vec(3, uint(8))) -> uint(8) {
                 let e = v.enumerate();
                 return e[0].1 + e[1].1 + e[2].1;
             }",
        );
        assert_eq!(kind_str(&return_ty(&db, krate, "f").unwrap()), "uint");
        let inf = infer(&db, krate, def_of(&db, krate, "f"));
        assert!(inf.diagnostics().is_empty(), "{:?}", inf.diagnostics());
    }

    #[test]
    fn integer_carries_an_explicit_clock_domain() {
        // `integer` is not pinned to `@const` by the type system — an
        // explicit `@clk` is a legitimate non-const integer (a testbench
        // counter). Its domain follows annotation/data flow, like any type.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn tb {dom clk: Clock} (n: integer @clk) -> integer @clk { return n; }",
        );
        let tb = infer(&db, krate, def_of(&db, krate, "tb"));
        assert!(tb.diagnostics().is_empty(), "{:?}", tb.diagnostics());
    }

    #[test]
    fn mixed_domain_tuples_keep_per_element_clocks() {
        // The fully-polymorphic case: two clocks in one tuple; projecting
        // each element recovers its own domain, and crossing them into one
        // add is a domain mismatch.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn ok {dom a: Clock, dom b: Clock} (x: uint(8) @a, y: uint(8) @b)
                 -> (uint(8) @a, uint(8) @b) {
                 let t: (uint(8) @a, uint(8) @b) = (x, y);
                 return (t.0, t.1);
             }
             fn bad {dom a: Clock, dom b: Clock} (x: uint(8) @a, y: uint(8) @b)
                 -> uint(8) @a {
                 let t: (uint(8) @a, uint(8) @b) = (x, y);
                 return t.0 + t.1;
             }",
        );
        let ok = infer(&db, krate, def_of(&db, krate, "ok"));
        assert!(ok.diagnostics().is_empty(), "{:?}", ok.diagnostics());
        let bad = infer(&db, krate, def_of(&db, krate, "bad"));
        assert!(
            !bad.diagnostics().is_empty(),
            "crossing two domains through tuple projections must be diagnosed"
        );
    }
}
