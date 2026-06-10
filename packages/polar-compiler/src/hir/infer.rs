//! `infer(def)` — type + domain inference (`planning/q3_typed_hir.md` §2, §6.4).
//!
//! An eager-unification walk over a function's [`body`](crate::hir::body), the
//! per-fn `InferCtxt` of the old `typeck` lifted onto a query. Produces a type
//! for every expression and local, the resolved callee of every method call, and
//! diagnostics. Depends on `body(self)`, `sig_of(self)`, and `sig_of` of the
//! callees/structs/ports it touches — **never their bodies**, so a caller
//! re-infers only when a callee's *signature* changes (the firewall).
//!
//! Per `domain_checking.md`, the **domain is a component of the type**, inferred
//! by the same walk but with its own lattice: `@const` is a supertype of every
//! concrete clock, an unconstrained domain variable defaults to `@const`. It is
//! not a parallel solve.
//!
//! **Scope:** structural-kind + domain inference for the monomorphic core, with
//! generic callees instantiated by substituting their `Param`s with fresh
//! variables. **Widths are checked** (Q4a): a literal's width and a Const-kind
//! generic both infer through a `const_vars` pool, and two ground literal widths
//! that disagree are a `WidthMismatch`. Symbolic widths — generic params,
//! arithmetic, anon-consts — are accepted here and deferred to the residual +
//! `const_eval` machinery (Q4b/c, `planning/q4_const_eval.md`).

use std::collections::HashMap;

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::hir::body::{Body, ConnArg, ExprId, ExprKind, NamedArg, Stmt, body};
use crate::hir::sig::sig_of;
use crate::hir::types::{
    ConstArg, Domain, GenericArg, GenericArgs, GenericParamKind, LocalId, Type, ValueKind,
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
    /// A `recv.method(…)` whose method did not resolve on the receiver's type.
    UnresolvedMethod { name: String },
}

impl InferDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            InferDiagnosticKind::TypeMismatch => "type mismatch".to_owned(),
            InferDiagnosticKind::WidthMismatch => "mismatched `uint` widths".to_owned(),
            InferDiagnosticKind::DomainMismatch => "mismatched clock domains".to_owned(),
            InferDiagnosticKind::UnresolvedMethod { name } => {
                format!("no method `{name}` on this type")
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
    diagnostics: Vec<InferDiagnostic>,
    /// Width equalities that could not be decided here because both sides are
    /// symbolic generic params (`uint(n)` vs `uint(m)`). Not an error — the
    /// back end discharges them as `initial assert (n == m)` (the start of the
    /// Q4b residual machinery; full `const_eval` is still deferred).
    width_residuals: Vec<(u32, u32)>,
}

impl<'db> Inference<'db> {
    pub fn expr_type(&self, e: ExprId) -> Option<&Type<'db>> {
        self.expr_types.get(&e)
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

    /// Unresolved width equalities between two generic-param indices (`(n, m)`),
    /// for the back end's `initial assert`.
    pub fn width_residuals(&self) -> &[(u32, u32)] {
        &self.width_residuals
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

    let mut cx = InferCtx::new(db, krate, map);

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
            let declared = cx.freshen_domains(declared);
            cx.unify(&var, &declared);
        }
    }

    let ret = sig.return_type.as_ref().map(|t| cx.freshen_domains(t));
    cx.infer_block(body, body.block(), ret.as_ref());
    cx.finish()
}

struct InferCtx<'a, 'db> {
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    map: &'a CrateDefMap<'db>,
    type_vars: Vec<Option<Type<'db>>>,
    domain_vars: Vec<Option<Domain>>,
    const_vars: Vec<Option<ConstArg>>,
    expr_types: HashMap<ExprId, Type<'db>>,
    local_types: HashMap<LocalId, Type<'db>>,
    method_resolutions: HashMap<ExprId, DefId<'db>>,
    diagnostics: Vec<InferDiagnostic>,
    width_residuals: Vec<(u32, u32)>,
    /// Def-relative span of the expression currently under inference — attached
    /// to any diagnostic raised while unifying it.
    current_span: Span,
}

impl<'a, 'db> InferCtx<'a, 'db> {
    fn new(db: &'db dyn salsa::Database, krate: SourceRoot, map: &'a CrateDefMap<'db>) -> Self {
        Self {
            db,
            krate,
            map,
            type_vars: Vec::new(),
            domain_vars: Vec::new(),
            const_vars: Vec::new(),
            expr_types: HashMap::new(),
            local_types: HashMap::new(),
            method_resolutions: HashMap::new(),
            diagnostics: Vec::new(),
            width_residuals: Vec::new(),
            current_span: Span::default(),
        }
    }

