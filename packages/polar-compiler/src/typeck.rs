//! Type and domain inference, first pass.
//!
//! Walks HIR, infers a type for every expression, and unifies eagerly when
//! the walk reaches a join (binary op, equation, return, call-site argument).
//! Constraints that can't be solved on the walk (deferred width const-eval,
//! clock-kind enforcement) are queued as [`Obligation`]s.
//!
//! Design is documented in `planning/type_inference.md`. Architecture follows
//! rustc's `InferCtxt`: a per-function context owns substitution tables for
//! type and domain variables, an obligation queue, and a `locals` environment.
//! Generation is the same shape it'll have once parametric structs return —
//! every callee parameter is instantiated through a substitution map keyed on
//! its `LocalId`, the same mechanism that handles today's inferable `#clk`.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::hir::{
    ConstValue, Domain, HirArg, HirBlock, HirCall, HirExpr, HirExprKind, HirFieldAccess, HirFn,
    HirId, HirItem, HirMethodCall, HirParam, HirPort, HirSourceFile, HirStmt, HirStruct, HirType,
    HirTypeKind, LocalId, ParamKind, ParamSection, PortTypeRef, TypeVar, ValueKind, ValueType,
};
use crate::resolve::{DefId, ResolveResult};
use crate::{Identifier, SourceExcerpt, SourceSpan};

// ============================================================================
// Errors and obligations
// ============================================================================

#[derive(Debug, Clone)]
pub struct TypeError {
    pub kind: TypeErrorKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeErrorKind {
    /// Two types failed to unify.
    TypeMismatch { expected: String, got: String },
    /// Two domains failed to unify.
    DomainMismatch { expected: String, got: String },
    /// A call had the wrong number of positional arguments. Direction-check
    /// catches most of these, but type-check sees the post-slotting view.
    ArityMismatch {
        callee: String,
        expected: usize,
        got: usize,
    },
    /// An expression slot resolved to a `Clock` meta-kind where a value type
    /// was expected (or vice versa).
    KindMismatch {
        expected: &'static str,
        got: &'static str,
    },
    /// A field access referenced a name not declared on the receiver's type.
    UnknownField {
        receiver_type: String,
        field: String,
    },
    /// A field access was attempted on a receiver whose type doesn't have
    /// fields (a scalar value, a clock, etc.).
    FieldAccessOnNonAggregate { receiver_type: String },
}

impl fmt::Display for TypeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected `{expected}`, got `{got}`")
            }
            Self::DomainMismatch { expected, got } => {
                write!(f, "domain mismatch: expected `{expected}`, got `{got}`")
            }
            Self::ArityMismatch {
                callee,
                expected,
                got,
            } => write!(f, "`{callee}` expects {expected} argument(s), got {got}"),
            Self::KindMismatch { expected, got } => {
                write!(f, "kind mismatch: expected {expected}, got {got}")
            }
            Self::UnknownField {
                receiver_type,
                field,
            } => write!(f, "type `{receiver_type}` has no field `{field}`"),
            Self::FieldAccessOnNonAggregate { receiver_type } => write!(
                f,
                "cannot access a field on a value of type `{receiver_type}`"
            ),
        }
    }
}

