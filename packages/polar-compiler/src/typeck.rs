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
    BinOp, ConstValue, Domain, HirArg, HirBlock, HirCall, HirExpr, HirExprKind, HirFn, HirId,
    HirItem, HirRecord, HirSourceFile, HirStmt, HirStruct, HirType, HirTypeKind, LocalId,
    PortTypeRef, TypeVar, ValueKind, ValueType,
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
    /// A record constructor refers to an unknown struct.
    UnknownStruct,
    /// A record constructor names a field the struct does not declare.
    UnknownStructField { struct_name: String, field: String },
    /// A record constructor omits a required field.
    MissingStructField { struct_name: String, field: String },
    /// A record constructor mentions the same field twice.
    DuplicateStructField { field: String },
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
            Self::UnknownStruct => write!(f, "record constructor names an unknown struct"),
            Self::UnknownStructField { struct_name, field } => {
                write!(f, "struct `{struct_name}` has no field `{field}`")
            }
            Self::MissingStructField { struct_name, field } => {
                write!(
                    f,
                    "missing field `{field}` in record constructor for `{struct_name}`"
                )
            }
            Self::DuplicateStructField { field } => {
                write!(f, "duplicate field `{field}` in record constructor")
            }
            Self::ArityMismatch {
                callee,
                expected,
                got,
            } => write!(f, "`{callee}` expects {expected} argument(s), got {got}"),
            Self::KindMismatch { expected, got } => {
                write!(f, "kind mismatch: expected {expected}, got {got}")
            }
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
    /// `DefId` of the prelude `reg` primitive. Calls to it use a hand-built
    /// signature instead of looking up a `HirFn`.
    reg_def_id: Option<DefId>,
    errors: Vec<TypeError>,
    residual_obligations: Vec<Obligation>,
    expr_types: HashMap<HirId, HirType>,
}

impl<'hir> FileCtx<'hir> {
    fn new(file: &'hir HirSourceFile, resolve: &'hir ResolveResult) -> Self {
        let mut fns = HashMap::new();
        let mut structs = HashMap::new();
        for item in &file.items {
            match item {
                HirItem::Fn(f) => {
                    fns.insert(f.def_id, f);
                }
                HirItem::Struct(s) => {
                    structs.insert(s.def_id, s);
                }
                HirItem::Port(_) => {}
            }
        }
        Self {
            resolve,
            fns,
            structs,
            reg_def_id: resolve.def_id("reg"),
            errors: Vec::new(),
            residual_obligations: Vec::new(),
            expr_types: HashMap::new(),
        }
    }

