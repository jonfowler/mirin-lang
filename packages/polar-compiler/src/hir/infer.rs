//! `infer(def)` — type + domain inference (`planning/q3_typed_hir.md` §2, §6.4).
//!
//! An eager-unification walk over a function's [`body`](crate::hir::body), the
//! per-fn `InferCtxt` of the old `typeck` lifted onto a query. Produces a type
//! for every expression and local, the resolved callee of every method call, and
//! diagnostics. Depends on `body(self)`, `sig_of(self)`, and `sig_of` of the
//! callees/structs/ports it touches — **never their bodies**, so a caller
//! re-infers only when a callee's *signature* changes (the firewall).
//!
//! Per `domain_checking_redux.md`, the **domain is a component of the type**,
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
    ConstArg, Direction, Domain, DomainSort, Folder, GenericArgs, GenericParam, InferVar, LocalId,
    Predicate, Term, TermKind, Type, ValueKind, super_fold_const, super_fold_type, type_has_infer,
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
    /// A uint width whose const evaluation came out negative.
    NegativeWidth {
        value: i128,
    },
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
}

impl InferDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
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
        }
    }
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

    let ret = sig.return_type.as_ref().map(|t| cx.freshen_domains(t));
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
}

/// The body locals referenced in const (width) position anywhere in `ty`.
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
                kind: ValueKind::UInt { width },
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
            obligations: Vec::new(),
            current_span: Span::default(),
        }
    }

    /// Try to const-evaluate a width tree in this def's body (soft failure).
    fn try_eval(&self, c: &ConstArg<'db>) -> Option<i128> {
        crate::hir::const_eval::eval_const(self.db, self.krate, self.def, c)
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
        }
    }

    /// Evaluate every ground width in the final types; a negative result is a
    /// hard error (`integer` maths may go negative in intermediates, a uint
    /// width may not come out negative). One diagnostic per distinct tree.
    fn check_widths(&mut self) {
        let mut seen: std::collections::HashSet<ConstArg> = Default::default();
        let mut bad: Vec<(Span, i128)> = Vec::new();
        let locals: Vec<(LocalId, Type<'db>)> = self
            .local_types
            .iter()
            .map(|(l, t)| (*l, t.clone()))
            .collect();
        for (l, t) in locals {
            for w in collect_widths(&t) {
                if seen.insert(w.clone())
                    && let Some(v) = self.try_eval(&w)
                    && v < 0
                {
                    bad.push((self.body.local_span(l), v));
                }
            }
        }
        for (span, value) in bad {
            self.current_span = span;
            self.diag(InferDiagnosticKind::NegativeWidth { value });
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
                break;
            }
        }
        for ob in std::mem::take(&mut self.obligations) {
            match ob.kind {
                ObligationKind::ConstEq(a, b) => residuals.push((a, b)),
                ObligationKind::ConstDomain(_) => {}
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
                // Top-level only: an aggregate's inner `Unspecified` slots are
                // the aggregate's own domain — stamped at field/record use and
                // by flatten in the backend, never independent variables.
                kind: kind.clone(),
                domain: self.freshen_domain(*domain),
            },
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
            (Type::Clock, Type::Clock) => {}
            _ => self.diag(InferDiagnosticKind::TypeMismatch),
        }
    }

    fn unify_kind(&mut self, a: &ValueKind<'db>, b: &ValueKind<'db>) {
        match (a, b) {
            (ValueKind::UInt { width: wa }, ValueKind::UInt { width: wb }) => {
                self.unify_width(wa.clone(), wb.clone());
            }
            (ValueKind::Bool, ValueKind::Bool)
            | (ValueKind::Reset, ValueKind::Reset)
            | (ValueKind::Event, ValueKind::Event)
            | (ValueKind::Integer, ValueKind::Integer) => {}
            // Literal polymorphism, approximated: a number literal infers as
            // `uint` of a fresh width, but must also serve `integer` const
            // positions (`pick(false, 8, 16)`). Until literals get their own
            // inference type, `integer ~ uint` is accepted leniently.
            (ValueKind::Integer, ValueKind::UInt { .. })
            | (ValueKind::UInt { .. }, ValueKind::Integer) => {}
            (ValueKind::Struct { def: x, args: ax }, ValueKind::Struct { def: y, args: ay })
                if x == y =>
            {
                self.unify_args(ax, ay)
            }
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
                    self.subsume(&r, &l);
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
            // A literal is `uint @const` of an inferred width — a fresh const var
            // that unifies to the width its context demands.
            ExprKind::Number(_) => {
                let width = self.fresh_const();
                Type::Value {
                    kind: ValueKind::UInt { width },
                    domain: Domain::Const,
                }
            }
            ExprKind::Bool(_) => Type::Value {
                kind: ValueKind::Bool,
                domain: Domain::Const,
            },
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
            ExprKind::When { event, body: inner } => {
                self.infer_expr(body, *event);
                let inner = inner.clone();
                let result = self.fresh_type();
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
        // Prelude `+` / `*`: operands share a structural kind; the result's
        // domain is the JOIN of operand domains (`@const` absorbs — `x + 3`
        // stays on x's clock, `3 + 4` stays const).
        if self.is_prelude_op(def) {
            let result = self.fresh_type();
            for a in args {
                let t = self.infer_expr(body, a.expr);
                self.merge_branch(&result, &t);
            }
            return result;
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
        // The field type is declared in terms of the def's generic params; we
        // instantiate it with the receiver's type args (`EarlyBinder::instantiate`).
        let (def, args) = match self.resolve_ty(&recv) {
            Type::Value {
                kind: ValueKind::Struct { def, args },
                ..
            } => (def, args),
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

    fn infer_method(
        &mut self,
        body: &Body<'db>,
        expr: ExprId,
        receiver: ExprId,
        method: &str,
        args: &[ConnArg],
    ) -> Type<'db> {
        let recv = self.infer_expr(body, receiver);
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
            match self.map.trait_dispatch(owner, method) {
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
        // Prelude methods not in the impl-method index yet (Q3a backfill pending).
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
            Some(def) => {
                // A port constructor yields a port value; a struct ctor a struct.
                if self.map.def_data(def).map(|d| d.kind) == Some(DefKind::Port) {
                    Type::Port {
                        def,
                        args: GenericArgs(args),
                        domain: rd,
                    }
                } else {
                    Type::Value {
                        kind: ValueKind::Struct {
                            def,
                            args: GenericArgs(args),
                        },
                        domain: rd,
                    }
                }
            }
            None => Type::Error,
        }
    }

    // ----- helpers -----

    fn owner_of(&mut self, ty: &Type<'db>) -> Option<DefId<'db>> {
        match self.resolve_ty(ty) {
            Type::Value {
                kind: ValueKind::Struct { def, .. },
                ..
            } => Some(def),
            Type::Value {
                kind: ValueKind::UInt { .. },
                ..
            } => self.prelude_def("uint"),
            Type::Value {
                kind: ValueKind::Bool,
                ..
            } => self.prelude_def("bool"),
            Type::Port { def, .. } => Some(def),
            Type::Clock => self.prelude_def("Clock"),
            _ => None,
        }
    }

    fn prelude_def(&self, name: &str) -> Option<DefId<'db>> {
        self.map
            .resolve_local(self.map.prelude(), name, Namespace::Item)
    }

    fn is_prelude_op(&self, def: DefId<'db>) -> bool {
        self.map
            .def_data(def)
            .map(|d| {
                d.module == self.map.prelude() && (d.name == "+" || d.name == "-" || d.name == "*")
            })
            .unwrap_or(false)
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
        vfs.set_file_text(db, "t.plr", text);
        vfs.source_root(db, "t.plr")
    }

    fn def_of<'db>(db: &'db RootDatabase, krate: SourceRoot, name: &str) -> DefId<'db> {
        let map = crate_def_map(db, krate);
        map.resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("def")
    }

    /// The expr typed by the function's first `return` statement.
    fn return_ty<'db>(db: &'db RootDatabase, krate: SourceRoot, name: &str) -> Option<Type<'db>> {
        let def = def_of(db, krate, name);
        let b = body(db, krate, def);
        let inf = infer(db, krate, def);
        b.block().stmts.iter().find_map(|s| match s {
            Stmt::Return { value } => inf.expr_type(*value).cloned(),
            _ => None,
        })
    }

    fn kind_str(ty: &Type) -> &'static str {
        match ty {
            Type::Value { kind, .. } => match kind {
                ValueKind::UInt { .. } => "uint",
                ValueKind::Bool => "bool",
                ValueKind::Reset => "reset",
                ValueKind::Event => "event",
                ValueKind::Integer => "integer",
                ValueKind::Struct { .. } => "struct",
                ValueKind::Param(_) => "param",
            },
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
            "fn ok (a: uint(8), b: uint(8)) -> uint(8) { return a + b; }\nfn sym { param n: integer, param m: integer } (a: uint(n), b: uint(m)) -> uint(n) { return a + b; }",
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
            inf.diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, InferDiagnosticKind::ClockedWidth)),
            "a clocked width must be rejected, got {:?}",
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
            "struct Bus(A: Type) = bus { valid: bool, data: A }\nfn f (b: Bus(uint(8))) -> uint(8) { return b.data; }",
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
            "struct Bus(A: Type) = bus { valid: bool, data: A }\nfn f (b: Bus(bool)) -> uint(8) { return b.data; }",
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
            "struct Bus(A: Type) = bus { valid: bool, data: A }\nfn f () -> Bus(uint(8)) { return bus { valid = true, data = 0 }; }",
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
            "t.plr",
            "fn callee (x: uint(8)) -> bool { return x; }\nfn caller (a: uint(8)) -> bool { return callee(a); }",
        );
        let krate = vfs.source_root(&mut db, "t.plr");
        let before = summary(&db, krate);
        // Edit only the callee's BODY (signature unchanged).
        vfs.set_file_text(
            &mut db,
            "t.plr",
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
}