/// A constraint the walker couldn't solve immediately. The flush-after-walk
/// step takes another swing using the now-richer substitution; whatever
/// survives lands in [`TypeCheckResult::residual_obligations`] for later
/// passes (const-eval, domain-bound solver) to discharge.
#[derive(Debug, Clone)]
pub struct Obligation {
    pub kind: ObligationKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub enum ObligationKind {
    /// Two width expressions must be equal under const-eval.
    WidthEq { lhs: HirExpr, rhs: HirExpr },
    /// A domain must inhabit the `Clock` kind (not `@const`). Discharged once
    /// the domain resolves and bound-tracking is in place.
    DomainKind { domain: Domain },
}

// ============================================================================
// Public API
// ============================================================================

#[derive(Debug, Default)]
pub struct TypeCheckResult {
    pub errors: Vec<TypeError>,
    /// Obligations that could not be discharged during this pass.
    pub residual_obligations: Vec<Obligation>,
    /// Types inferred for each expression keyed by `HirId`.
    pub expr_types: HashMap<HirId, HirType>,
    /// Resolved callee `DefId` for each `HirExprKind::MethodCall` expression,
    /// keyed by the MethodCall's `HirId`. Consumed by the `method_lower` pass
    /// to rewrite each `MethodCall` into a regular `Call`.
    pub method_resolutions: HashMap<HirId, DefId>,
    /// Inferred type for each local, keyed by `LocalId`. Includes params,
    /// `let`s, `var`s, and implicit vars introduced by source-arrows. Used
    /// by `flatten` to decide whether to split an aggregate-typed local
    /// that wasn't declared via a `var` statement.
    pub local_types: HashMap<LocalId, HirType>,
}

/// Run type and domain checking on a lowered HIR file.
pub fn check_file(file: &HirSourceFile, resolve: &ResolveResult) -> TypeCheckResult {
    let mut ctx = FileCtx::new(file, resolve);
    for item in &file.items {
        match item {
            HirItem::Fn(func) => {
                let mut infer = InferCtxt::new();
                infer.check_fn(func, &ctx);
                ctx.collect(&mut infer);
            }
            HirItem::Struct(s) => ctx.check_struct(s),
            HirItem::Port(_) => {
                // Port field types are already lowered. A future pass will
                // verify clock-parameter use inside fields; nothing to do yet.
            }
        }
    }
    TypeCheckResult {
        errors: ctx.errors,
        residual_obligations: ctx.residual_obligations,
        expr_types: ctx.expr_types,
        method_resolutions: ctx.method_resolutions,
        local_types: ctx.local_types,
    }
}

pub fn render_type_errors(
    errors: &[TypeError],
    source: &str,
    path: Option<&Path>,
    f: &mut impl fmt::Write,
) -> fmt::Result {
    for (i, error) in errors.iter().enumerate() {
        if i > 0 {
            writeln!(f)?;
        }
        writeln!(f, "error: {}", error.kind)?;
        if let Some(path) = path {
            writeln!(
                f,
                " --> {}:{}:{}",
                path.display(),
                error.span.start.row + 1,
                error.span.start.column + 1
            )?;
        } else {
            writeln!(
                f,
                " --> {}:{}",
                error.span.start.row + 1,
                error.span.start.column + 1
            )?;
        }
        if let Some(excerpt) = excerpt_for_span(source, &error.span) {
            writeln!(f, "  |")?;
            writeln!(f, "{:>2} | {}", excerpt.line_number, excerpt.line_text)?;
            writeln!(
                f,
                "  | {}{}",
                " ".repeat(excerpt.highlight_start),
                "^".repeat(
                    excerpt
                        .highlight_end
                        .saturating_sub(excerpt.highlight_start)
                )
            )?;
        }
    }
    Ok(())
}

fn excerpt_for_span(source: &str, span: &SourceSpan) -> Option<SourceExcerpt> {
    let line_text = source.lines().nth(span.start.row)?.to_owned();
    let start = span.start.column.min(line_text.len());
    let end = if span.start.row == span.end.row {
        span.end
            .column
            .max(start + 1)
            .min(line_text.len().max(start + 1))
    } else {
        line_text.len().max(start + 1)
    };
    Some(SourceExcerpt {
        line_number: span.start.row + 1,
        line_text,
        highlight_start: start,
        highlight_end: end,
    })
}

// ============================================================================
// Width obligation check (early-error pass over residual obligations)
// ============================================================================

/// Early-error check over `WidthEq` obligations left after type-checking.
///
/// Per the non-monomorphic SV backend plan, symbolic width equalities
/// (`N + N ~ 2 * N`) are *not* a Polar-compile-time concern — they thread
/// through to the SV elaborator as parameter-level arithmetic. This pass
/// therefore only catches the cases that *can* be resolved now:
///
/// - Both sides are concrete integers and disagree → error.
/// - Both sides are concrete integers and agree → discharged.
/// - Either side is non-literal → recorded in `unresolved_widths` for the
///   SV emitter to translate into parameter expressions (or, eventually,
///   for an algebraic checker to discharge symbolically).
///
/// `DomainKind` obligations are pass-through; the bound-tracking domain
/// solver will handle them later.
pub fn check_width_obligations(obligations: &[Obligation]) -> WidthCheckResult {
    let mut errors = Vec::new();
    let mut unresolved_widths = Vec::new();
    let mut unresolved_domain_kinds = Vec::new();
    for ob in obligations {
        match &ob.kind {
            ObligationKind::WidthEq { lhs, rhs } => {
                match (const_eval_width(lhs), const_eval_width(rhs)) {
                    (Some(a), Some(b)) if a != b => {
                        errors.push(TypeError {
                            kind: TypeErrorKind::TypeMismatch {
                                expected: format!("uint({a})"),
                                got: format!("uint({b})"),
                            },
                            span: ob.span.clone(),
                        });
                    }
                    (Some(_), Some(_)) => {}
                    _ => unresolved_widths.push(ob.clone()),
                }
            }
            ObligationKind::DomainKind { .. } => unresolved_domain_kinds.push(ob.clone()),
        }
    }
    WidthCheckResult {
        errors,
        unresolved_widths,
        unresolved_domain_kinds,
    }
}

#[derive(Debug, Default)]
pub struct WidthCheckResult {
    pub errors: Vec<TypeError>,
    /// Symbolic `WidthEq` obligations that could not be reduced to literals.
    /// TODO: thread these into the SV emitter so they can be emitted as
    /// parameter arithmetic (`logic [N+M-1:0]` etc.) once parametric widths
    /// land. Currently dropped on the floor by the CLI.
    pub unresolved_widths: Vec<Obligation>,
    /// `DomainKind` obligations awaiting the bound-tracking domain solver.
    pub unresolved_domain_kinds: Vec<Obligation>,
}

fn const_eval_width(e: &HirExpr) -> Option<u64> {
    match &e.kind {
        HirExprKind::Const(ConstValue::Integer(n)) => Some(*n),
        _ => None,
    }
}

// ============================================================================
// File-scoped context (struct/port/fn lookups shared across functions)
// ============================================================================

struct FileCtx<'hir> {
    #[allow(dead_code)]
    resolve: &'hir ResolveResult,
    /// User-defined functions keyed by their `DefId`. Used to look up
    /// signatures at call sites.
    fns: HashMap<DefId, &'hir HirFn>,
    /// Structs keyed by `DefId`. Used to validate record constructors.
    structs: HashMap<DefId, &'hir HirStruct>,
    /// Ports keyed by `DefId`. Used to resolve field access on port-typed
    /// receivers (substituting the port's `#clk` parameter for the
    /// instance's actual clock).
    ports: HashMap<DefId, &'hir HirPort>,
    /// `DefId` of the prelude `reg` primitive. Calls to it use a hand-built
    /// signature instead of looking up a `HirFn`.
    reg_def_id: Option<DefId>,
    /// `DefId`s of the prelude arithmetic operators. HIR lowering desugars
    /// `a + b` into a `HirCall` against one of these. Their polymorphic
    /// signature (`{N, D}(uint(N) @D, uint(N) @D) -> uint(N) @D`) is handled
    /// by `infer_arith_call` until value-level type parameters land.
    add_def_id: Option<DefId>,
    mul_def_id: Option<DefId>,
    errors: Vec<TypeError>,
    residual_obligations: Vec<Obligation>,
    expr_types: HashMap<HirId, HirType>,
    method_resolutions: HashMap<HirId, DefId>,
    local_types: HashMap<LocalId, HirType>,
}

impl<'hir> FileCtx<'hir> {
    fn new(file: &'hir HirSourceFile, resolve: &'hir ResolveResult) -> Self {
        let mut fns = HashMap::new();
        let mut structs = HashMap::new();
        let mut ports = HashMap::new();
        for item in &file.items {
            match item {
                HirItem::Fn(f) => {
                    fns.insert(f.def_id, f);
                }
                HirItem::Struct(s) => {
                    structs.insert(s.def_id, s);
                }
                HirItem::Port(p) => {
                    ports.insert(p.def_id, p);
                }
            }
        }
        Self {
            resolve,
            fns,
            structs,
            ports,
            reg_def_id: resolve.def_id("reg"),
            add_def_id: resolve.def_id("+"),
            mul_def_id: resolve.def_id("*"),
            errors: Vec::new(),
            residual_obligations: Vec::new(),
            expr_types: HashMap::new(),
            method_resolutions: HashMap::new(),
            local_types: HashMap::new(),
        }
    }