    fn finish(mut self) -> Inference<'db> {
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
        Inference {
            expr_types: self.expr_types,
            local_types: self.local_types,
            method_resolutions: self.method_resolutions,
            diagnostics: self.diagnostics,
            width_residuals: self.width_residuals,
        }
    }

    // ----- inference variables -----

    fn fresh_type(&mut self) -> Type<'db> {
        self.type_vars.push(None);
        Type::Infer(self.type_vars.len() as u32 - 1)
    }

    fn fresh_domain(&mut self) -> Domain {
        self.domain_vars.push(None);
        Domain::Infer(self.domain_vars.len() as u32 - 1)
    }

    fn fresh_const(&mut self) -> ConstArg {
        self.const_vars.push(None);
        ConstArg::Infer(self.const_vars.len() as u32 - 1)
    }

    fn resolve_const(&self, w: &ConstArg) -> ConstArg {
        let mut cur = w.clone();
        while let ConstArg::Infer(v) = cur {
            match &self.const_vars[v as usize] {
                Some(c) => cur = c.clone(),
                None => break,
            }
        }
        cur
    }

    /// Replace every `Unspecified` domain in `ty` with a fresh domain var, so an
    /// un-annotated type's domain can be inferred. Concrete/`Param` domains stay.
    fn freshen_domains(&mut self, ty: &Type<'db>) -> Type<'db> {
        match ty {
            Type::Value { kind, domain } => Type::Value {
                kind: self.freshen_kind_domains(kind),
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

    fn freshen_kind_domains(&mut self, kind: &ValueKind<'db>) -> ValueKind<'db> {
        // Only the top-level domain matters for the scalar kinds we infer here.
        kind.clone()
    }

    fn freshen_domain(&mut self, d: Domain) -> Domain {
        match d {
            Domain::Unspecified => self.fresh_domain(),
            other => other,
        }
    }

    fn resolve_ty(&self, ty: &Type<'db>) -> Type<'db> {
        let mut cur = ty.clone();
        while let Type::Infer(v) = cur {
            match &self.type_vars[v as usize] {
                Some(t) => cur = t.clone(),
                None => break,
            }
        }
        cur
    }

    fn resolve_domain(&self, d: Domain) -> Domain {
        let mut cur = d;
        while let Domain::Infer(v) = cur {
            match self.domain_vars[v as usize] {
                Some(t) => cur = t,
                None => break,
            }
        }
        cur
    }

    fn deep_resolve(&self, ty: &Type<'db>) -> Type<'db> {
        match self.resolve_ty(ty) {
            Type::Value { kind, domain } => Type::Value {
                kind: self.deep_resolve_kind(&kind),
                domain: self.resolve_domain_default(domain),
            },
            Type::Port { def, args, domain } => Type::Port {
                def,
                args: self.deep_resolve_args(&args),
                domain: self.resolve_domain_default(domain),
            },
            Type::Infer(_) => Type::Error, // unconstrained — surfaces as a hole
            other => other,
        }
    }

    fn deep_resolve_kind(&self, kind: &ValueKind<'db>) -> ValueKind<'db> {
        match kind {
            ValueKind::UInt { width } => ValueKind::UInt {
                width: self.deep_resolve_const(width),
            },
            ValueKind::Struct { def, args } => ValueKind::Struct {
                def: *def,
                args: self.deep_resolve_args(args),
            },
            other => other.clone(),
        }
    }

    /// An unbound const var has no determined width yet → `Deferred`.
    fn deep_resolve_const(&self, w: &ConstArg) -> ConstArg {
        match self.resolve_const(w) {
            ConstArg::Infer(_) => ConstArg::Deferred,
            other => other,
        }
    }

    fn deep_resolve_args(&self, args: &GenericArgs<'db>) -> GenericArgs<'db> {
        GenericArgs(
            args.0
                .iter()
                .map(|a| match a {
                    GenericArg::Type(t) => GenericArg::Type(self.deep_resolve(t)),
                    GenericArg::Const(c) => GenericArg::Const(self.deep_resolve_const(c)),
                    GenericArg::Domain(d) => GenericArg::Domain(self.resolve_domain_default(*d)),
                })
                .collect(),
        )
    }

    fn resolve_domain_default(&self, d: Domain) -> Domain {
        match self.resolve_domain(d) {
            // Unconstrained domain var compacts to `@const` (MLsub top).
            Domain::Infer(_) => Domain::Const,
            other => other,
        }
    }

    // ----- unification -----

    fn unify(&mut self, a: &Type<'db>, b: &Type<'db>) {
        let a = self.resolve_ty(a);
        let b = self.resolve_ty(b);
        // Same term — nothing to do. Crucially this covers the same *unbound
        // variable* on both sides (`v + v`): without it the bind arm writes
        // `Infer(v) := Infer(v)` and `resolve_*` chases the self-loop forever.
        if a == b {
            return;
        }
        match (&a, &b) {
            (Type::Infer(v), _) => self.type_vars[*v as usize] = Some(b.clone()),
            (_, Type::Infer(v)) => self.type_vars[*v as usize] = Some(a.clone()),
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
            | (ValueKind::Usize, ValueKind::Usize) => {}
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
                (GenericArg::Type(tx), GenericArg::Type(ty)) => self.unify(tx, ty),
                (GenericArg::Const(cx), GenericArg::Const(cy)) => {
                    self.unify_width(cx.clone(), cy.clone())
                }
                (GenericArg::Domain(dx), GenericArg::Domain(dy)) => self.unify_domain(*dx, *dy),
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
    fn unify_width(&mut self, a: ConstArg, b: ConstArg) {
        let a = self.resolve_const(&a);
        let b = self.resolve_const(&b);
        if a == b {
            return; // incl. the same unbound var on both sides — see `unify`
        }
        match (&a, &b) {
            (ConstArg::Infer(v), _) => self.const_vars[*v as usize] = Some(b.clone()),
            (_, ConstArg::Infer(v)) => self.const_vars[*v as usize] = Some(a.clone()),
            (ConstArg::Lit(x), ConstArg::Lit(y)) if x != y => {
                self.diag(InferDiagnosticKind::WidthMismatch)
            }
            // Two distinct generic-param widths can't be decided here — record
            // the obligation for the back end's `initial assert` (Q4b residual).
            (ConstArg::Param(x), ConstArg::Param(y)) if x != y => {
                self.width_residuals.push((*x, *y));
            }
            // Ground-equal, or otherwise symbolic (deferred) → defer to Q4b.
            _ => {}
        }
    }

    fn unify_domain(&mut self, a: Domain, b: Domain) {
        let a = self.resolve_domain(a);
        let b = self.resolve_domain(b);
        if a == b {
            return; // incl. the same unbound var on both sides — see `unify`
        }
        match (a, b) {
            (Domain::Infer(v), other) | (other, Domain::Infer(v)) => {
                self.domain_vars[v as usize] = Some(other);
            }
            // `@const` is a subtype of every domain — compatible with anything.
            (Domain::Const, _) | (_, Domain::Const) => {}
            (Domain::Clock(x), Domain::Clock(y)) if x == y => {}
            (Domain::Param(x), Domain::Param(y)) if x == y => {}
            (Domain::Unspecified, _) | (_, Domain::Unspecified) => {}
            _ => self.diag(InferDiagnosticKind::DomainMismatch),
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
                    self.unify(&lt, &vt);
                }
                Stmt::VarDecl { .. } => {}
                Stmt::Equation { lhs, rhs } => {
                    let l = self.infer_expr(body, *lhs);
                    let r = self.infer_expr(body, *rhs);
                    self.unify(&l, &r);
                }
                Stmt::Return { value } => {
                    let v = self.infer_expr(body, *value);
                    if let Some(ret) = ret {
                        self.unify(ret, &v);
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
                self.unify(ret, &t);
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
            } => self.infer_call(body, *callee, args, named),
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
        // Prelude `+` / `*`: both operands share a type; the result is that type.
        if self.is_prelude_op(def) {
            let mut acc: Option<Type<'db>> = None;
            for a in args {
                let t = self.infer_expr(body, a.expr);
                match &acc {
                    Some(prev) => self.unify(prev, &t),
                    None => acc = Some(t),
                }
            }
            return acc.unwrap_or(Type::Error);
        }
        self.call_def(body, def, args, named, false)
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
        def: DefId<'db>,
        args: &[ConnArg],
        named: &[NamedArg],
        skip_self: bool,
    ) -> Type<'db> {
        let sig = sig_of(self.db, self.krate, def);
        let subst = self.fresh_subst(&sig.generic_params);

        // Positional args bind the positional value params (in declared order),
        // skipping `self` for a method call.
        let positional: Vec<&super::sig::Param<'db>> = sig
            .params
            .iter()
            .filter(|p| !p.from_named_section && (!skip_self || !p.is_self))
            .collect();
        for (i, a) in args.iter().enumerate() {
            let at = self.infer_expr(body, a.expr);
            if let Some(p) = positional.get(i) {
                let pt = self.substitute(&p.ty, &subst);
                self.unify(&pt, &at);
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
                self.unify(&pt, &at);
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
            _ => return Type::Error,
        };
        let sig = sig_of(self.db, self.krate, def);
        match sig.fields.iter().find(|f| f.name == field) {
            Some(f) => self.substitute(&f.ty, &args.0),
            None => Type::Error,
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
            return self.call_def(body, method_def, args, &[], true);
        }
        // Prelude methods not in the impl-method index yet (Q3a backfill pending).
        for a in args {
            self.infer_expr(body, a.expr);
        }
        match method {
            // `clk.posedge()` : Clock -> Event @const.
            "posedge" => Type::Value {
                kind: ValueKind::Event,
                domain: Domain::Const,
            },
            // `x.reg(…)` : the registered value has the receiver's type.
            "reg" => recv,
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
        for f in fields {
            let vt = self.infer_expr(body, f.value);
            if let Some(sig) = field_sig
                && let Some(decl) = sig.fields.iter().find(|d| d.name == f.name)
            {
                let dt = self.substitute(&decl.ty, &args);
                self.unify(&dt, &vt);
            }
        }
        match owner {
            Some(def) => Type::Value {
                kind: ValueKind::Struct {
                    def,
                    args: GenericArgs(args),
                },
                domain: self.fresh_domain(),
            },
            None => Type::Error,
        }
    }

    // ----- helpers -----

    fn owner_of(&self, ty: &Type<'db>) -> Option<DefId<'db>> {
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
            .map(|d| d.module == self.map.prelude() && (d.name == "+" || d.name == "*"))
            .unwrap_or(false)
    }

    /// Build a substitution mapping each of `params`'s indices to a fresh
    /// inference variable of the matching kind (Type/Domain), so a generic
    /// callee is instantiated at the call site. Const generics map to `Deferred`
    /// (widths are a Q4 concern).
    fn fresh_subst(&mut self, params: &[crate::hir::types::GenericParam]) -> Vec<GenericArg<'db>> {
        params
            .iter()
            .map(|p| match p.kind {
                GenericParamKind::Type => GenericArg::Type(self.fresh_type()),
                GenericParamKind::Domain => GenericArg::Domain(self.fresh_domain()),
                GenericParamKind::Const => GenericArg::Const(self.fresh_const()),
            })
            .collect()
    }

    fn substitute(&mut self, ty: &Type<'db>, subst: &[GenericArg<'db>]) -> Type<'db> {
        match ty {
            Type::Value {
                kind: ValueKind::Param(i),
                domain,
            } => match subst.get(*i as usize) {
                // The type arg replaces the param; the annotation domain travels
                // with the arg (a simplification — domain stamping is Q4/Q5).
                Some(GenericArg::Type(t)) => t.clone(),
                _ => Type::Value {
                    kind: ValueKind::Param(*i),
                    domain: self.subst_domain(*domain, subst),
                },
            },
            Type::Value { kind, domain } => Type::Value {
                kind: self.subst_kind(kind, subst),
                domain: self.subst_domain(*domain, subst),
            },
            Type::Port { def, args, domain } => Type::Port {
                def: *def,
                args: self.subst_args(args, subst),
                domain: self.subst_domain(*domain, subst),
            },
            other => other.clone(),
        }
    }

    fn subst_kind(&mut self, kind: &ValueKind<'db>, subst: &[GenericArg<'db>]) -> ValueKind<'db> {
        match kind {
            ValueKind::UInt { width } => ValueKind::UInt {
                width: self.subst_const(width, subst),
            },
            // A parametric struct's args are themselves in terms of the
            // enclosing def's generics — instantiate them too.
            ValueKind::Struct { def, args } => ValueKind::Struct {
                def: *def,
                args: self.subst_args(args, subst),
            },
            other => other.clone(),
        }
    }

    fn subst_args(
        &mut self,
        args: &GenericArgs<'db>,
        subst: &[GenericArg<'db>],
    ) -> GenericArgs<'db> {
        GenericArgs(
            args.0
                .iter()
                .map(|a| match a {
                    GenericArg::Type(t) => GenericArg::Type(self.substitute(t, subst)),
                    GenericArg::Const(c) => GenericArg::Const(self.subst_const(c, subst)),
                    GenericArg::Domain(d) => GenericArg::Domain(self.subst_domain(*d, subst)),
                })
                .collect(),
        )
    }

    fn subst_const(&self, width: &ConstArg, subst: &[GenericArg<'db>]) -> ConstArg {
        match width {
            ConstArg::Param(i) => match subst.get(*i as usize) {
                Some(GenericArg::Const(c)) => c.clone(),
                _ => ConstArg::Deferred,
            },
            other => other.clone(),
        }
    }

    fn subst_domain(&mut self, d: Domain, subst: &[GenericArg<'db>]) -> Domain {
        match d {
            Domain::Param(i) => match subst.get(i as usize) {
                Some(GenericArg::Domain(dom)) => *dom,
                _ => Domain::Unspecified,
            },
            Domain::Unspecified => self.fresh_domain(),
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
                ValueKind::Usize => "usize",
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
            "fn ok (a: uint(8), b: uint(8)) -> uint(8) { return a + b; }\nfn sym { param n: usize, param m: usize } (a: uint(n), b: uint(m)) -> uint(n) { return a + b; }",
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
            "struct Bus(A: Type) = bus { valid: bool, data: A }\nfn f () -> Bus(uint(8)) { return bus { valid: true, data: 0 }; }",
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