    fn collect(&mut self, infer: &mut InferCtxt) {
        self.errors.append(&mut infer.errors);
        self.residual_obligations.append(&mut infer.obligations);
        for (id, ty) in infer.expr_types.drain() {
            self.expr_types.insert(id, ty);
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

    fn is_reg(&self, def_id: DefId) -> bool {
        self.reg_def_id == Some(def_id)
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
        // We represent domain variables by binding the next-free `Unspecified`
        // slot at allocation time. The chosen DomainVar marker is the index
        // stored implicitly in the table — see `resolve_domain` for the walk.
        let _ = id;
        Domain::Unspecified
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

    fn resolve_domain(&self, d: &Domain) -> Domain {
        // The first-pass design represents domain variables out-of-band: the
        // `Unspecified` variant + an index. We don't currently store the index
        // in `Domain` itself, which keeps the HIR shape unchanged while we
        // wire things up. For now, `Unspecified` just stays `Unspecified`
        // until something binds it — see `unify_domains`.
        d.clone()
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
            (HirTypeKind::Port(pa), HirTypeKind::Port(pb)) if pa.def == pb.def => {}
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
            (Domain::Unspecified, _) | (_, Domain::Unspecified) => {
                // First-pass policy: an unannotated domain absorbs whatever
                // it's unified against. Replacing Unspecified with a true
                // DomainVar lands when the domain solver does.
            }
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
            HirExprKind::Binary(op, l, r) => self.infer_binary(*op, l, r, expr.span.clone(), file),
            HirExprKind::Call(call) => self.infer_call(call, file),
            HirExprKind::Record(rec) => self.infer_record(rec, expr.span.clone(), file),
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

    fn infer_binary(
        &mut self,
        _op: BinOp,
        l: &HirExpr,
        r: &HirExpr,
        span: SourceSpan,
        file: &FileCtx<'_>,
    ) -> HirType {
        let lt = self.infer_expr(l, file);
        let rt = self.infer_expr(r, file);
        // `+` and `*` require two `uint` of the same width and the same
        // domain. Unifying directly handles both — width-equality drops to
        // the WidthEq obligation when symbolic.
        self.unify_types(&lt, &rt, span);
        self.resolve_type(&lt)
    }

    fn infer_call(&mut self, call: &HirCall, file: &FileCtx<'_>) -> HirType {
        // Build the callee's signature. For user-defined functions the
        // signature comes from the lowered `HirFn`. The prelude `reg` uses a
        // hand-built signature with implicit width inference between `self`
        // and `reset_val`.
        if file.is_reg(call.callee) {
            return self.infer_reg_call(call, file);
        }
        let Some(callee) = file.lookup_fn(call.callee) else {
            // Unknown callee — direction-check should have caught this; if
            // not, downgrade to a fresh var so the walk continues.
            return self.fresh_type_var(call.span.clone());
        };

        // Build a substitution map for inferable named parameters. Each
        // inferable `Clock`-kinded param becomes a fresh `DomainVar` for the
        // duration of this call.
        let mut subst = SigSubst::default();
        for param in &callee.params {
            if param.inferable {
                if is_clock_type(&param.ty) {
                    subst
                        .domain_subst
                        .insert(param.local, self.fresh_domain_var());
                }
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
                HirArg::Given(e) | HirArg::Default(e) => Some(e),
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
            HirArg::Given(e) | HirArg::Default(e) => self.infer_expr(e, file),
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
        if let HirArg::Given(e) | HirArg::Default(e) = &call.args[2] {
            let rt = self.infer_expr(e, file);
            self.unify_types(&rst_expected, &rt, e.span.clone());
        }

        // Slot 3: `reset_val: uint(N) @clk` — width and domain unify with `self`.
        if let HirArg::Given(e) | HirArg::Default(e) = &call.args[3] {
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

    fn infer_record(&mut self, rec: &HirRecord, span: SourceSpan, file: &FileCtx<'_>) -> HirType {
        let Some(struct_def) = file.lookup_struct(rec.struct_def) else {
            self.errors.push(TypeError {
                kind: TypeErrorKind::UnknownStruct,
                span: span.clone(),
            });
            return self.fresh_type_var(span);
        };

        // Field-name validation + per-field type unification.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for field in &rec.fields {
            if let Some(_prev) = seen.insert(field.name.as_str(), 0) {
                self.errors.push(TypeError {
                    kind: TypeErrorKind::DuplicateStructField {
                        field: field.name.clone(),
                    },
                    span: field.span.clone(),
                });
                continue;
            }
            let Some(decl) = struct_def.fields.iter().find(|f| f.name == field.name) else {
                self.errors.push(TypeError {
                    kind: TypeErrorKind::UnknownStructField {
                        struct_name: struct_def.name.clone(),
                        field: field.name.clone(),
                    },
                    span: field.span.clone(),
                });
                let _ = self.infer_expr(&field.value, file);
                continue;
            };
            let value_ty = self.infer_expr(&field.value, file);
            self.unify_types(&decl.ty, &value_ty, field.span.clone());
        }

        for decl in &struct_def.fields {
            if !seen.contains_key(decl.name.as_str()) {
                self.errors.push(TypeError {
                    kind: TypeErrorKind::MissingStructField {
                        struct_name: struct_def.name.clone(),
                        field: decl.name.clone(),
                    },
                    span: span.clone(),
                });
            }
        }

        // The struct's domain is whatever the use site needs. Leave it
        // Unspecified — surrounding unification will pin it.
        HirType {
            kind: HirTypeKind::Value(ValueType {
                kind: ValueKind::Struct {
                    def: rec.struct_def,
                },
                domain: Domain::Unspecified,
            }),
            span,
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

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
    fn type_checks_first_pass_examples() {
        let examples: &[(&str, &str)] = &[
            (
                "add_constant",
                include_str!("../../../examples/add_constant.plr"),
            ),
            (
                "accumulator",
                include_str!("../../../examples/accumulator.plr"),
            ),
            ("counter", include_str!("../../../examples/counter.plr")),
            ("mult_add", include_str!("../../../examples/mult_add.plr")),
            (
                "packet_struct",
                include_str!("../../../examples/packet_struct.plr"),
            ),
            ("pipeline", include_str!("../../../examples/pipeline.plr")),
            (
                "shift_register",
                include_str!("../../../examples/shift_register.plr"),
            ),
            (
                "simple_port",
                include_str!("../../../examples/simple_port.plr"),
            ),
        ];
        for (name, source) in examples {
            let r = check(source);
            assert!(
                r.errors.is_empty(),
                "example `{name}` had type errors: {:?}",
                r.errors
            );
        }
    }

    #[test]
    fn missing_struct_field_is_reported() {
        let r = check(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn f() -> Packet { return packet { valid: false }; }",
        );
        assert!(
            r.errors.iter().any(|e| matches!(
                &e.kind,
                TypeErrorKind::MissingStructField { struct_name, field }
                    if struct_name == "Packet" && field == "payload"
            )),
            "errors: {:?}",
            r.errors
        );
    }

    #[test]
    fn unknown_struct_field_is_reported() {
        let r = check(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn f() -> Packet { return packet { valid: false, payload: 0, extra: 1 }; }",
        );
        assert!(
            r.errors.iter().any(|e| matches!(
                &e.kind,
                TypeErrorKind::UnknownStructField { struct_name, field }
                    if struct_name == "Packet" && field == "extra"
            )),
            "errors: {:?}",
            r.errors
        );
    }
}