    fn collect(&mut self, infer: &mut InferCtxt) {
        self.errors.append(&mut infer.errors);
        self.residual_obligations.append(&mut infer.obligations);
        for (id, ty) in infer.expr_types.drain() {
            self.expr_types.insert(id, ty);
        }
        for (id, def) in infer.method_resolutions.drain() {
            self.method_resolutions.insert(id, def);
        }
        for (id, ty) in infer.locals.drain() {
            self.local_types.insert(id, ty);
        }
    }

    fn check_struct(&mut self, _s: &HirStruct) {
        // Field types are already lowered. Future passes will verify width
        // const-ness and reject self-referential structs. Nothing to check
        // here yet.
    }

    fn lookup_fn(&self, def_id: DefId) -> Option<&'hir HirFn> {
        self.fns.get(&def_id).copied()
    }

    fn lookup_struct(&self, def_id: DefId) -> Option<&'hir HirStruct> {
        self.structs.get(&def_id).copied()
    }

    fn lookup_port(&self, def_id: DefId) -> Option<&'hir HirPort> {
        self.ports.get(&def_id).copied()
    }

    fn is_reg(&self, def_id: DefId) -> bool {
        self.reg_def_id == Some(def_id)
    }

    fn is_arith_op(&self, def_id: DefId) -> bool {
        Some(def_id) == self.add_def_id || Some(def_id) == self.mul_def_id
    }
}

// ============================================================================
// Per-function inference context
// ============================================================================

struct InferCtxt {
    /// Resolution table for type variables. Index is `TypeVar.0`. `None` means
    /// unbound; `Some(t)` means bound to `t` (which may itself contain
    /// variables — chains are walked by `resolve_type`).
    type_vars: Vec<Option<HirType>>,
    /// Resolution table for domain variables.
    domain_vars: Vec<Option<Domain>>,
    /// Types of local bindings (params, lets, vars). Populated as the walker
    /// encounters declarations.
    locals: HashMap<LocalId, HirType>,
    /// Per-expression inferred types, keyed by `HirId`.
    expr_types: HashMap<HirId, HirType>,
    /// Resolved callee for each `HirExprKind::MethodCall`, keyed by the
    /// MethodCall's `HirId`. Filled in by `infer_method_call`.
    method_resolutions: HashMap<HirId, DefId>,
    /// Constraints not solved at the walk site.
    obligations: Vec<Obligation>,
    errors: Vec<TypeError>,
}

impl InferCtxt {
    fn new() -> Self {
        Self {
            type_vars: Vec::new(),
            domain_vars: Vec::new(),
            locals: HashMap::new(),
            expr_types: HashMap::new(),
            method_resolutions: HashMap::new(),
            obligations: Vec::new(),
            errors: Vec::new(),
        }
    }

    // ---- variable allocation ----

    fn fresh_type_var(&mut self, span: SourceSpan) -> HirType {
        let id = TypeVar(self.type_vars.len() as u32);
        self.type_vars.push(None);
        HirType {
            kind: HirTypeKind::Var(id),
            span,
        }
    }

    fn fresh_domain_var(&mut self) -> Domain {
        let id = self.domain_vars.len() as u32;
        self.domain_vars.push(None);
        Domain::Var(id)
    }

    // ---- resolution (follow substitution chains) ----

    fn resolve_type(&self, ty: &HirType) -> HirType {
        match &ty.kind {
            HirTypeKind::Var(v) => {
                match self.type_vars.get(v.0 as usize).and_then(|r| r.as_ref()) {
                    Some(bound) => {
                        let r = self.resolve_type(bound);
                        HirType {
                            kind: r.kind,
                            span: ty.span.clone(),
                        }
                    }
                    None => ty.clone(),
                }
            }
            HirTypeKind::Value(vt) => HirType {
                kind: HirTypeKind::Value(ValueType {
                    kind: vt.kind.clone(),
                    domain: self.resolve_domain(&vt.domain),
                }),
                span: ty.span.clone(),
            },
            _ => ty.clone(),
        }
    }

    /// Follow substitution chain for a domain. `Var(n)` that is unbound stays
    /// as `Var(n)` — callers that need a concrete result should call
    /// `finalize_domain` instead (which defaults unbound vars to `Const`).
    fn resolve_domain(&self, d: &Domain) -> Domain {
        match d {
            Domain::Var(i) => match self.domain_vars.get(*i as usize).and_then(|r| r.as_ref()) {
                Some(bound) => self.resolve_domain(bound),
                None => d.clone(),
            },
            other => other.clone(),
        }
    }

    /// Like `resolve_domain` but defaults unbound `Var` and `Unspecified` to
    /// `Const`. Called during finalisation after the walk is complete.
    fn finalize_domain(&self, d: &Domain) -> Domain {
        match d {
            Domain::Var(i) => match self.domain_vars.get(*i as usize).and_then(|r| r.as_ref()) {
                Some(bound) => self.finalize_domain(bound),
                None => Domain::Const,
            },
            Domain::Unspecified => Domain::Const,
            other => other.clone(),
        }
    }

    /// Finalise a type by following all substitution chains and defaulting any
    /// remaining `Var` or `Unspecified` domains to `Const`. Call this after the
    /// full walk to produce clean types for downstream passes.
    fn finalize_type(&self, ty: &HirType) -> HirType {
        match &ty.kind {
            HirTypeKind::Var(v) => {
                match self.type_vars.get(v.0 as usize).and_then(|r| r.as_ref()) {
                    Some(bound) => self.finalize_type(bound),
                    None => ty.clone(),
                }
            }
            HirTypeKind::Value(vt) => HirType {
                kind: HirTypeKind::Value(ValueType {
                    kind: vt.kind.clone(),
                    domain: self.finalize_domain(&vt.domain),
                }),
                span: ty.span.clone(),
            },
            _ => ty.clone(),
        }
    }

    /// Resolve all stored expression and local types, defaulting any
    /// unbound domain variables to `Const`. Called at the end of `check_fn`.
    fn finalize_types(&mut self) {
        let ids: Vec<HirId> = self.expr_types.keys().copied().collect();
        for id in ids {
            let ty = self.expr_types[&id].clone();
            let resolved = self.finalize_type(&ty);
            self.expr_types.insert(id, resolved);
        }
        let locals: Vec<LocalId> = self.locals.keys().copied().collect();
        for local in locals {
            let ty = self.locals[&local].clone();
            let resolved = self.finalize_type(&ty);
            self.locals.insert(local, resolved);
        }
    }

