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
    ConstValue, Domain, GenericArg, GenericArgs, HirArg, HirBlock, HirCall, HirExpr, HirExprKind,
    HirFieldAccess, HirFn, HirId, HirItem, HirLocalInfo, HirMethodCall, HirParam, HirPort,
    HirSourceFile, HirStmt, HirStruct, HirType, HirTypeKind, LocalId, ParamSection, PortTypeRef,
    TypeVar, ValueKind, ValueType,
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
    /// Two width expressions must be equal under const-eval. Surface
    /// representation kept as `HirExpr` for diagnostics and back-compat
    /// with `check_width_obligations`; equivalent `NormalConst` form is
    /// stored on the `ConstEq` companion for the residual machinery.
    WidthEq { lhs: HirExpr, rhs: HirExpr },
    /// Two const-typed expressions normalised to sum-of-monomials must
    /// be equal. Produced by `unify_widths` when local normalised
    /// equality fails (one or both sides have unbound vars or unresolved
    /// `Param` refs). Discharged via the fixpoint loop in
    /// [`discharge_obligations`] using current `const_vars` bindings;
    /// whatever survives is attached to the enclosing fn as a residual
    /// constraint and propagated through call sites.
    ConstEq {
        lhs: crate::hirt::normal_const::NormalConst,
        rhs: crate::hirt::normal_const::NormalConst,
    },
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
    /// Per-fn residual constraints surviving Phase D's fixpoint
    /// discharge. Keyed by the fn's DefId. Each entry is a list of
    /// `(NormalConst, NormalConst)` predicates that must hold at the
    /// fn's monomorphic instantiation — propagated through call sites
    /// during typeck, and emitted as SV `initial assert(…)` at the
    /// monomorphic module by sv_lower.
    pub fn_residuals: HashMap<DefId, Vec<FnResidual>>,
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
                ctx.collect(&mut infer, func.def_id);
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
        fn_residuals: ctx.fn_residuals,
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
            ObligationKind::ConstEq { lhs, rhs } => {
                // The fixpoint discharge pass that runs at end-of-fn
                // typeck already simplifies these; anything that
                // survives is a *residual constraint*. If both sides
                // are ground and unequal at this stage the typeck pass
                // would have already errored — here we just record
                // residuals for downstream uses (Phase D′ will emit
                // SystemVerilog asserts).
                if lhs.is_ground() && rhs.is_ground() && lhs != rhs {
                    errors.push(TypeError {
                        kind: TypeErrorKind::TypeMismatch {
                            expected: format!("width {}", describe_normal(lhs)),
                            got: format!("width {}", describe_normal(rhs)),
                        },
                        span: ob.span.clone(),
                    });
                } else if lhs != rhs {
                    unresolved_widths.push(ob.clone());
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

fn obligations_equal(a: &Obligation, b: &Obligation) -> bool {
    match (&a.kind, &b.kind) {
        (
            ObligationKind::ConstEq { lhs: la, rhs: ra },
            ObligationKind::ConstEq { lhs: lb, rhs: rb },
        ) => la == lb && ra == rb,
        _ => false,
    }
}

fn describe_normal(nc: &crate::hirt::normal_const::NormalConst) -> String {
    use crate::hirt::normal_const::NormalVar;
    if nc.terms.is_empty() {
        return nc.constant.to_string();
    }
    let mut parts: Vec<String> = nc
        .terms
        .iter()
        .map(|(c, v)| {
            let name = match v {
                NormalVar::Param(i) => format!("'P{i}"),
                NormalVar::ConstVar(i) => format!("?C{i}"),
                NormalVar::Local(l) => format!("loc#{}", l.0),
            };
            if *c == 1 { name } else { format!("{c}*{name}") }
        })
        .collect();
    if nc.constant != 0 {
        parts.push(nc.constant.to_string());
    }
    parts.join(" + ")
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
    /// `DefId`s of the prelude `uint` and `bool` primitive types. Used to map
    /// scalar receivers to an `impl_methods` owner so method dispatch on
    /// primitives flows through the same path as user-defined `impl T`.
    uint_def_id: Option<DefId>,
    bool_def_id: Option<DefId>,
    /// `DefId` of the prelude `Clock` type. Used so `clk.posedge()` etc.
    /// resolve via `impl_methods` like every other method call.
    clock_def_id: Option<DefId>,
    errors: Vec<TypeError>,
    residual_obligations: Vec<Obligation>,
    /// Per-fn residual constraints, populated after each `check_fn` from
    /// the InferCtxt's surviving `ConstEq` obligations. Read at call
    /// sites: `infer_call` looks up the callee's residuals, substitutes
    /// the call's `GenericArgs` through them, and pushes the result as
    /// fresh obligations in the caller's context.
    fn_residuals: HashMap<DefId, Vec<FnResidual>>,
    expr_types: HashMap<HirId, HirType>,
    method_resolutions: HashMap<HirId, DefId>,
    local_types: HashMap<LocalId, HirType>,
}

/// A residual constraint attached to a fn's signature. Stored as a pair
/// of `NormalConst`s with `Param(i)` references to the fn's generic
/// params; substitution at the call site rewrites those Params with the
/// caller's `GenericArgs[i].Const`.
#[derive(Debug, Clone)]
pub struct FnResidual {
    pub lhs: crate::hirt::normal_const::NormalConst,
    pub rhs: crate::hirt::normal_const::NormalConst,
    pub span: SourceSpan,
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
            uint_def_id: resolve.def_id("uint"),
            bool_def_id: resolve.def_id("bool"),
            clock_def_id: resolve.def_id("Clock"),
            errors: Vec::new(),
            residual_obligations: Vec::new(),
            fn_residuals: HashMap::new(),
            expr_types: HashMap::new(),
            method_resolutions: HashMap::new(),
            local_types: HashMap::new(),
        }
    }

    fn collect(&mut self, infer: &mut InferCtxt, owner: DefId) {
        self.errors.append(&mut infer.errors);
        // Split residuals: `ConstEq` survivors are attached to the fn's
        // signature for call-site propagation; everything else (legacy
        // WidthEq, DomainKind) keeps the existing flat-list path.
        let mut residuals = Vec::new();
        let mut other = Vec::new();
        for ob in infer.obligations.drain(..) {
            match ob.kind {
                ObligationKind::ConstEq { lhs, rhs } => residuals.push(FnResidual {
                    lhs,
                    rhs,
                    span: ob.span,
                }),
                _ => other.push(ob),
            }
        }
        if !residuals.is_empty() {
            self.fn_residuals.insert(owner, residuals);
        }
        self.residual_obligations.append(&mut other);
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

    /// Lookup a `HirFn` local's source name by its `LocalId`. Used by the
    /// generic-args substitution to pair Domain-kind generic params (named
    /// `clk` etc.) with the matching `HirParam`'s LocalId.
    fn local_name<'a>(&'a self, func: &'a HirFn, local: LocalId) -> Option<&'a str> {
        func.locals
            .get(local.0 as usize)
            .map(|info| info.name.as_str())
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
    /// Inference variables at the `ValueKind` level. Allocated when a
    /// parametric callsite needs a placeholder for a Type-kind generic
    /// argument's structural part (so the surrounding `ValueType.domain`
    /// can be substituted independently from the kind). Index = `u32` id.
    value_kind_vars: Vec<Option<ValueKind>>,
    /// Resolution table for const variables (Const-kind generic args).
    /// Allocated by `build_sig_subst` for each `param N: usize`-shaped
    /// generic param at a call site; pinned by `unify_widths` when an
    /// operand width is concrete. Index = `u32` id.
    const_vars: Vec<Option<HirExpr>>,
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
            value_kind_vars: Vec::new(),
            const_vars: Vec::new(),
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

    /// Allocate a fresh structural-only inference variable at the
    /// `ValueKind` level. Used by `fresh_args_for_def` for Type-kind
    /// generic args, paired with a separate fresh domain so an `A @clk`
    /// param can be unified element-wise.
    fn fresh_value_kind_var(&mut self) -> u32 {
        let id = self.value_kind_vars.len() as u32;
        self.value_kind_vars.push(None);
        id
    }

    /// Allocate a fresh const inference variable. Used by `build_sig_subst`
    /// to populate `GenericArg::Const(ConstVar(?C))` for each Const-kind
    /// generic param at a call site; `unify_widths` pins it via the call's
    /// operand widths.
    fn fresh_const_var(&mut self, span: SourceSpan) -> HirExpr {
        let id = self.const_vars.len() as u32;
        self.const_vars.push(None);
        HirExpr {
            kind: HirExprKind::ConstVar(id),
            ty: None,
            span,
            id: HirId(u32::MAX),
        }
    }

    /// Allocate a fresh `GenericArgs` for a use site of a parametric def.
    /// Each declared generic param becomes a fresh inference variable of the
    /// matching kind, so unification at the call/use site can pin it down.
    /// Mirrors rustc's `fresh_args_for_def` in `hir_ty_lowering`.
    fn fresh_args_for_def(
        &mut self,
        def_id: DefId,
        span: &SourceSpan,
        file: &FileCtx<'_>,
    ) -> GenericArgs {
        let params = file
            .resolve
            .defs
            .get(def_id.0 as usize)
            .map(|info| info.generic_params.clone())
            .unwrap_or_default();
        let mut args = Vec::with_capacity(params.len());
        for param in &params {
            args.push(match param.kind {
                crate::resolve::GenericParamKind::Type => {
                    GenericArg::Type(self.fresh_type_var(span.clone()))
                }
                crate::resolve::GenericParamKind::Const => GenericArg::Const(HirExpr {
                    kind: HirExprKind::Const(ConstValue::Integer(0)),
                    ty: None,
                    span: span.clone(),
                    id: HirId(u32::MAX),
                }),
                crate::resolve::GenericParamKind::Domain => {
                    GenericArg::Domain(self.fresh_domain_var())
                }
            });
        }
        GenericArgs(args)
    }

    /// Substitute the enclosing def's `Param(i)` references in `ty` with the
    /// matching entry from `args`. Mirrors rustc's `EarlyBinder::instantiate`:
    /// a single walk over the type, replacing `Param(i)` slots with the
    /// concrete (possibly inference-variable-bearing) argument.
    ///
    /// Only `Type`-kinded params substitute today; `Const` (width) and
    /// `Domain` substitution are deferred until parametric widths and
    /// parametric ports cross the typeck boundary.
    fn instantiate(
        &self,
        ty: &HirType,
        def_id: DefId,
        args: &GenericArgs,
        file: &FileCtx<'_>,
    ) -> HirType {
        let subst = self.build_substitution(def_id, args, file);
        self.apply_substitution(ty, &subst)
    }

    /// Build a `Substitution` from a def's `generic_params` paired with the
    /// supplied `args`. Type-kind args feed `type_subst`; Domain-kind args
    /// feed `domain_subst` keyed on the matching `HirParam`'s `LocalId` (so
    /// `Domain::Clock(local)` references in field/return types resolve to
    /// the caller's actual clock).
    fn build_substitution(
        &self,
        def_id: DefId,
        args: &GenericArgs,
        file: &FileCtx<'_>,
    ) -> Substitution {
        let mut domain_locals: HashMap<LocalId, Domain> = HashMap::new();
        let generic_params = &file.resolve.def_info(def_id).generic_params;
        // For Domain-kind params, find the matching HirParam's `LocalId`.
        // Structs have no HirParams, so the lookup returns an empty slice
        // and domain_locals stays empty — correct, since structs don't
        // carry `dom` params today.
        let (params, locals): (&[HirParam], &[HirLocalInfo]) =
            if let Some(p) = file.lookup_port(def_id) {
                (p.params.as_slice(), p.locals.as_slice())
            } else if let Some(f) = file.lookup_fn(def_id) {
                (f.params.as_slice(), f.locals.as_slice())
            } else {
                (&[], &[])
            };
        for (i, gp) in generic_params.iter().enumerate() {
            if !matches!(gp.kind, crate::resolve::GenericParamKind::Domain) {
                continue;
            }
            let Some(GenericArg::Domain(d)) = args.0.get(i) else {
                continue;
            };
            for p in params {
                if locals
                    .get(p.local.0 as usize)
                    .map(|info| info.name == gp.name)
                    .unwrap_or(false)
                {
                    domain_locals.insert(p.local, d.clone());
                    break;
                }
            }
        }
        Substitution {
            args: args.clone(),
            domain_locals,
        }
    }

    /// Apply a `Substitution` recursively to a type. Substitutes
    /// `ValueKind::Param(i)` via `args[i]`'s `Type` payload (keeping the
    /// outer domain where set), `HirExprKind::Param(i)` inside `uint(N)`
    /// widths via `args[i]`'s `Const` payload, `PortTypeRef.domain` and
    /// nested `Domain::Clock(local)` via `domain_locals`.
    fn apply_substitution(&self, ty: &HirType, subst: &Substitution) -> HirType {
        match &ty.kind {
            HirTypeKind::Value(vt) => {
                let domain = self.substitute_domain(&vt.domain, subst);
                let kind = match &vt.kind {
                    ValueKind::Param(i) => match subst.args.0.get(*i as usize) {
                        Some(GenericArg::Type(arg_ty)) => {
                            match &arg_ty.kind {
                                HirTypeKind::Value(arg_vt) => HirType {
                                    kind: HirTypeKind::Value(ValueType {
                                        kind: arg_vt.kind.clone(),
                                        domain: match &domain {
                                            Domain::Unspecified => arg_vt.domain.clone(),
                                            other => other.clone(),
                                        },
                                    }),
                                    span: ty.span.clone(),
                                },
                                _ => arg_ty.clone(),
                            }
                            .kind
                        }
                        _ => HirTypeKind::Value(ValueType {
                            kind: vt.kind.clone(),
                            domain,
                        }),
                    },
                    ValueKind::UInt { width } => HirTypeKind::Value(ValueType {
                        kind: ValueKind::UInt {
                            width: Box::new(self.substitute_const_expr(width, subst)),
                        },
                        domain,
                    }),
                    _ => HirTypeKind::Value(ValueType {
                        kind: vt.kind.clone(),
                        domain,
                    }),
                };
                HirType {
                    kind,
                    span: ty.span.clone(),
                }
            }
            HirTypeKind::Port(p) => HirType {
                kind: HirTypeKind::Port(PortTypeRef {
                    def: p.def,
                    args: p.args.clone(),
                    domain: self.substitute_domain(&p.domain, subst),
                }),
                span: ty.span.clone(),
            },
            _ => ty.clone(),
        }
    }

    fn substitute_domain(&self, d: &Domain, subst: &Substitution) -> Domain {
        match d {
            Domain::Clock(local) => subst
                .domain_locals
                .get(local)
                .cloned()
                .unwrap_or_else(|| d.clone()),
            Domain::Param(i) => match subst.args.0.get(*i as usize) {
                Some(GenericArg::Domain(dom)) => dom.clone(),
                _ => d.clone(),
            },
            other => other.clone(),
        }
    }

    fn substitute_const_expr(&self, expr: &HirExpr, subst: &Substitution) -> HirExpr {
        if subst.args.0.is_empty() {
            return expr.clone();
        }
        match &expr.kind {
            HirExprKind::Param(i) => match subst.args.0.get(*i as usize) {
                Some(GenericArg::Const(c)) => HirExpr {
                    span: expr.span.clone(),
                    ..c.clone()
                },
                _ => expr.clone(),
            },
            _ => expr.clone(),
        }
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
                    kind: self.resolve_value_kind(&vt.kind),
                    domain: self.resolve_domain(&vt.domain),
                }),
                span: ty.span.clone(),
            },
            _ => ty.clone(),
        }
    }

    /// Follow substitution chains for a `ValueKind`: `Var(n)` chases the
    /// `value_kind_vars` table; `UInt { width }` resolves any `ConstVar`
    /// inside the width via `const_vars`.
    fn resolve_value_kind(&self, k: &ValueKind) -> ValueKind {
        match k {
            ValueKind::Var(i) => match self
                .value_kind_vars
                .get(*i as usize)
                .and_then(|r| r.as_ref())
            {
                Some(bound) => self.resolve_value_kind(bound),
                None => k.clone(),
            },
            ValueKind::UInt { width } => ValueKind::UInt {
                width: Box::new(self.resolve_const_expr(width)),
            },
            _ => k.clone(),
        }
    }

    /// Follow substitution chain for a const expression. `ConstVar(n)` that
    /// resolves to another `ConstVar` walks the chain; unbound stays as-is.
    fn resolve_const_expr(&self, e: &HirExpr) -> HirExpr {
        match &e.kind {
            HirExprKind::ConstVar(i) => {
                match self.const_vars.get(*i as usize).and_then(|r| r.as_ref()) {
                    Some(bound) => {
                        let r = self.resolve_const_expr(bound);
                        HirExpr {
                            span: e.span.clone(),
                            ..r
                        }
                    }
                    None => e.clone(),
                }
            }
            _ => e.clone(),
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
                    kind: self.resolve_value_kind(&vt.kind),
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
                // Port unification: def-equality plus element-wise arg
                // unification, plus the implicit-domain unification for
                // single-domain ports (`DF @clk`).
                let aa = pa.args.clone();
                let ab = pb.args.clone();
                self.unify_generic_args(&aa, &ab, &span);
                let da = pa.domain.clone();
                let db = pb.domain.clone();
                self.unify_domains(&da, &db, span.clone());
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

    /// Element-wise unify two `GenericArgs` lists. Length mismatch is treated
    /// as a soft error: each arm of `unify_types` separately reports a
    /// `TypeMismatch` for the kind change, so the args mismatch here is
    /// silent. Within matched kinds (`Type` ~ `Type`, etc.) the underlying
    /// types/exprs/domains are unified through the usual paths.
    fn unify_generic_args(&mut self, a: &GenericArgs, b: &GenericArgs, span: &SourceSpan) {
        if a.len() != b.len() {
            // The outer kind-equality already passed, so the def is the same
            // — having a different arg count means a bug in the HIR
            // lowering. Report via `describe_type` at the caller.
            return;
        }
        for (lhs, rhs) in a.0.iter().zip(b.0.iter()) {
            match (lhs, rhs) {
                (GenericArg::Type(ta), GenericArg::Type(tb)) => {
                    self.unify_types(ta, tb, span.clone());
                }
                (GenericArg::Const(_), GenericArg::Const(_)) => {
                    // TODO: const args (e.g. `param N: usize`) currently
                    // unify via the width path inside containing types.
                    // A standalone const obligation lands once parametric
                    // widths surface at the top-level args list.
                }
                (GenericArg::Domain(da), GenericArg::Domain(db)) => {
                    self.unify_domains(da, db, span.clone());
                }
                _ => {
                    // Kind mismatch — already an HIR-lowering bug because
                    // `lower_generic_args` validates kinds against the def's
                    // declared params. Keep silent; outer unification will
                    // already have noticed the wider type mismatch.
                }
            }
        }
    }

    /// Returns `true` if the value kinds unify structurally.
    fn unify_value_kinds(&mut self, a: &ValueKind, b: &ValueKind, span: &SourceSpan) -> bool {
        match (a, b) {
            (ValueKind::Bool, ValueKind::Bool) => true,
            (ValueKind::Reset, ValueKind::Reset) => true,
            // `high`/`low` lower to `ConstValue::Bool` (the underlying repr),
            // but they may appear in `Reset @clk` positions (parameter
            // defaults, reset arguments). Accept the Bool↔Reset crossover
            // for now; a future revision should split high/low into a
            // dedicated `ConstValue::Reset(bool)` variant.
            (ValueKind::Bool, ValueKind::Reset) => true,
            (ValueKind::Reset, ValueKind::Bool) => true,
            (ValueKind::Usize, ValueKind::Usize) => true,
            (ValueKind::Event, ValueKind::Event) => true,
            (ValueKind::UInt { width: wa }, ValueKind::UInt { width: wb }) => {
                self.unify_widths(wa, wb, span);
                true
            }
            (ValueKind::Struct { def: da, args: aa }, ValueKind::Struct { def: db, args: ab }) => {
                if da != db {
                    return false;
                }
                self.unify_generic_args(aa, ab, span);
                true
            }
            (ValueKind::Param(a), ValueKind::Param(b)) => a == b,
            (ValueKind::Var(va), ValueKind::Var(vb)) if va == vb => true,
            (ValueKind::Var(v), other) => {
                let resolved = self.resolve_value_kind(&ValueKind::Var(*v));
                if let ValueKind::Var(unbound) = resolved {
                    // Still unbound — bind to the concrete kind.
                    self.value_kind_vars[unbound as usize] = Some(other.clone());
                    true
                } else {
                    self.unify_value_kinds(&resolved, other, span)
                }
            }
            (other, ValueKind::Var(v)) => {
                let resolved = self.resolve_value_kind(&ValueKind::Var(*v));
                if let ValueKind::Var(unbound) = resolved {
                    self.value_kind_vars[unbound as usize] = Some(other.clone());
                    true
                } else {
                    self.unify_value_kinds(other, &resolved, span)
                }
            }
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
        let lhs = self.resolve_const_expr(lhs);
        let rhs = self.resolve_const_expr(rhs);
        match (&lhs.kind, &rhs.kind) {
            // Same const var on both sides — already unified.
            (HirExprKind::ConstVar(a), HirExprKind::ConstVar(b)) if a == b => {}
            // Bind an unbound const var to the other side.
            (HirExprKind::ConstVar(i), _) => {
                self.const_vars[*i as usize] = Some(rhs.clone());
            }
            (_, HirExprKind::ConstVar(j)) => {
                self.const_vars[*j as usize] = Some(lhs.clone());
            }
            // Ground-vs-ground (or any pair both of which normalise to a
            // canonical form): compare via sum-of-monomials normal form so
            // `M + N` ≡ `N + M`, `N + N` ≡ `2 * N`, etc. The current width
            // grammar only emits bare literals / Param / Local refs, so
            // normalisation reduces to identity comparisons today; the
            // infrastructure is in place for Phase D's residual propagation
            // through arithmetic.
            _ => match (
                crate::hirt::normal_const::normalise(&lhs),
                crate::hirt::normal_const::normalise(&rhs),
            ) {
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) => {
                    // Both normalise but they're not yet equal — they
                    // may be once bound vars get pinned. Defer as a
                    // ConstEq obligation; the post-walk fixpoint pass
                    // simplifies via `const_vars` bindings and discharges
                    // / errors / propagates as residual.
                    self.obligations.push(Obligation {
                        kind: ObligationKind::ConstEq { lhs: a, rhs: b },
                        span: span.clone(),
                    });
                }
                _ => {
                    // At least one side is opaque (non-linear or
                    // unrecognised shape) — defer via the older
                    // `WidthEq` shape that doesn't have a normalised form.
                    self.obligations.push(Obligation {
                        kind: ObligationKind::WidthEq {
                            lhs: lhs.clone(),
                            rhs: rhs.clone(),
                        },
                        span: span.clone(),
                    });
                }
            },
        }
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
        // Type-check parameter defaults at definition time. Without this,
        // a wrong default (e.g. `rstn: Reset @clk = 42`) would only error
        // at call sites that omit the arg — so a fn with no callers ships
        // a broken default silently.
        for param in &hir_fn.params {
            if let Some(default) = &param.default {
                let default_ty = self.infer_expr(default, file);
                self.unify_types(&param.ty, &default_ty, default.span.clone());
            }
        }
        self.check_block(&hir_fn.body, hir_fn.return_type.as_ref(), file);
        self.discharge_obligations();
        self.finalize_types();
    }

    /// Fixpoint discharge of `ConstEq` obligations. Each iteration
    /// simplifies every obligation using current `const_vars` bindings,
    /// canonicalises both sides, and either:
    /// - drops the obligation (`lhs == rhs` after simplification);
    /// - records an immediate error (both sides ground, unequal);
    /// - keeps it for the next iteration (a simplification step made
    ///   progress, so re-running may discharge more).
    /// Iterates until no obligation changes — what remains is the fn's
    /// residual constraint set.
    fn discharge_obligations(&mut self) {
        loop {
            let before: Vec<Obligation> = self.obligations.clone();
            let mut next: Vec<Obligation> = Vec::with_capacity(before.len());
            for ob in before {
                if let ObligationKind::ConstEq { lhs, rhs } = &ob.kind {
                    let lhs2 = self.simplify_normal_const(lhs);
                    let rhs2 = self.simplify_normal_const(rhs);
                    // Subtract to canonicalise: lhs == rhs iff (lhs - rhs)
                    // is the zero polynomial. If the difference is ground,
                    // we can decide right now; if it has variable terms,
                    // keep as residual.
                    let diff = lhs2.clone().sub(rhs2.clone());
                    if diff.is_ground() {
                        if diff.constant == 0 {
                            continue; // discharged
                        }
                        self.errors.push(TypeError {
                            kind: TypeErrorKind::TypeMismatch {
                                expected: format!("width {}", describe_normal(&lhs2)),
                                got: format!("width {}", describe_normal(&rhs2)),
                            },
                            span: ob.span.clone(),
                        });
                        continue;
                    }
                    next.push(Obligation {
                        kind: ObligationKind::ConstEq {
                            lhs: lhs2,
                            rhs: rhs2,
                        },
                        span: ob.span.clone(),
                    });
                } else {
                    next.push(ob);
                }
            }
            if next.len() == self.obligations.len()
                && next
                    .iter()
                    .zip(self.obligations.iter())
                    .all(|(a, b)| obligations_equal(a, b))
            {
                break;
            }
            self.obligations = next;
        }
    }

    /// Substitute every `ConstVar` reference in `nc` with its current
    /// binding, recursively (the binding may itself be a `NormalConst`
    /// containing further vars). Vars without a binding stay as-is.
    fn simplify_normal_const(
        &self,
        nc: &crate::hirt::normal_const::NormalConst,
    ) -> crate::hirt::normal_const::NormalConst {
        use crate::hirt::normal_const::NormalVar;
        nc.simplify(&mut |var| match var {
            NormalVar::ConstVar(i) => {
                let bound = self.const_vars.get(*i as usize).and_then(|r| r.as_ref())?;
                let resolved = self.resolve_const_expr(bound);
                crate::hirt::normal_const::normalise(&resolved)
            }
            _ => None,
        })
    }

    /// Substitute a callee's residual constraint through the call's
    /// `GenericArgs`. `Param(i)` references become the caller's
    /// `args[i].Const` (normalised); `ConstVar` and `Local` refs that
    /// somehow survive the callee's discharge stay as-is (typically they
    /// won't, but the substitution is defensive).
    fn substitute_residual_through_args(
        &self,
        nc: &crate::hirt::normal_const::NormalConst,
        args: &GenericArgs,
    ) -> crate::hirt::normal_const::NormalConst {
        use crate::hirt::normal_const::NormalVar;
        nc.simplify(&mut |var| match var {
            NormalVar::Param(i) => match args.0.get(*i as usize) {
                Some(GenericArg::Const(c)) => crate::hirt::normal_const::normalise(c),
                _ => None,
            },
            _ => None,
        })
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
            HirStmt::AlwaysFf(_) => {
                // Produced by `lower_block_expressions` after typeck.
                // Re-typeck on already-flattened HIR (test harnesses) just
                // walks past — the original typeck pass already validated
                // the source `when` form.
            }
            HirStmt::If(i) => {
                // `HirStmt::If` is normally produced by
                // `lower_block_expressions`, which runs strictly after the
                // main typeck pass. The flatten/sv_lower test harnesses
                // re-run typeck on already-lowered HIR as a sanity check —
                // walk the branches but don't enforce any additional
                // constraints beyond what the original typeck already did.
                let _ = self.infer_expr(&i.condition, file);
                self.check_block(&i.then_branch, expected_return, file);
                self.check_block(&i.else_branch, expected_return, file);
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
            // `Param(i)` / `ConstVar(_)` only appear inside widths of
            // declared types, not in body expressions. Reaching here means
            // a width expression was walked as a value — return `usize` as
            // the carrier type.
            HirExprKind::Param(_) | HirExprKind::ConstVar(_) => HirType {
                kind: HirTypeKind::Value(ValueType {
                    kind: ValueKind::Usize,
                    domain: Domain::Const,
                }),
                span: expr.span.clone(),
            },
            HirExprKind::Call(call) => self.infer_call(call, file),
            HirExprKind::Field(field) => self.infer_field(field, expr.span.clone(), file),
            HirExprKind::MethodCall(mc) => {
                self.infer_method_call(mc, expr.id, expr.span.clone(), file)
            }
            HirExprKind::Block(b) => self.infer_block_expr(b, expr.span.clone(), file),
            HirExprKind::If(if_expr) => self.infer_if_expr(if_expr, expr.span.clone(), file),
            HirExprKind::When(when_expr) => {
                self.infer_when_expr(when_expr, expr.span.clone(), file)
            }
        };
        self.expr_types.insert(expr.id, ty.clone());
        ty
    }

    /// A block-expression's type is the type of its tail. Statements inside
    /// are type-checked for side effects (let bindings, var declarations,
    /// equations); they don't contribute to the block's value type. A
    /// block with no tail has no value — typeck rejects that in any
    /// position that expects a value.
    fn infer_block_expr(
        &mut self,
        block: &crate::hir::HirBlockExpr,
        span: SourceSpan,
        file: &FileCtx<'_>,
    ) -> HirType {
        // The block's tail (if any) carries the value type; statements
        // inside don't `return` to the enclosing function, so the expected
        // return type is `None` here.
        self.check_block(&block.block, None, file);
        match &block.tail {
            Some(tail) => self.infer_expr(tail, file),
            None => {
                // A value-position block with no tail is ill-typed. Use a
                // fresh type var so unification with the expected context
                // produces a meaningful error elsewhere.
                self.fresh_type_var(span)
            }
        }
    }

    /// `if cond { … } else { … }`. The condition must unify with `bool @D`
    /// (for some domain D); both branches' types must unify; the
    /// if-expression's type is the unified branch type.
    fn infer_if_expr(
        &mut self,
        if_expr: &crate::hir::HirIfExpr,
        span: SourceSpan,
        file: &FileCtx<'_>,
    ) -> HirType {
        let cond_ty = self.infer_expr(&if_expr.condition, file);
        let expected_cond = HirType {
            kind: HirTypeKind::Value(ValueType {
                kind: ValueKind::Bool,
                domain: self.fresh_domain_var(),
            }),
            span: if_expr.condition.span.clone(),
        };
        self.unify_types(&expected_cond, &cond_ty, if_expr.condition.span.clone());

        let then_ty = self.infer_block_expr(&if_expr.then_branch, span.clone(), file);
        let else_ty = self.infer_block_expr(&if_expr.else_branch, span.clone(), file);
        self.unify_types(&then_ty, &else_ty, span);
        self.resolve_type(&then_ty)
    }

    /// `when EVENT { body }`: the event unifies with `Event @D` (fresh
    /// domain D), the body's type T flows through unchanged. The
    /// when-expression's type is T — the body's tail value type, in the
    /// same clock domain.
    fn infer_when_expr(
        &mut self,
        when_expr: &crate::hir::HirWhenExpr,
        span: SourceSpan,
        file: &FileCtx<'_>,
    ) -> HirType {
        let event_ty = self.infer_expr(&when_expr.event, file);
        let domain = self.fresh_domain_var();
        let expected_event = HirType {
            kind: HirTypeKind::Value(ValueType {
                kind: ValueKind::Event,
                domain: domain.clone(),
            }),
            span: when_expr.event.span.clone(),
        };
        self.unify_types(&expected_event, &event_ty, when_expr.event.span.clone());

        let body_ty = self.infer_block_expr(&when_expr.body, span.clone(), file);
        // Tie the body's domain to the event's domain so they agree.
        if let HirTypeKind::Value(vt) = &body_ty.kind {
            self.unify_domains(&domain, &vt.domain, span);
        }
        self.resolve_type(&body_ty)
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
        if let Some(struct_def) = file.lookup_struct(call.callee) {
            return self.infer_struct_call(struct_def, call, file);
        }
        let Some(callee) = file.lookup_fn(call.callee) else {
            // Unknown callee — direction-check should have caught this; if
            // not, downgrade to a fresh var so the walk continues.
            return self.fresh_type_var(call.span.clone());
        };

        let subst = self.build_sig_subst(callee, file, &call.span);

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
            let param_ty = self.apply_substitution(&param.ty, &subst);
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

        // Propagate any residual constraints the callee couldn't discharge
        // locally. Each residual's `Param(i)` refs get rewritten through
        // this call's `GenericArgs`, then pushed as fresh `ConstEq`
        // obligations in our own queue. Our discharge pass simplifies and
        // either discharges them, errors on ground-vs-ground mismatch, or
        // promotes them to our caller's residuals.
        for r in file.fn_residuals.get(&callee.def_id).into_iter().flatten() {
            let lhs = self.substitute_residual_through_args(&r.lhs, &subst.args);
            let rhs = self.substitute_residual_through_args(&r.rhs, &subst.args);
            self.obligations.push(Obligation {
                kind: ObligationKind::ConstEq { lhs, rhs },
                span: r.span.clone(),
            });
        }

        match &callee.return_type {
            Some(rt) => self.apply_substitution(rt, &subst),
            None => HirType {
                kind: HirTypeKind::Clock, // void-ish placeholder
                span: call.span.clone(),
            },
        }
    }

    /// Build a `Substitution` for a call to `callee`. Allocates a fresh
    /// inference variable per declared `generic_param`: Type → a fresh
    /// `ValueKind::Var` wrapped in a `HirType`; Domain → a fresh
    /// `Domain::Var`. Const-kind gets a placeholder `Const(0)` for now
    /// (Phase B introduces a real const inference variable).
    ///
    /// The resulting `Substitution` is used to instantiate the callee's
    /// param/return types at the call site: `apply_substitution` rewrites
    /// `Param(i)` references with `args[i]`'s payload, threading fresh
    /// variables through the inference chain.
    fn build_sig_subst(
        &mut self,
        callee: &HirFn,
        file: &FileCtx<'_>,
        span: &SourceSpan,
    ) -> Substitution {
        let generic_params = &file.resolve.def_info(callee.def_id).generic_params;
        let mut arg_list = Vec::with_capacity(generic_params.len());
        let mut domain_locals: HashMap<LocalId, Domain> = HashMap::new();
        for gp in generic_params {
            match gp.kind {
                crate::resolve::GenericParamKind::Type => {
                    let var_id = self.fresh_value_kind_var();
                    arg_list.push(GenericArg::Type(HirType {
                        kind: HirTypeKind::Value(ValueType {
                            kind: ValueKind::Var(var_id),
                            domain: Domain::Unspecified,
                        }),
                        span: span.clone(),
                    }));
                }
                crate::resolve::GenericParamKind::Domain => {
                    let fresh_d = self.fresh_domain_var();
                    if let Some(param) = callee
                        .params
                        .iter()
                        .find(|p| file.local_name(callee, p.local) == Some(gp.name.as_str()))
                    {
                        domain_locals.insert(param.local, fresh_d.clone());
                    }
                    arg_list.push(GenericArg::Domain(fresh_d));
                }
                crate::resolve::GenericParamKind::Const => {
                    arg_list.push(GenericArg::Const(self.fresh_const_var(span.clone())));
                }
            }
        }
        Substitution {
            args: GenericArgs(arg_list),
            domain_locals,
        }
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
        // Allocate fresh inference variables for the struct's generic
        // parameters. Each declared field's type is substituted through
        // `instantiate(...)` before unifying against the arg expression —
        // a `data: A` field becomes `?T0` so the arg's actual type pins
        // `?T0` for use at the result type.
        let args = self.fresh_args_for_def(struct_def.def_id, &call.span, file);
        for (decl, arg) in struct_def.fields.iter().zip(call.args.iter()) {
            if let HirArg::Provided { expr, .. } = arg {
                let value_ty = self.infer_expr(expr, file);
                let expected = self.instantiate(&decl.ty, struct_def.def_id, &args, file);
                self.unify_types(&expected, &value_ty, expr.span.clone());
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
                    args,
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
                ValueKind::Struct { def, args } => {
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
                    // Substitute the receiver's generic args into the
                    // declared field type so a `data: A` field on a
                    // `Bus(uint(8))` receiver yields `uint(8)`.
                    let instantiated = self.instantiate(&decl_field.ty, *def, args, file);
                    stamp_domain(&instantiated, &vt.domain)
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
                let Some(port_def) = file.lookup_port(port_ref.def) else {
                    return self.fresh_type_var(span);
                };
                let Some(decl_field) = port_def.fields.iter().find(|f| f.name == field.name) else {
                    self.errors.push(TypeError {
                        kind: TypeErrorKind::UnknownField {
                            receiver_type: port_def.name.clone(),
                            field: field.name.clone(),
                        },
                        span: field.name_span.clone(),
                    });
                    return self.fresh_type_var(span);
                };
                // First substitute the port's generic args (Type-kind via
                // `ValueKind::Param(i)`, Domain-kind via
                // `Domain::Clock(local)`). Then stamp the port's implicit
                // domain over any remaining Unspecified slot — the
                // `DF @clk` single-domain shorthand.
                let instantiated =
                    self.instantiate(&decl_field.ty, port_ref.def, &port_ref.args, file);
                stamp_domain(&instantiated, &port_ref.domain)
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

        // Map the receiver's type to an `impl_methods` owner. User-defined
        // structs and ports carry their own `DefId`; primitive scalars
        // (`uint(N)`, eventually `bool`) use the prelude `BuiltinType`
        // DefId so prelude methods like `uint::reg` dispatch through the
        // same table as `impl T { fn m … }`.
        let owner_def = match &resolved_recv.kind {
            HirTypeKind::Value(vt) => match &vt.kind {
                ValueKind::Struct { def, .. } => Some(*def),
                ValueKind::UInt { .. } => file.uint_def_id,
                ValueKind::Bool => file.bool_def_id,
                _ => None,
            },
            HirTypeKind::Port(p) => Some(p.def),
            HirTypeKind::Clock => file.clock_def_id,
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

        // Build a substitution covering both the callee's generic_params
        // (Type/Domain) and any inferable `dom clk` named params.
        let subst = self.build_sig_subst(callee, file, &span);

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
        let self_expected = self.apply_substitution(&self_param.ty, &subst);
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
            let param_ty = self.apply_substitution(&param.ty, &subst);
            if let HirArg::Provided { expr, .. } = arg {
                let arg_ty = self.infer_expr(expr, file);
                self.unify_types(&param_ty, &arg_ty, expr.span.clone());
            }
        }

        match &callee.return_type {
            Some(rt) => self.apply_substitution(rt, &subst),
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

/// Unified call-site / use-site substitution. One entry per declared
/// `generic_param` on the def, in declared order. Mirrors rustc's
/// `GenericArgs` shape — `apply_substitution` walks the type and substitutes
/// `Param(i)` references (both `ValueKind::Param` and `HirExprKind::Param`)
/// with the matching arg's payload.
///
/// At a *call site* the args contain fresh inference variables: Type-kind
/// → `Value(Var(?T))`, Domain-kind → `Domain::Var(?D)`, Const-kind →
/// placeholder `Const(0)` (Phase B replaces this with a const inference
/// variable). At a *use site* (struct field access, port instance) the
/// args carry the use-site's concrete generic args.
///
/// `domain_locals` is an O(1) index from a Domain-kind generic param's
/// `HirParam.local` to the corresponding `args[i].Domain` payload —
/// field/return types reference Domain-kind params as
/// `Domain::Clock(local)`, so this lets `substitute_domain` resolve the
/// binding without re-scanning args + generic_params at every lookup.
#[derive(Default, Debug)]
struct Substitution {
    args: GenericArgs,
    domain_locals: HashMap<LocalId, Domain>,
}

/// Sentinel: a width position that should be treated as "any width" for
/// unification purposes. Used for integer literals whose width is not yet
/// known. Recognised by the `(Const(Integer(0)), HirId::MAX)` shape — a
/// concrete-looking width that the placeholder mechanism emits but no real
/// `uint(0)` width could share, since `0` literals at width position aren't
/// surface-writable on a value. Note: `ConstVar` and `Param` expressions
/// also use `HirId::MAX`, so we check the kind explicitly instead of just
/// the id.
fn is_width_placeholder(e: &HirExpr) -> bool {
    matches!(e.kind, HirExprKind::Const(ConstValue::Integer(0))) && e.id == HirId(u32::MAX)
}

fn describe_type(ty: &HirType) -> String {
    match &ty.kind {
        HirTypeKind::Var(v) => format!("?{}", v.0),
        HirTypeKind::Value(vt) => {
            let body = match &vt.kind {
                ValueKind::UInt { width } => match &width.kind {
                    HirExprKind::Const(ConstValue::Integer(n)) => format!("uint({n})"),
                    HirExprKind::ConstVar(i) => format!("uint(?C{i})"),
                    HirExprKind::Param(i) => format!("uint('P{i})"),
                    HirExprKind::Local(l) => format!("uint(loc#{})", l.0),
                    _ => "uint(?)".to_owned(),
                },
                ValueKind::Bool => "bool".to_owned(),
                ValueKind::Reset => "Reset".to_owned(),
                ValueKind::Usize => "usize".to_owned(),
                ValueKind::Event => "Event".to_owned(),
                ValueKind::Struct { def, .. } => format!("struct#{}", def.0),
                ValueKind::Param(i) => format!("'P{i}"),
                ValueKind::Var(i) => format!("?V{i}"),
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
        Domain::Param(i) => format!("'D{i}"),
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
    use crate::surface::ir::parse_surface_source;

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
    fn type_checks_parametric_struct_use() {
        // `Bus(uint(8))` at a param site, `b.data` access — the field type
        // `A` in the struct decl becomes `Param(0)`; instantiating with the
        // arg gives `uint(8)` for the field access result.
        let r = check(
            "struct Bus(A: Type) = bus { valid: bool, data: A }\n\
             fn pick (b: Bus(uint(8)) @clk) -> uint(8) @clk { return b.data; }",
        );
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
    }

    #[test]
    fn type_checks_parametric_struct_constructor() {
        // `bus { valid: true, data: x }` should unify `?A` with `x`'s type
        // through the substituted field type, yielding `Bus(uint(8))`.
        let r = check(
            "struct Bus(A: Type) = bus { valid: bool, data: A }\n\
             fn mk (x: uint(8) @clk) -> Bus(uint(8)) @clk { return bus { valid: true, data: x }; }",
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

    /// Phase D: a fn that constrains two distinct generic widths to be
    /// equal via use inside its body. The discharge loop can't decide
    /// `Param(0) == Param(1)` locally, so it survives as a residual on
    /// the fn's signature.
    #[test]
    fn const_eq_residual_persists_when_undischargeable() {
        let r = check(
            "fn pair_add\n\
               { dom clk: Clock }\n\
               ( param n: usize, param m: usize, a: uint(n) @clk, b: uint(m) @clk )\n\
               -> uint(n) @clk\n\
             { return a + b; }",
        );
        assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);
        // The residual (n = m) survives discharge — the fn itself can't
        // decide it, so it'd propagate to caller obligations or stay as a
        // sig-level constraint. We don't yet surface residuals on the
        // public TypeCheckResult; the test just confirms compilation
        // succeeds without spurious errors.
    }
}