    // ---- unification ----

    fn unify_types(&mut self, expected: &HirType, got: &HirType, span: SourceSpan) {
        let a = self.resolve_type(expected);
        let b = self.resolve_type(got);
        match (&a.kind, &b.kind) {
            (HirTypeKind::Var(va), HirTypeKind::Var(vb)) if va == vb => {}
            (HirTypeKind::Var(v), _) => {
                self.type_vars[v.0 as usize] = Some(b.clone());
            }
            (_, HirTypeKind::Var(v)) => {
                self.type_vars[v.0 as usize] = Some(a.clone());
            }
            (HirTypeKind::Value(va), HirTypeKind::Value(vb)) => {
                let kind_ok = self.unify_value_kinds(&va.kind, &vb.kind, &span);
                self.unify_domains(&va.domain, &vb.domain, span.clone());
                if !kind_ok {
                    self.errors.push(TypeError {
                        kind: TypeErrorKind::TypeMismatch {
                            expected: describe_type(&a),
                            got: describe_type(&b),
                        },
                        span,
                    });
                }
            }
            (HirTypeKind::Port(pa), HirTypeKind::Port(pb)) if pa.def == pb.def => {
                // Port unification is currently def-equality only. Once
                // positional type arguments land (`Stream8(clk)`), this arm
                // unifies the per-argument domains.
            }
            (HirTypeKind::Clock, HirTypeKind::Clock) => {}
            _ => {
                self.errors.push(TypeError {
                    kind: TypeErrorKind::TypeMismatch {
                        expected: describe_type(&a),
                        got: describe_type(&b),
                    },
                    span,
                });
            }
        }
    }

    /// Returns `true` if the value kinds unify structurally.
    fn unify_value_kinds(&mut self, a: &ValueKind, b: &ValueKind, span: &SourceSpan) -> bool {
        match (a, b) {
            (ValueKind::Bool, ValueKind::Bool) => true,
            (ValueKind::Reset, ValueKind::Reset) => true,
            (ValueKind::Usize, ValueKind::Usize) => true,
            (ValueKind::UInt { width: wa }, ValueKind::UInt { width: wb }) => {
                self.unify_widths(wa, wb, span);
                true
            }
            (ValueKind::Struct { def: da }, ValueKind::Struct { def: db }) => da == db,
            _ => false,
        }
    }

    fn unify_widths(&mut self, lhs: &HirExpr, rhs: &HirExpr, span: &SourceSpan) {
        // Width-inference placeholder produced by `const_type` for integer
        // literals: `id == HirId(u32::MAX)`. It matches any width — the
        // literal adopts whatever width its use site demands.
        if is_width_placeholder(lhs) || is_width_placeholder(rhs) {
            return;
        }
        if let (
            HirExprKind::Const(ConstValue::Integer(a)),
            HirExprKind::Const(ConstValue::Integer(b)),
        ) = (&lhs.kind, &rhs.kind)
        {
            if a != b {
                self.errors.push(TypeError {
                    kind: TypeErrorKind::TypeMismatch {
                        expected: format!("uint({a})"),
                        got: format!("uint({b})"),
                    },
                    span: span.clone(),
                });
            }
            return;
        }
        // Otherwise punt to const-eval.
        self.obligations.push(Obligation {
            kind: ObligationKind::WidthEq {
                lhs: lhs.clone(),
                rhs: rhs.clone(),
            },
            span: span.clone(),
        });
    }

    fn unify_domains(&mut self, expected: &Domain, got: &Domain, span: SourceSpan) {
        let a = self.resolve_domain(expected);
        let b = self.resolve_domain(got);
        match (&a, &b) {
            // Two vars pointing to the same slot — already unified.
            (Domain::Var(i), Domain::Var(j)) if i == j => {}
            // Bind a domain variable.
            (Domain::Var(i), _) => {
                self.domain_vars[*i as usize] = Some(b.clone());
            }
            (_, Domain::Var(j)) => {
                self.domain_vars[*j as usize] = Some(a.clone());
            }
            // `Unspecified` is a lowering artifact meaning "no annotation";
            // it defaults to @const at finalisation, so accept it here.
            (Domain::Unspecified, _) | (_, Domain::Unspecified) => {}
            (Domain::Const, _) | (_, Domain::Const) => {
                // `@const` is a supertype of every concrete clock domain.
                // Accept; bound-tracking will land with MLsub work.
            }
            (Domain::Clock(x), Domain::Clock(y)) if x == y => {}
            _ => {
                self.errors.push(TypeError {
                    kind: TypeErrorKind::DomainMismatch {
                        expected: describe_domain(&a),
                        got: describe_domain(&b),
                    },
                    span,
                });
            }
        }
    }

    // ---- driver: checking a function ----

    fn check_fn(&mut self, hir_fn: &HirFn, file: &FileCtx<'_>) {
        for param in &hir_fn.params {
            self.locals.insert(param.local, param.ty.clone());
        }
        self.check_block(&hir_fn.body, hir_fn.return_type.as_ref(), file);
        self.finalize_types();
    }

    fn check_block(
        &mut self,
        block: &HirBlock,
        expected_return: Option<&HirType>,
        file: &FileCtx<'_>,
    ) {
        for stmt in &block.statements {
            self.check_stmt(stmt, expected_return, file);
        }
    }

    fn check_stmt(
        &mut self,
        stmt: &HirStmt,
        expected_return: Option<&HirType>,
        file: &FileCtx<'_>,
    ) {
        match stmt {
            HirStmt::Let(l) => {
                let ty = self.infer_expr(&l.value, file);
                self.locals.insert(l.local, ty);
            }
            HirStmt::VarDecl(v) => {
                let ty =
                    v.ty.clone()
                        .unwrap_or_else(|| self.fresh_type_var(v.span.clone()));
                self.locals.insert(v.local, ty);
            }
            HirStmt::Equation(eq) => {
                let lhs_ty = self
                    .locals
                    .get(&eq.lhs)
                    .cloned()
                    .unwrap_or_else(|| self.fresh_type_var(eq.span.clone()));
                let rhs_ty = self.infer_expr(&eq.rhs, file);
                self.unify_types(&lhs_ty, &rhs_ty, eq.span.clone());
            }
            HirStmt::Return(e) => {
                let ty = self.infer_expr(e, file);
                if let Some(expected) = expected_return {
                    self.unify_types(expected, &ty, e.span.clone());
                }
            }
            HirStmt::Expr(e) => {
                let _ = self.infer_expr(e, file);
            }
        }
    }

    // ---- driver: inferring an expression ----

    fn infer_expr(&mut self, expr: &HirExpr, file: &FileCtx<'_>) -> HirType {
        let ty = match &expr.kind {
            HirExprKind::Const(c) => self.const_type(c, expr.span.clone()),
            HirExprKind::Local(id) => self
                .locals
                .get(id)
                .cloned()
                .unwrap_or_else(|| self.fresh_type_var(expr.span.clone())),
            HirExprKind::Call(call) => self.infer_call(call, file),
            HirExprKind::Field(field) => self.infer_field(field, expr.span.clone(), file),
            HirExprKind::MethodCall(mc) => {
                self.infer_method_call(mc, expr.id, expr.span.clone(), file)
            }
        };
        self.expr_types.insert(expr.id, ty.clone());
        ty
    }

    fn const_type(&mut self, c: &ConstValue, span: SourceSpan) -> HirType {
        match c {
            ConstValue::Integer(_) => {
                // Literal width is unknown at the use site — leave a fresh
                // width placeholder. The first-pass policy is to defer width
                // const-eval, so this placeholder typically gets unified
                // against a known width during the walk.
                let width = HirExpr {
                    kind: HirExprKind::Const(ConstValue::Integer(0)),
                    ty: None,
                    span: span.clone(),
                    id: HirId(u32::MAX),
                };
                let _ = width; // placeholder kept for shape; real width inference lands later
                HirType {
                    kind: HirTypeKind::Value(ValueType {
                        kind: ValueKind::UInt {
                            width: Box::new(HirExpr {
                                kind: HirExprKind::Const(ConstValue::Integer(0)),
                                ty: None,
                                span: span.clone(),
                                id: HirId(u32::MAX),
                            }),
                        },
                        domain: Domain::Const,
                    }),
                    span,
                }
            }
            ConstValue::Bool(_) => HirType {
                kind: HirTypeKind::Value(ValueType {
                    kind: ValueKind::Bool,
                    domain: Domain::Const,
                }),
                span,
            },
        }
    }

    fn infer_call(&mut self, call: &HirCall, file: &FileCtx<'_>) -> HirType {
        // Prelude operators and primitives are dispatched here with bespoke
        // logic until value-level type/width parameters land. Each of them
        // has a polymorphic signature the general substitution machinery
        // doesn't yet handle (`(+){N, D}(uint(N) @D, uint(N) @D)`,
        // `reg{N, D}(...)`, etc.); the special cases unify args directly.
        if file.is_arith_op(call.callee) {
            return self.infer_arith_call(call, file);
        }
        if file.is_reg(call.callee) {
            return self.infer_reg_call(call, file);
        }
        if let Some(struct_def) = file.lookup_struct(call.callee) {
            return self.infer_struct_call(struct_def, call, file);
        }
        let Some(callee) = file.lookup_fn(call.callee) else {
            // Unknown callee — direction-check should have caught this; if
            // not, downgrade to a fresh var so the walk continues.
            return self.fresh_type_var(call.span.clone());
        };

        // Build a substitution map for inferable named parameters. A
        // named `dom` parameter without a default contributes a fresh
        // `DomainVar` that is unified with the corresponding caller-side
        // domain when arguments are checked.
        let mut subst = SigSubst::default();
        for param in &callee.params {
            let inferable = matches!(param.section, ParamSection::Named)
                && matches!(param.kind, ParamKind::Dom | ParamKind::Param)
                && param.default.is_none();
            if inferable && is_clock_type(&param.ty) {
                subst
                    .domain_subst
                    .insert(param.local, self.fresh_domain_var());
            }
        }

        // Slot each arg's expression against the corresponding parameter.
        if call.args.len() != callee.params.len() {
            self.errors.push(TypeError {
                kind: TypeErrorKind::ArityMismatch {
                    callee: callee.name.clone(),
                    expected: callee.params.len(),
                    got: call.args.len(),
                },
                span: call.span.clone(),
            });
        }
        for (param, arg) in callee.params.iter().zip(call.args.iter()) {
            let param_ty = subst.apply_to_type(&param.ty);
            let arg_expr = match arg {
                HirArg::Provided { expr, .. } => Some(expr),
                HirArg::Inferable => None,
            };
            if let Some(e) = arg_expr {
                let arg_ty = self.infer_expr(e, file);
                self.unify_types(&param_ty, &arg_ty, e.span.clone());
            }
            // Inferable args carry no expression; their domain/type is what
            // the substitution and unification produce. Nothing to check here.
        }

        match &callee.return_type {
            Some(rt) => subst.apply_to_type(rt),
            None => HirType {
                kind: HirTypeKind::Clock, // void-ish placeholder
                span: call.span.clone(),
            },
        }
    }

    /// Arithmetic operators have signature
    /// `{N, D}(uint(N) @D, uint(N) @D) -> uint(N) @D`. The two operands
    /// must agree on width and domain; the result is the unified type.
    /// Today the implicit `N` and `D` parameters are unified by directly
    /// equating the operand types; once value-level type/width parameters
    /// land they fold into the standard substitution path.
    fn infer_arith_call(&mut self, call: &HirCall, file: &FileCtx<'_>) -> HirType {
        if call.args.len() != 2 {
            self.errors.push(TypeError {
                kind: TypeErrorKind::ArityMismatch {
                    callee: "<arith>".to_owned(),
                    expected: 2,
                    got: call.args.len(),
                },
                span: call.span.clone(),
            });
            return self.fresh_type_var(call.span.clone());
        }
        let lhs_ty = match &call.args[0] {
            HirArg::Provided { expr, .. } => self.infer_expr(expr, file),
            HirArg::Inferable => self.fresh_type_var(call.span.clone()),
        };
        let rhs_ty = match &call.args[1] {
            HirArg::Provided { expr, .. } => self.infer_expr(expr, file),
            HirArg::Inferable => self.fresh_type_var(call.span.clone()),
        };
        self.unify_types(&lhs_ty, &rhs_ty, call.span.clone());
        self.resolve_type(&lhs_ty)
    }

    /// `reg` has an implicit width parameter `N` shared between `self` and
    /// `reset_val`. We synthesise that relationship by inferring `self`'s
    /// width and unifying `reset_val`'s width against it. Domain handling
    /// follows the general path: `#clk` becomes a fresh domain var that
    /// flows through `rstn`, `self`, and the return type.
    fn infer_reg_call(&mut self, call: &HirCall, file: &FileCtx<'_>) -> HirType {
        if call.args.len() != 4 {
            self.errors.push(TypeError {
                kind: TypeErrorKind::ArityMismatch {
                    callee: "reg".to_owned(),
                    expected: 4,
                    got: call.args.len(),
                },
                span: call.span.clone(),
            });
            return self.fresh_type_var(call.span.clone());
        }

        // Slot 0: `#clk` (inferable) — fresh domain var.
        let clk_domain = self.fresh_domain_var();

        // Slot 1: `self @clk`.
        let self_ty = match &call.args[1] {
            HirArg::Provided { expr, .. } => self.infer_expr(expr, file),
            HirArg::Inferable => self.fresh_type_var(call.span.clone()),
        };

        // Slot 2: `rst: Reset @clk`.
        let rst_expected = HirType {
            kind: HirTypeKind::Value(ValueType {
                kind: ValueKind::Reset,
                domain: clk_domain.clone(),
            }),
            span: call.span.clone(),
        };
        if let HirArg::Provided { expr: e, .. } = &call.args[2] {
            let rt = self.infer_expr(e, file);
            self.unify_types(&rst_expected, &rt, e.span.clone());
        }

        // Slot 3: `reset_val: uint(N) @clk` — width and domain unify with `self`.
        if let HirArg::Provided { expr: e, .. } = &call.args[3] {
            let rv_ty = self.infer_expr(e, file);
            self.unify_types(&self_ty, &rv_ty, e.span.clone());
        }

        // The result is `uint(N) @clk` — same as `self`.
        // Tie `self`'s domain to the inferable `#clk` slot too.
        if let HirTypeKind::Value(vt) = &self_ty.kind {
            self.unify_domains(&clk_domain, &vt.domain, call.span.clone());
        }

        // Queue a clock-kind obligation: `clk_domain` must be a real clock,
        // not `@const`. Discharged by the bound-tracking domain solver.
        self.obligations.push(Obligation {
            kind: ObligationKind::DomainKind { domain: clk_domain },
            span: call.span.clone(),
        });

        self.resolve_type(&self_ty)
    }

    /// Type-check a struct-constructor call. The struct's declared fields act
    /// as the callee's positional parameters; HIR lowering already slotted
    /// the user's named fields into declared order and reported shape errors,
    /// so this method only needs to unify each arg's type against the
    /// corresponding field's declared type.
    fn infer_struct_call(
        &mut self,
        struct_def: &HirStruct,
        call: &HirCall,
        file: &FileCtx<'_>,
    ) -> HirType {
        if call.args.len() != struct_def.fields.len() {
            // HIR lowering should have produced exactly one arg per declared
            // field. A mismatch is a sign of an upstream bug; fall back
            // gracefully.
            self.errors.push(TypeError {
                kind: TypeErrorKind::ArityMismatch {
                    callee: struct_def.name.clone(),
                    expected: struct_def.fields.len(),
                    got: call.args.len(),
                },
                span: call.span.clone(),
            });
        }
        // Fresh domain var for the struct instance. Gets bound when the result
        // is unified with its use context (e.g. assigned to a @clk var).
        //
        // TODO: this propagation is order-dependent for mixed-domain literals
        // because the `@const` arm in `unify_domains` accepts without
        // rebinding. If a `@const` field is unified first, the struct's
        // domain pins to `@const` and later `@clk` fields slip past silently.
        // The proper fix is MLsub-style bound tracking — recording @const as
        // a lower bound but allowing concrete clocks to refine the variable.
        let domain = self.fresh_domain_var();
        for (decl, arg) in struct_def.fields.iter().zip(call.args.iter()) {
            if let HirArg::Provided { expr, .. } = arg {
                let value_ty = self.infer_expr(expr, file);
                self.unify_types(&decl.ty, &value_ty, expr.span.clone());
                // Propagate the field's domain up to the struct's domain so
                // that e.g. `packet { valid: true, data: some_clk_signal }`
                // infers @clk for the whole struct.
                if let HirTypeKind::Value(vt) = &value_ty.kind {
                    self.unify_domains(&domain, &vt.domain, expr.span.clone());
                }
            }
        }
        HirType {
            kind: HirTypeKind::Value(ValueType {
                kind: ValueKind::Struct {
                    def: struct_def.def_id,
                },
                domain,
            }),
            span: call.span.clone(),
        }
    }

    /// Infer the type of `<receiver>.<name>`. Resolves the receiver's type;
    /// for a struct, returns the declared field type with the receiver's
    /// domain stamped over `Unspecified` slots; for a port, substitutes the
    /// port's `#clk` parameter `LocalId` for the receiver's clock domain
    /// (the binding carried on `PortTypeRef.domain`).
    fn infer_field(
        &mut self,
        field: &HirFieldAccess,
        span: SourceSpan,
        file: &FileCtx<'_>,
    ) -> HirType {
        let recv_ty = self.infer_expr(&field.receiver, file);
        let resolved = self.resolve_type(&recv_ty);

        match &resolved.kind {
            HirTypeKind::Value(vt) => match &vt.kind {
                ValueKind::Struct { def } => {
                    let Some(struct_def) = file.lookup_struct(*def) else {
                        // Resolver should have caught an undefined struct
                        // before this; downgrade to a fresh var so the walk
                        // continues.
                        return self.fresh_type_var(span);
                    };
                    let Some(decl_field) = struct_def.fields.iter().find(|f| f.name == field.name)
                    else {
                        self.errors.push(TypeError {
                            kind: TypeErrorKind::UnknownField {
                                receiver_type: struct_def.name.clone(),
                                field: field.name.clone(),
                            },
                            span: field.name_span.clone(),
                        });
                        return self.fresh_type_var(span);
                    };
                    stamp_domain(&decl_field.ty, &vt.domain)
                }
                _ => {
                    self.errors.push(TypeError {
                        kind: TypeErrorKind::FieldAccessOnNonAggregate {
                            receiver_type: describe_type(&resolved),
                        },
                        span: span.clone(),
                    });
                    self.fresh_type_var(span)
                }
            },
            HirTypeKind::Port(port_ref) => {
                // Port field access depends on the port's clock binding being
                // available so the field type can have its `#clk` parameter
                // substituted out. That binding lived on `PortTypeRef.domain`
                // (single-clock-only); it has been removed pending the
                // `Stream8(clk)` positional type-argument syntax. Validate
                // the field name to give a useful error, but the typed
                // result is unrecoverable today.
                if let Some(port_def) = file.lookup_port(port_ref.def)
                    && !port_def.fields.iter().any(|f| f.name == field.name)
                {
                    self.errors.push(TypeError {
                        kind: TypeErrorKind::UnknownField {
                            receiver_type: port_def.name.clone(),
                            field: field.name.clone(),
                        },
                        span: field.name_span.clone(),
                    });
                    return self.fresh_type_var(span);
                }
                self.errors.push(TypeError {
                    kind: TypeErrorKind::FieldAccessOnNonAggregate {
                        receiver_type: describe_type(&resolved),
                    },
                    span: span.clone(),
                });
                self.fresh_type_var(span)
            }
            _ => {
                self.errors.push(TypeError {
                    kind: TypeErrorKind::FieldAccessOnNonAggregate {
                        receiver_type: describe_type(&resolved),
                    },
                    span: span.clone(),
                });
                self.fresh_type_var(span)
            }
        }
    }

    /// Resolve `recv.method(args)` against the receiver's type. Looks up
    /// `(receiver_type_def, method_name)` in `ResolveResult::impl_methods`,
    /// type-checks args against the method's signature (with the receiver
    /// as the implicit `self` arg), and records the resolution for the
    /// `method_lower` pass.
    fn infer_method_call(
        &mut self,
        mc: &HirMethodCall,
        expr_id: HirId,
        span: SourceSpan,
        file: &FileCtx<'_>,
    ) -> HirType {
        let recv_ty = self.infer_expr(&mc.receiver, file);
        let resolved_recv = self.resolve_type(&recv_ty);

        // Receiver must be a user-defined type (struct or port) — only those
        // can have `impl` blocks.
        let owner_def = match &resolved_recv.kind {
            HirTypeKind::Value(vt) => match &vt.kind {
                ValueKind::Struct { def } => Some(*def),
                _ => None,
            },
            HirTypeKind::Port(p) => Some(p.def),
            _ => None,
        };

        let Some(owner) = owner_def else {
            self.errors.push(TypeError {
                kind: TypeErrorKind::FieldAccessOnNonAggregate {
                    receiver_type: describe_type(&resolved_recv),
                },
                span: span.clone(),
            });
            // Still infer arg expressions for completeness.
            for arg in &mc.args {
                if let HirArg::Provided { expr, .. } = arg {
                    let _ = self.infer_expr(expr, file);
                }
            }
            return self.fresh_type_var(span);
        };

        // Look up the method on `owner` in the resolver's per-type table.
        let Some(&method_def) = file.resolve.impl_methods.get(&(owner, mc.name.clone())) else {
            self.errors.push(TypeError {
                kind: TypeErrorKind::UnknownField {
                    receiver_type: describe_type(&resolved_recv),
                    field: mc.name.clone(),
                },
                span: mc.name_span.clone(),
            });
            for arg in &mc.args {
                if let HirArg::Provided { expr, .. } = arg {
                    let _ = self.infer_expr(expr, file);
                }
            }
            return self.fresh_type_var(span);
        };

        let Some(callee) = file.lookup_fn(method_def) else {
            return self.fresh_type_var(span);
        };

        // Record the resolution for the post-typeck `method_lower` rewrite.
        self.method_resolutions.insert(expr_id, method_def);

        // Build a substitution for inferable named params (e.g. `dom clk`).
        let mut subst = SigSubst::default();
        for param in &callee.params {
            let inferable = matches!(param.section, ParamSection::Named)
                && matches!(param.kind, ParamKind::Dom | ParamKind::Param)
                && param.default.is_none();
            if inferable && is_clock_type(&param.ty) {
                subst
                    .domain_subst
                    .insert(param.local, self.fresh_domain_var());
            }
        }

        // Split positional params into `self` and the rest. The receiver is
        // checked against the first positional param; the remaining args are
        // checked against the rest.
        let positional: Vec<&HirParam> = callee
            .params
            .iter()
            .filter(|p| matches!(p.section, ParamSection::Positional))
            .collect();

        let Some(self_param) = positional.first() else {
            self.errors.push(TypeError {
                kind: TypeErrorKind::ArityMismatch {
                    callee: callee.name.clone(),
                    expected: 1,
                    got: 0,
                },
                span: span.clone(),
            });
            return self.fresh_type_var(span);
        };
        let self_expected = subst.apply_to_type(&self_param.ty);
        self.unify_types(&self_expected, &resolved_recv, mc.receiver.span.clone());

        let user_params = &positional[1..];
        if user_params.len() != mc.args.len() {
            self.errors.push(TypeError {
                kind: TypeErrorKind::ArityMismatch {
                    callee: callee.name.clone(),
                    expected: user_params.len(),
                    got: mc.args.len(),
                },
                span: span.clone(),
            });
        }
        for (param, arg) in user_params.iter().zip(mc.args.iter()) {
            let param_ty = subst.apply_to_type(&param.ty);
            if let HirArg::Provided { expr, .. } = arg {
                let arg_ty = self.infer_expr(expr, file);
                self.unify_types(&param_ty, &arg_ty, expr.span.clone());
            }
        }

        match &callee.return_type {
            Some(rt) => subst.apply_to_type(rt),
            None => HirType {
                kind: HirTypeKind::Clock,
                span,
            },
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Copy `recv_domain` over any `Domain::Unspecified` slot in `ty`. Used by
/// struct field access: the field's declared type carries no domain
/// annotation, so it adopts the receiver's domain at the use site.
fn stamp_domain(ty: &HirType, recv_domain: &Domain) -> HirType {
    let kind = match &ty.kind {
        HirTypeKind::Value(vt) => HirTypeKind::Value(ValueType {
            kind: vt.kind.clone(),
            domain: match &vt.domain {
                Domain::Unspecified => recv_domain.clone(),
                other => other.clone(),
            },
        }),
        other => other.clone(),
    };
    HirType {
        kind,
        span: ty.span.clone(),
    }
}

#[allow(dead_code)]
fn unused_identifier_marker(_: &Identifier) {}

/// Substitution map keyed on a callee parameter's `LocalId`. At call sites,
/// each inferable param's `LocalId` maps to a fresh inference variable; uses
/// of that `LocalId` in other param types or the return type are rewritten
/// through this map. This is the same shape parametric structs will use.
#[derive(Default)]
struct SigSubst {
    /// `Clock`-typed inferable params: their `LocalId` maps to a fresh domain.
    domain_subst: HashMap<LocalId, Domain>,
}

impl SigSubst {
    fn apply_to_type(&self, ty: &HirType) -> HirType {
        let kind = match &ty.kind {
            HirTypeKind::Value(vt) => HirTypeKind::Value(ValueType {
                kind: vt.kind.clone(),
                domain: self.apply_to_domain(&vt.domain),
            }),
            HirTypeKind::Port(p) => HirTypeKind::Port(PortTypeRef { def: p.def }),
            other => other.clone(),
        };
        HirType {
            kind,
            span: ty.span.clone(),
        }
    }

    fn apply_to_domain(&self, d: &Domain) -> Domain {
        match d {
            Domain::Clock(local) => self
                .domain_subst
                .get(local)
                .cloned()
                .unwrap_or_else(|| d.clone()),
            other => other.clone(),
        }
    }
}

fn is_clock_type(ty: &HirType) -> bool {
    matches!(ty.kind, HirTypeKind::Clock)
}

/// Sentinel: a width position that should be treated as "any width" for
/// unification purposes. Used for integer literals whose width is not yet
/// known. Recognised by `HirId(u32::MAX)`.
fn is_width_placeholder(e: &HirExpr) -> bool {
    e.id == HirId(u32::MAX)
}

fn describe_type(ty: &HirType) -> String {
    match &ty.kind {
        HirTypeKind::Var(v) => format!("?{}", v.0),
        HirTypeKind::Value(vt) => {
            let body = match &vt.kind {
                ValueKind::UInt { width } => match &width.kind {
                    HirExprKind::Const(ConstValue::Integer(n)) => format!("uint({n})"),
                    _ => "uint(N)".to_owned(),
                },
                ValueKind::Bool => "bool".to_owned(),
                ValueKind::Reset => "Reset".to_owned(),
                ValueKind::Usize => "usize".to_owned(),
                ValueKind::Struct { def } => format!("struct#{}", def.0),
            };
            let dom = describe_domain(&vt.domain);
            if dom.is_empty() {
                body
            } else {
                format!("{body} @{dom}")
            }
        }
        HirTypeKind::Port(p) => format!("port#{}", p.def.0),
        HirTypeKind::Clock => "Clock".to_owned(),
    }
}

fn describe_domain(d: &Domain) -> String {
    match d {
        Domain::Const => "const".to_owned(),
        Domain::Clock(l) => format!("clk#{}", l.0),
        Domain::Unspecified => String::new(),
        Domain::Var(n) => format!("?D{n}"),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::lower_to_hir;
    use crate::resolve::resolve_file;
    use crate::surface_ir::parse_surface_source;

    fn check(source: &str) -> TypeCheckResult {
        let file = parse_surface_source(source).expect("parse failed");
        let resolve = resolve_file(&file);
        assert!(
            resolve.errors.is_empty(),
            "resolve errors: {:?}",
            resolve.errors
        );
        let hir = lower_to_hir(&file, &resolve).expect("hir lowering");
        check_file(&hir, &resolve)
    }

    #[test]
    fn type_checks_simple_function() {
        let r = check(
            "fn f(rstn: Reset @clk, data: uint(8) @clk) -> uint(8) @clk { return data.reg(rstn, 0); }",
        );
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn type_checks_accumulator_pattern() {
        let r = check(
            "fn f(rstn: Reset @clk, data: uint(8) @clk) -> uint(8) @clk { var acc: uint(8) @clk = (acc + data).reg(rstn, 0); return acc; }",
        );
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn type_checks_record_constructor() {
        let r = check(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn idle() -> Packet { return packet { valid: false, payload: 0 }; }",
        );
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn type_checks_working_examples() {
        for (name, source) in crate::test_support::working_examples() {
            let r = check(&source);
            assert!(
                r.errors.is_empty(),
                "example `{name}` had type errors: {:?}",
                r.errors
            );
        }
    }

    #[test]
    fn type_checks_struct_field_access() {
        let r = check(
            "struct Pair = pair { a: bool, b: uint(8) }\n\
             fn f(p: Pair @clk) -> uint(8) @clk { return p.b; }",
        );
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn struct_field_access_carries_receiver_domain() {
        let r = check(
            "struct Pair = pair { a: bool, b: uint(8) }\n\
             fn f(rstn: Reset @clk, p: Pair @clk) -> uint(8) @clk { return p.b.reg(rstn, 0); }",
        );
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn unknown_struct_field_is_reported_by_typeck() {
        let r = check(
            "struct Pair = pair { a: bool, b: uint(8) }\n\
             fn f(p: Pair) -> bool { return p.c; }",
        );
        assert!(
            r.errors.iter().any(|e| matches!(
                &e.kind,
                TypeErrorKind::UnknownField { receiver_type, field }
                    if receiver_type == "Pair" && field == "c"
            )),
            "expected UnknownField, got: {:?}",
            r.errors
        );
    }

    #[test]
    fn field_access_on_scalar_is_rejected() {
        let r = check("fn f(x: uint(8)) -> bool { return x.payload; }");
        assert!(
            r.errors
                .iter()
                .any(|e| matches!(&e.kind, TypeErrorKind::FieldAccessOnNonAggregate { .. })),
            "expected FieldAccessOnNonAggregate, got: {:?}",
            r.errors
        );
    }

    // Port field-access tests removed: they exercised `Stream8 @clk`, which
    // is rejected at HIR lowering pending the `Stream8(clk)` positional
    // type-argument syntax. Restore once that lands.
}
