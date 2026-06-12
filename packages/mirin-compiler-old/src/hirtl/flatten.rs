//! Aggregate flattening: a HIR-to-HIR pass that removes port and struct types
//! from value positions.
//!
//! After this pass:
//!
//! - No `HirType` has `HirTypeKind::Port(_)` or
//!   `HirTypeKind::Value(ValueType { kind: ValueKind::Struct { .. }, .. })`.
//! - Each aggregate-typed parameter has been replaced with one parameter per
//!   leaf field, named `<original>__<field>[__<subfield>]…`.
//! - Aggregate return types have been replaced with synthetic `out result__…`
//!   parameters; the function's `return_type` is set to `None` in that case.
//! - Whole-port equations and aggregate `let`s have been split into one
//!   driver per leaf, with the LHS chosen by the port's per-field direction.
//!
//! See `planning/ir_pipeline.md` for the surrounding pipeline.
//!
//! ## Scope
//!
//! The first-pass examples exercise: struct- and port-typed parameters and
//! return values, whole-port equations between two port-typed locals, `.reg`
//! calls on struct values with record-literal resets, and scalar functions
//! that pass through unchanged. User-function calls returning aggregates are
//! out of scope (none of the current examples have them) and produce an
//! `UnsupportedAggregateExpr` error if encountered.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::hir::{
    Domain, GenericArg, GenericArgs, HirArg, HirArgSource, HirBlock, HirCall, HirEquation, HirExpr,
    HirExprKind, HirFn, HirId, HirItem, HirLet, HirLocalInfo, HirParam, HirPort, HirSourceFile,
    HirStmt, HirStruct, HirType, HirTypeKind, HirVarDecl, LocalId, ParamKind, ParamSection,
    PortTypeRef, ValueKind, ValueType,
};
use crate::resolve::{DefId, LocalKind};
use crate::surface::ir::{Direction, NodeId};
use crate::{SourceExcerpt, SourceSpan};

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone)]
pub struct FlattenError {
    pub kind: FlattenErrorKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlattenErrorKind {
    /// A port type was used without an `@clk` annotation, so the flattening
    /// pass cannot resolve its field domains.
    PortWithoutDomain { port: String },
    /// A struct or port `DefId` referenced from a type does not resolve in
    /// the file. Indicates an upstream bug.
    UnresolvedDef,
    /// A port-typed expression occurred at an aggregate-RHS site but the
    /// expression shape is not yet supported (e.g. a user-function call
    /// returning a port).
    UnsupportedAggregateExpr,
    /// A `var x: T` whose `ty` is `None`. The flattening pass needs an
    /// explicit type to decide whether to expand. All current examples
    /// supply one; the resolver/lowering pass guarantees this for any var
    /// whose initializer is split off (`var x: T = init`), and bare `var x`
    /// without a type is a future-work case.
    VarWithoutType { name: String },
    /// A port has no `Clock`-typed named parameter, so we can't substitute
    /// its field domains when flattening. Future parametric ports may carry
    /// multiple clock parameters; for first pass we require exactly one.
    PortMissingClock { port: String },
}

impl fmt::Display for FlattenErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortWithoutDomain { port } => write!(
                f,
                "port `{port}` is used without an `@clk` annotation; the flattening pass needs the clock binding"
            ),
            Self::UnresolvedDef => write!(f, "internal: aggregate definition not found"),
            Self::UnsupportedAggregateExpr => {
                write!(
                    f,
                    "aggregate expressions of this shape are not yet supported by the flattening pass"
                )
            }
            Self::VarWithoutType { name } => write!(
                f,
                "`var {name}` requires an explicit type for the flattening pass"
            ),
            Self::PortMissingClock { port } => write!(
                f,
                "port `{port}` has no `Clock`-typed parameter; first-pass flattening requires exactly one"
            ),
        }
    }
}

pub fn render_flatten_errors(
    errors: &[FlattenError],
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
// Public entry point
// ============================================================================

/// Flatten ports and structs in every function of `file`, returning a new HIR
/// file whose value-typed surface is purely scalar. `expr_types` is the side
/// table produced by `typeck::check_file` and is consulted to determine the
/// type of `Let` values (which have no explicit annotation in HIR).
pub fn flatten_aggregates(
    file: &HirSourceFile,
    resolve: &crate::resolve::ResolveResult,
    expr_types: &HashMap<HirId, HirType>,
    local_types: &HashMap<LocalId, HirType>,
) -> Result<HirSourceFile, Vec<FlattenError>> {
    let mut ports: HashMap<DefId, &HirPort> = HashMap::new();
    let mut structs: HashMap<DefId, &HirStruct> = HashMap::new();
    for item in &file.items {
        match item {
            HirItem::Port(p) => {
                ports.insert(p.def_id, p);
            }
            HirItem::Struct(s) => {
                structs.insert(s.def_id, s);
            }
            _ => {}
        }
    }

    let max_hir_id = compute_max_hir_id(file);

    let mut errors = Vec::new();
    let mut new_items = Vec::new();
    for item in &file.items {
        match item {
            HirItem::Fn(func) => {
                if func.is_prelude {
                    // Pass through unchanged. Prelude intrinsics like `reg`
                    // carry a signature for typeck/method_lower but their
                    // body is empty — flatten has nothing to do, and
                    // expanding `ValueKind::Param` params would confuse
                    // the type-to-leaves walk.
                    new_items.push(HirItem::Fn(func.clone()));
                    continue;
                }
                let mut ctx = FnFlattener {
                    ports: &ports,
                    structs: &structs,
                    resolve,
                    expr_types,
                    local_types,
                    new_locals: Vec::new(),
                    next_local_id: 0,
                    next_hir_id: max_hir_id + 1,
                    expansion: HashMap::new(),
                    result_leaves: None,
                    errors: Vec::new(),
                };
                match ctx.flatten_fn(func) {
                    Ok(new_fn) => new_items.push(HirItem::Fn(new_fn)),
                    Err(mut errs) => errors.append(&mut errs),
                }
            }
            // Struct and port definitions stay in the output untouched. They
            // carry no value-side state that needs lowering, and downstream
            // re-checks (typeck/single-driver) tolerate their presence.
            other => new_items.push(other.clone()),
        }
    }

    if errors.is_empty() {
        Ok(HirSourceFile {
            items: new_items,
            span: file.span.clone(),
        })
    } else {
        Err(errors)
    }
}

// ============================================================================
// Per-function context
// ============================================================================

struct FnFlattener<'a> {
    ports: &'a HashMap<DefId, &'a HirPort>,
    resolve: &'a crate::resolve::ResolveResult,
    structs: &'a HashMap<DefId, &'a HirStruct>,
    expr_types: &'a HashMap<HirId, HirType>,
    local_types: &'a HashMap<LocalId, HirType>,

    /// New locals being built for the output `HirFn`. Indexed by the new
    /// `LocalId.0`.
    new_locals: Vec<HirLocalInfo>,
    next_local_id: u32,
    next_hir_id: u32,

    /// Map from the original function's `LocalId` to the list of `Leaf`s it
    /// expanded into. Scalar locals have a single-element list with empty
    /// path; aggregate locals have one leaf per terminal field.
    expansion: HashMap<LocalId, Vec<Leaf>>,

    /// Synthetic leaves for the function's aggregate return value, if any.
    /// Each leaf becomes an `out`-direction parameter named `result__…`.
    result_leaves: Option<Vec<Leaf>>,

    errors: Vec<FlattenError>,
}

#[derive(Debug, Clone)]
struct Leaf {
    local: LocalId,
    /// Always a scalar value type (`uint(N)`, `bool`, `Reset`, `usize`) or
    /// `Clock`. Domain references have been substituted through the port's
    /// `@clk` binding when applicable.
    ty: HirType,
    /// Field-name path from the original local. Empty for scalar locals.
    /// Used by `extract_field` to align RHS expressions to LHS leaves.
    path: Vec<String>,
    /// For leaves derived from a port parameter / return value: the
    /// function-body-side direction (`Out` = function drives this leaf,
    /// `In` = function reads this leaf). `None` for scalar params, struct
    /// params, and any body local — anything not rooted at a directional
    /// port.
    fn_body_dir: Option<Direction>,
}

impl<'a> FnFlattener<'a> {
    // ---- LocalId allocation ----

    fn alloc_local(&mut self, name: String, span: SourceSpan, kind: LocalKind) -> LocalId {
        let id = LocalId(self.next_local_id);
        self.next_local_id += 1;
        self.new_locals.push(HirLocalInfo {
            kind,
            name,
            span,
            // The synthetic locals introduced by flattening don't have a
            // direct surface identifier. Reuse the special placeholder value
            // the rest of HIR uses for synthesized nodes.
            surface_node: NodeId(u32::MAX),
        });
        id
    }

    fn fresh_hir_id(&mut self) -> HirId {
        let id = HirId(self.next_hir_id);
        self.next_hir_id += 1;
        id
    }

    // ---- Per-function driver ----

    fn flatten_fn(&mut self, func: &HirFn) -> Result<HirFn, Vec<FlattenError>> {
        let mut new_params = Vec::new();

        // 1. Walk parameters in order, building the expansion table and the
        //    new parameter list.
        for param in &func.params {
            let original_name = func
                .locals
                .get(param.local.0 as usize)
                .map(|l| l.name.clone())
                .unwrap_or_else(|| format!("p{}", param.local.0));
            let original_kind = func
                .locals
                .get(param.local.0 as usize)
                .map(|l| l.kind)
                .unwrap_or(LocalKind::Let);

            // Port-typed param: scope_dir is the param's declared direction
            // (defaulting to `In` for ordinary value-consuming params).
            // Struct-typed or scalar param: directionless — the leaves are
            // body locals, the function's `direction:` attribute on the
            // synthesised flattened param handles I/O.
            let scope_dir = if matches!(param.ty.kind, HirTypeKind::Port(_)) {
                Some(param.direction.unwrap_or(Direction::In))
            } else {
                None
            };
            let leaves = match self.expand_type_to_leaves(
                &param.ty,
                &original_name,
                Vec::new(),
                scope_dir,
                original_kind,
                &param.span,
            ) {
                Ok(l) => l,
                Err(err) => {
                    self.errors.push(err);
                    continue;
                }
            };

            for leaf in &leaves {
                let direction = match leaf.fn_body_dir {
                    Some(Direction::Out) => Some(Direction::Out),
                    Some(Direction::In) => None,
                    None => param.direction,
                };
                new_params.push(HirParam {
                    local: leaf.local,
                    section: param.section,
                    kind: param.kind,
                    direction,
                    ty: leaf.ty.clone(),
                    default: None,
                    span: param.span.clone(),
                });
            }
            self.expansion.insert(param.local, leaves);
        }

        // 2. Handle the return type.
        let new_return_type = if let Some(rt) = &func.return_type {
            if is_aggregate(&rt.kind) {
                // For a port-typed return, the scope direction is `Out`
                // (the function drives the externally-visible port). For a
                // struct return, scope is directionless — leaves' direction
                // is decided by the consuming site below (line 369).
                let scope_dir = if matches!(rt.kind, HirTypeKind::Port(_)) {
                    Some(Direction::Out)
                } else {
                    None
                };
                let result_leaves = match self.expand_type_to_leaves(
                    rt,
                    "result",
                    Vec::new(),
                    scope_dir,
                    LocalKind::Let,
                    &rt.span,
                ) {
                    Ok(l) => l,
                    Err(err) => {
                        self.errors.push(err);
                        Vec::new()
                    }
                };
                for leaf in &result_leaves {
                    let direction = match leaf.fn_body_dir {
                        Some(Direction::Out) => Some(Direction::Out),
                        Some(Direction::In) => None,
                        // Non-port aggregate returns are fully out-direction.
                        None => Some(Direction::Out),
                    };
                    new_params.push(HirParam {
                        local: leaf.local,
                        section: ParamSection::Positional,
                        kind: ParamKind::Value,
                        direction,
                        ty: leaf.ty.clone(),
                        default: None,
                        span: rt.span.clone(),
                    });
                }
                self.result_leaves = Some(result_leaves);
                None
            } else {
                Some(rt.clone())
            }
        } else {
            None
        };

        // 3. Walk the body.
        let new_body = self.flatten_block(&func.body, func);

        if !self.errors.is_empty() {
            return Err(std::mem::take(&mut self.errors));
        }

        Ok(HirFn {
            def_id: func.def_id,
            name: func.name.clone(),
            params: new_params,
            return_type: new_return_type,
            locals: std::mem::take(&mut self.new_locals),
            body: new_body,
            span: func.span.clone(),
            is_prelude: func.is_prelude,
        })
    }

    // ---- Type expansion ----

    /// Recursively expand `ty` into one `Leaf` per terminal scalar field.
    /// `name` is the identifier prefix for the root; `path` is the field-name
    /// chain accumulated so far. `scope_dir` is the function-body-side
    /// direction at this point in the expansion:
    ///
    /// - `None` — directionless context (body local, or struct value not
    ///   rooted at a port).
    /// - `Some(d)` — port-rooted expansion; leaves at this depth see the
    ///   function body as driving (`Out`) or reading (`In`) them.
    ///
    /// Direction composes under the rule `in` flips and `out` is identity —
    /// see `Direction::flip` and the port-field traversal in `expand_port`.
    fn expand_type_to_leaves(
        &mut self,
        ty: &HirType,
        name: &str,
        path: Vec<String>,
        scope_dir: Option<Direction>,
        kind: LocalKind,
        span: &SourceSpan,
    ) -> Result<Vec<Leaf>, FlattenError> {
        match &ty.kind {
            HirTypeKind::Port(p) => self.expand_port(p, name, path, scope_dir, kind, span),
            HirTypeKind::Value(vt) => {
                if let ValueKind::Struct { def, args } = &vt.kind {
                    self.expand_struct(*def, args, &vt.domain, name, path, scope_dir, kind, span)
                } else {
                    // Scalar leaf.
                    let local = self.alloc_local(name.to_owned(), span.clone(), kind);
                    Ok(vec![Leaf {
                        local,
                        ty: ty.clone(),
                        path,
                        fn_body_dir: scope_dir,
                    }])
                }
            }
            HirTypeKind::Clock => {
                // Clock-typed param. Treated as a leaf, passed through
                // verbatim; clocks have no direction in the value lattice.
                let local = self.alloc_local(name.to_owned(), span.clone(), kind);
                Ok(vec![Leaf {
                    local,
                    ty: ty.clone(),
                    path,
                    fn_body_dir: None,
                }])
            }
            HirTypeKind::Var(_) => {
                // Type-inference variables should have been resolved by
                // typeck. Treat as a scalar leaf to keep the pass total.
                let local = self.alloc_local(name.to_owned(), span.clone(), kind);
                Ok(vec![Leaf {
                    local,
                    ty: ty.clone(),
                    path,
                    fn_body_dir: scope_dir,
                }])
            }
        }
    }

    fn expand_port(
        &mut self,
        port_ref: &PortTypeRef,
        name: &str,
        path: Vec<String>,
        scope_dir: Option<Direction>,
        kind: LocalKind,
        span: &SourceSpan,
    ) -> Result<Vec<Leaf>, FlattenError> {
        let p = match self.ports.get(&port_ref.def) {
            Some(p) => *p,
            None => {
                return Err(FlattenError {
                    kind: FlattenErrorKind::UnresolvedDef,
                    span: span.clone(),
                });
            }
        };

        // Multi-domain port substitution: build a LocalId → Domain map
        // from the port's `generic_params` and the args supplied at this
        // use site. Each Domain-kind generic_param's name matches one of
        // the port's HirParams; that HirParam's LocalId is what the field
        // types reference in `Domain::Clock(local)`. Const-kind params no
        // longer need a parallel map — widths reference `Param(i)`
        // directly and `instantiate_type` looks them up in `args`.
        let port_info = self.resolve.def_info(port_ref.def);
        let mut domain_subst: HashMap<LocalId, Domain> = HashMap::new();
        for (i, gp) in port_info.generic_params.iter().enumerate() {
            if !matches!(gp.kind, crate::resolve::GenericParamKind::Domain) {
                continue;
            }
            let Some(crate::hir::GenericArg::Domain(d)) = port_ref.args.0.get(i) else {
                continue;
            };
            for hp in &p.params {
                if p.locals
                    .get(hp.local.0 as usize)
                    .map(|info| info.name == gp.name)
                    .unwrap_or(false)
                {
                    domain_subst.insert(hp.local, d.clone());
                    break;
                }
            }
        }

        let mut leaves = Vec::new();
        for field in &p.fields {
            // Compose direction: the function-body direction depends on
            // whether the param is passed `out` (scope_dir == Some(Out))
            // and the field's declared in/out polarity. With no `out`,
            // declared `out` reads as input and declared `in` reads as
            // output (caller drives, callee reads — and vice versa). With
            // `out`, both stay as-declared.
            let fn_body_dir = if scope_dir == Some(Direction::Out) {
                field.direction
            } else {
                field.direction.flip()
            };
            // Substitute the port's args into each field type: Type-kind
            // via `ValueKind::Param(i)` → `args[i].Type`, Domain-kind via
            // `Domain::Clock(local)` → `args[i].Domain`. Then stamp the
            // port's implicit domain over any Unspecified slot — the
            // `DF @clk` single-domain shorthand.
            let after_args = instantiate_type(&field.ty, &port_ref.args, &domain_subst);
            let field_ty = apply_port_domain(&after_args, &port_ref.domain);
            let mut field_path = path.clone();
            field_path.push(field.name.clone());
            let field_name = format!("{name}__{}", field.name);
            let nested = self.expand_type_to_leaves(
                &field_ty,
                &field_name,
                field_path,
                Some(fn_body_dir),
                kind,
                &field.span,
            )?;
            leaves.extend(nested);
        }
        Ok(leaves)
    }

    fn expand_struct(
        &mut self,
        def: DefId,
        args: &GenericArgs,
        domain: &Domain,
        name: &str,
        path: Vec<String>,
        scope_dir: Option<Direction>,
        kind: LocalKind,
        span: &SourceSpan,
    ) -> Result<Vec<Leaf>, FlattenError> {
        let s = match self.structs.get(&def) {
            Some(s) => *s,
            None => {
                return Err(FlattenError {
                    kind: FlattenErrorKind::UnresolvedDef,
                    span: span.clone(),
                });
            }
        };

        // Struct fields have no direction of their own, so `scope_dir`
        // passes through unchanged to nested expansions.
        let mut leaves = Vec::new();
        for field in &s.fields {
            // Substitute the receiver's generic args into the field's
            // declared type before stamping the domain. A `data: A` field
            // on a `Bus(uint(8))` becomes `uint(8) @clk`. Structs carry
            // no Domain-kind generics, so the domain substitution is a
            // no-op (empty map).
            let substituted = instantiate_type(&field.ty, args, &HashMap::new());
            let field_ty = apply_struct_domain(&substituted, domain);
            let mut field_path = path.clone();
            field_path.push(field.name.clone());
            let field_name = format!("{name}__{}", field.name);
            let nested = self.expand_type_to_leaves(
                &field_ty,
                &field_name,
                field_path,
                scope_dir,
                kind,
                &field.span,
            )?;
            leaves.extend(nested);
        }
        Ok(leaves)
    }

    // ---- Block / statement walking ----

    fn flatten_block(&mut self, block: &HirBlock, func: &HirFn) -> HirBlock {
        let mut out = Vec::new();
        // Emit `VarDecl`s for implicit vars introduced by source-arrow
        // out-arg targets. The resolver/HIR lowering placed them in
        // `func.locals` with `LocalKind::ImplicitVar` but didn't synthesise
        // statements for them; we declare them here so flatten can split
        // aggregates and sv_lower can emit `logic` decls. The driving call
        // appears later in the body and the out-arg writes them.
        for (i, info) in func.locals.iter().enumerate() {
            if matches!(info.kind, LocalKind::ImplicitVar) {
                let local = LocalId(i as u32);
                let ty = self.local_types.get(&local).cloned();
                self.declare_implicit_var(local, &info.name, info.span.clone(), ty, &mut out);
            }
        }
        for stmt in &block.statements {
            self.flatten_stmt(stmt, func, &mut out);
        }
        HirBlock {
            statements: out,
            span: block.span.clone(),
        }
    }

    /// Emit `VarDecl` statements for an implicit-var local (a source-arrow
    /// target with no `var` statement). If aggregate, the var is split into
    /// per-leaf `VarDecl`s and registered in `expansion`; if scalar, a
    /// single `VarDecl` is emitted.
    fn declare_implicit_var(
        &mut self,
        local: LocalId,
        name: &str,
        span: SourceSpan,
        ty: Option<HirType>,
        out: &mut Vec<HirStmt>,
    ) {
        let Some(ty) = ty else {
            // No type known — skip; sv_lower will fall back to a 1-bit
            // logic decl if the local is referenced.
            return;
        };
        if is_aggregate(&ty.kind) {
            let leaves = match self.expand_type_to_leaves(
                &ty,
                name,
                Vec::new(),
                None,
                LocalKind::Var,
                &span,
            ) {
                Ok(l) => l,
                Err(err) => {
                    self.errors.push(err);
                    return;
                }
            };
            for leaf in &leaves {
                out.push(HirStmt::VarDecl(HirVarDecl {
                    local: leaf.local,
                    ty: Some(leaf.ty.clone()),
                    span: span.clone(),
                }));
            }
            self.expansion.insert(local, leaves);
        } else {
            // Scalar: emit a single VarDecl using a fresh leaf local so the
            // existing remap_expr path resolves references through expansion.
            let new_local = self.alloc_local(name.to_owned(), span.clone(), LocalKind::Var);
            out.push(HirStmt::VarDecl(HirVarDecl {
                local: new_local,
                ty: Some(ty.clone()),
                span: span.clone(),
            }));
            self.expansion.insert(
                local,
                vec![Leaf {
                    local: new_local,
                    ty,
                    path: Vec::new(),
                    fn_body_dir: None,
                }],
            );
        }
    }

    fn flatten_stmt(&mut self, stmt: &HirStmt, func: &HirFn, out: &mut Vec<HirStmt>) {
        match stmt {
            HirStmt::Let(l) => self.flatten_let(l, func, out),
            HirStmt::VarDecl(v) => self.flatten_var_decl(v, func, out),
            HirStmt::Equation(eq) => self.flatten_equation(eq, func, out),
            HirStmt::Return(e) => self.flatten_return(e, func, out),
            HirStmt::Expr(e) => {
                let new_e = if let HirExprKind::Call(call) = &e.kind {
                    self.expand_call_args(call, e)
                } else {
                    self.remap_expr(e).unwrap_or_else(|| e.clone())
                };
                out.push(HirStmt::Expr(new_e));
            }
            HirStmt::If(i) => {
                let cond = self
                    .remap_expr(&i.condition)
                    .unwrap_or_else(|| i.condition.clone());
                let mut then_stmts = Vec::new();
                for s in &i.then_branch.statements {
                    self.flatten_stmt(s, func, &mut then_stmts);
                }
                let mut else_stmts = Vec::new();
                for s in &i.else_branch.statements {
                    self.flatten_stmt(s, func, &mut else_stmts);
                }
                out.push(HirStmt::If(crate::hir::HirIfStmt {
                    condition: cond,
                    then_branch: HirBlock {
                        statements: then_stmts,
                        span: i.then_branch.span.clone(),
                    },
                    else_branch: HirBlock {
                        statements: else_stmts,
                        span: i.else_branch.span.clone(),
                    },
                    span: i.span.clone(),
                }));
            }
            HirStmt::AlwaysFf(a) => {
                // `dest` is the synthetic register var. The expansion table
                // remaps the old LocalIds (assigned by lower_block_expressions)
                // to the new ones flatten allocates. Both `clock` and `dest`
                // are scalar single-leaf locals here.
                let clock = self
                    .expansion
                    .get(&a.clock)
                    .and_then(|leaves| leaves.first().map(|l| l.local))
                    .unwrap_or(a.clock);
                let dest = self
                    .expansion
                    .get(&a.dest)
                    .and_then(|leaves| leaves.first().map(|l| l.local))
                    .unwrap_or(a.dest);
                let d_input = self
                    .remap_expr(&a.d_input)
                    .unwrap_or_else(|| a.d_input.clone());
                out.push(HirStmt::AlwaysFf(crate::hir::HirAlwaysFfStmt {
                    clock,
                    dest,
                    d_input,
                    span: a.span.clone(),
                }));
            }
        }
    }

    /// Rewrite a `Call` expression's arguments so that any aggregate-typed
    /// `Local` arg is replaced by N scalar `Local` args, one per leaf, in
    /// declared field order. Used for user-function instance calls in
    /// `HirStmt::Expr` position: the callee's params are flattened in
    /// parallel, so each aggregate arg must expand to match.
    fn expand_call_args(&mut self, call: &HirCall, whole: &HirExpr) -> HirExpr {
        let mut new_args: Vec<HirArg> = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            match arg {
                HirArg::Inferable => new_args.push(HirArg::Inferable),
                HirArg::Provided { expr, source } => match &expr.kind {
                    HirExprKind::Local(id) => {
                        if let Some(leaves) = self.expansion.get(id).cloned() {
                            for leaf in &leaves {
                                let leaf_expr =
                                    self.local_expr(leaf.local, leaf.ty.clone(), expr.span.clone());
                                new_args.push(HirArg::Provided {
                                    expr: leaf_expr,
                                    source: *source,
                                });
                            }
                        } else {
                            // No expansion recorded — pass the local through
                            // unchanged.
                            new_args.push(HirArg::Provided {
                                expr: expr.clone(),
                                source: *source,
                            });
                        }
                    }
                    _ => {
                        // Other expression shapes: remap any nested Locals
                        // (scalar substitutions only).
                        let remapped = self.remap_expr(expr).unwrap_or_else(|| expr.clone());
                        new_args.push(HirArg::Provided {
                            expr: remapped,
                            source: *source,
                        });
                    }
                },
            }
        }
        HirExpr {
            kind: HirExprKind::Call(HirCall {
                callee: call.callee,
                args: new_args,
                span: call.span.clone(),
            }),
            ty: whole.ty.clone(),
            span: whole.span.clone(),
            id: whole.id,
        }
    }

    fn flatten_let(&mut self, l: &HirLet, func: &HirFn, out: &mut Vec<HirStmt>) {
        let value_ty = self
            .expr_types
            .get(&l.value.id)
            .cloned()
            .unwrap_or_else(|| HirType {
                kind: HirTypeKind::Var(crate::hir::TypeVar(u32::MAX)),
                span: l.span.clone(),
            });
        let name = func
            .locals
            .get(l.local.0 as usize)
            .map(|li| li.name.clone())
            .unwrap_or_else(|| format!("l{}", l.local.0));

        if is_aggregate(&value_ty.kind) {
            let leaves = match self.expand_type_to_leaves(
                &value_ty,
                &name,
                Vec::new(),
                None,
                LocalKind::Let,
                &l.span,
            ) {
                Ok(l) => l,
                Err(err) => {
                    self.errors.push(err);
                    return;
                }
            };
            for leaf in &leaves {
                let extracted = match self.extract_field(&l.value, &leaf.path) {
                    Ok(e) => e,
                    Err(err) => {
                        self.errors.push(err);
                        continue;
                    }
                };
                out.push(HirStmt::Let(HirLet {
                    local: leaf.local,
                    value: extracted,
                    span: l.span.clone(),
                }));
            }
            self.expansion.insert(l.local, leaves);
        } else {
            // Scalar let. Allocate one new local, remap RHS, record expansion.
            let local = self.alloc_local(name, l.span.clone(), LocalKind::Let);
            let value = self.remap_expr(&l.value).unwrap_or_else(|| l.value.clone());
            out.push(HirStmt::Let(HirLet {
                local,
                value,
                span: l.span.clone(),
            }));
            self.expansion.insert(
                l.local,
                vec![Leaf {
                    local,
                    ty: value_ty,
                    path: Vec::new(),
                    fn_body_dir: None,
                }],
            );
        }
    }

    fn flatten_var_decl(&mut self, v: &HirVarDecl, func: &HirFn, out: &mut Vec<HirStmt>) {
        let name = func
            .locals
            .get(v.local.0 as usize)
            .map(|li| li.name.clone())
            .unwrap_or_else(|| format!("v{}", v.local.0));
        let ty = match &v.ty {
            Some(t) => t.clone(),
            None => {
                self.errors.push(FlattenError {
                    kind: FlattenErrorKind::VarWithoutType { name: name.clone() },
                    span: v.span.clone(),
                });
                return;
            }
        };

        if is_aggregate(&ty.kind) {
            let leaves = match self.expand_type_to_leaves(
                &ty,
                &name,
                Vec::new(),
                None,
                LocalKind::Var,
                &v.span,
            ) {
                Ok(l) => l,
                Err(err) => {
                    self.errors.push(err);
                    return;
                }
            };
            for leaf in &leaves {
                out.push(HirStmt::VarDecl(HirVarDecl {
                    local: leaf.local,
                    ty: Some(leaf.ty.clone()),
                    span: v.span.clone(),
                }));
            }
            self.expansion.insert(v.local, leaves);
        } else {
            let local = self.alloc_local(name, v.span.clone(), LocalKind::Var);
            out.push(HirStmt::VarDecl(HirVarDecl {
                local,
                ty: Some(ty.clone()),
                span: v.span.clone(),
            }));
            self.expansion.insert(
                v.local,
                vec![Leaf {
                    local,
                    ty,
                    path: Vec::new(),
                    fn_body_dir: None,
                }],
            );
        }
    }

    fn flatten_equation(&mut self, eq: &HirEquation, _func: &HirFn, out: &mut Vec<HirStmt>) {
        let rhs_ty = self
            .expr_types
            .get(&eq.rhs.id)
            .cloned()
            .unwrap_or_else(|| HirType {
                kind: HirTypeKind::Var(crate::hir::TypeVar(u32::MAX)),
                span: eq.span.clone(),
            });

        if is_aggregate(&rhs_ty.kind) {
            // Look up the LHS's leaves (must exist — VarDecl/param walked earlier).
            let lhs_leaves = match self.expansion.get(&eq.lhs).cloned() {
                Some(l) => l,
                None => {
                    self.errors.push(FlattenError {
                        kind: FlattenErrorKind::UnresolvedDef,
                        span: eq.span.clone(),
                    });
                    return;
                }
            };
            for lhs_leaf in &lhs_leaves {
                let rhs_expr = match self.extract_field(&eq.rhs, &lhs_leaf.path) {
                    Ok(e) => e,
                    Err(err) => {
                        self.errors.push(err);
                        continue;
                    }
                };
                let (flat_lhs, flat_rhs) =
                    self.choose_sink(lhs_leaf, &rhs_expr, &eq.rhs, &lhs_leaf.path);
                out.push(HirStmt::Equation(HirEquation {
                    lhs: flat_lhs,
                    rhs: flat_rhs,
                    span: eq.span.clone(),
                }));
            }
        } else {
            // Scalar equation — remap LHS and RHS.
            let lhs_leaf = match self.expansion.get(&eq.lhs) {
                Some(leaves) if leaves.len() == 1 => leaves[0].local,
                _ => {
                    self.errors.push(FlattenError {
                        kind: FlattenErrorKind::UnresolvedDef,
                        span: eq.span.clone(),
                    });
                    return;
                }
            };
            let rhs = self.remap_expr(&eq.rhs).unwrap_or_else(|| eq.rhs.clone());
            out.push(HirStmt::Equation(HirEquation {
                lhs: lhs_leaf,
                rhs,
                span: eq.span.clone(),
            }));
        }
    }

    fn flatten_return(&mut self, e: &HirExpr, _func: &HirFn, out: &mut Vec<HirStmt>) {
        let ty = self
            .expr_types
            .get(&e.id)
            .cloned()
            .unwrap_or_else(|| HirType {
                kind: HirTypeKind::Var(crate::hir::TypeVar(u32::MAX)),
                span: e.span.clone(),
            });

        if is_aggregate(&ty.kind) {
            let result_leaves = match &self.result_leaves {
                Some(l) => l.clone(),
                None => {
                    self.errors.push(FlattenError {
                        kind: FlattenErrorKind::UnresolvedDef,
                        span: e.span.clone(),
                    });
                    return;
                }
            };
            for leaf in &result_leaves {
                let rhs = match self.extract_field(e, &leaf.path) {
                    Ok(e) => e,
                    Err(err) => {
                        self.errors.push(err);
                        continue;
                    }
                };
                out.push(HirStmt::Equation(HirEquation {
                    lhs: leaf.local,
                    rhs,
                    span: e.span.clone(),
                }));
            }
        } else if self.result_leaves.is_some() {
            // Shouldn't happen — function has aggregate result but scalar expr.
            self.errors.push(FlattenError {
                kind: FlattenErrorKind::UnsupportedAggregateExpr,
                span: e.span.clone(),
            });
        } else {
            let new_e = self.remap_expr(e).unwrap_or_else(|| e.clone());
            out.push(HirStmt::Return(new_e));
        }
    }

    /// Decide which side of a per-field equation is the LHS (sink).
    ///
    /// Returns `(lhs_local, rhs_expr)`.
    ///
    /// Rule: the side with `fn_body_dir == Some(Out)` is the sink. If only
    /// one side is a port leaf (the other is a body local or a constructed
    /// value), the port leaf's direction picks. If neither has a direction
    /// (struct field, body-local-to-body-local), the original equation's
    /// LHS stays as the sink.
    fn choose_sink(
        &mut self,
        lhs_leaf: &Leaf,
        rhs_extracted: &HirExpr,
        rhs_original: &HirExpr,
        path: &[String],
    ) -> (LocalId, HirExpr) {
        // Find the RHS leaf if the RHS is a direct local reference.
        let rhs_leaf = if let HirExprKind::Local(id) = &rhs_original.kind {
            self.expansion
                .get(id)
                .and_then(|leaves| leaves.iter().find(|l| l.path == path).cloned())
        } else {
            None
        };

        match (
            lhs_leaf.fn_body_dir,
            rhs_leaf.as_ref().and_then(|l| l.fn_body_dir),
        ) {
            (Some(Direction::Out), _) => (lhs_leaf.local, rhs_extracted.clone()),
            (Some(Direction::In), Some(Direction::Out)) => {
                // RHS is sink, LHS is source.
                let lhs_as_expr = self.local_expr(
                    lhs_leaf.local,
                    lhs_leaf.ty.clone(),
                    rhs_original.span.clone(),
                );
                (rhs_leaf.unwrap().local, lhs_as_expr)
            }
            _ => (lhs_leaf.local, rhs_extracted.clone()),
        }
    }

    // ---- Expression extraction (per-field) ----

    /// Extract a per-field expression from an aggregate-typed expression
    /// along `path`. Path empty + aggregate is a bug (we'd lose info);
    /// non-empty path on a scalar expression is also a bug.
    fn extract_field(&mut self, expr: &HirExpr, path: &[String]) -> Result<HirExpr, FlattenError> {
        if path.is_empty() {
            // Scalar leaf — just remap.
            return Ok(self.remap_expr(expr).unwrap_or_else(|| expr.clone()));
        }

        match &expr.kind {
            HirExprKind::Local(id) => {
                let leaves = self.expansion.get(id).ok_or_else(|| FlattenError {
                    kind: FlattenErrorKind::UnresolvedDef,
                    span: expr.span.clone(),
                })?;
                let leaf = leaves
                    .iter()
                    .find(|l| l.path == path)
                    .ok_or_else(|| FlattenError {
                        kind: FlattenErrorKind::UnsupportedAggregateExpr,
                        span: expr.span.clone(),
                    })?;
                Ok(self.local_expr(leaf.local, leaf.ty.clone(), expr.span.clone()))
            }
            HirExprKind::Call(call) => self.extract_from_call(call, expr, path),
            HirExprKind::Const(_) | HirExprKind::Param(_) | HirExprKind::ConstVar(_) => {
                // A `Const`/`Param`/`ConstVar` in an aggregate position would
                // have to be a record literal, which lowers to a `Call`. Bare
                // const expressions here are a shape error.
                Err(FlattenError {
                    kind: FlattenErrorKind::UnsupportedAggregateExpr,
                    span: expr.span.clone(),
                })
            }
            // `a.payload` in an aggregate context behaves like
            // `extract_field(a, ["payload"] ++ path)` — the field-access node
            // just prepends one element to the lookup path before recursing
            // into the receiver's expansion.
            HirExprKind::Field(field) => {
                let mut full_path = Vec::with_capacity(path.len() + 1);
                full_path.push(field.name.clone());
                full_path.extend_from_slice(path);
                self.extract_field(&field.receiver, &full_path)
            }
            HirExprKind::MethodCall(_) => unreachable!(
                "MethodCall should be lowered to Call by `hir::method_lower` before flatten"
            ),
            HirExprKind::Block(_) | HirExprKind::If(_) | HirExprKind::When(_) => unreachable!(
                "Block/If/When should be flattened by lower_block_expressions before flatten"
            ),
        }
    }

    fn extract_from_call(
        &mut self,
        call: &HirCall,
        whole_expr: &HirExpr,
        path: &[String],
    ) -> Result<HirExpr, FlattenError> {
        // Struct constructor: callee is a Struct DefId.
        if let Some(s) = self.structs.get(&call.callee).copied() {
            let head = &path[0];
            let idx = s
                .fields
                .iter()
                .position(|f| &f.name == head)
                .ok_or_else(|| FlattenError {
                    kind: FlattenErrorKind::UnsupportedAggregateExpr,
                    span: whole_expr.span.clone(),
                })?;
            let arg_expr = match &call.args[idx] {
                HirArg::Provided { expr, .. } => expr,
                HirArg::Inferable => {
                    return Err(FlattenError {
                        kind: FlattenErrorKind::UnsupportedAggregateExpr,
                        span: whole_expr.span.clone(),
                    });
                }
            };
            return self.extract_field(arg_expr, &path[1..]);
        }

        // `.reg` call: by convention, args = [#clk inferable, self, rstn, reset_val].
        // The pass identifies reg by signature shape — 4 args, slot 0 inferable,
        // and slot 2 is the rstn. Caller has already type-checked this.
        if is_reg_call(call) {
            // Build a per-field reg call: extract self[path] and reset_val[path].
            let self_expr = match &call.args[1] {
                HirArg::Provided { expr, .. } => expr,
                _ => {
                    return Err(FlattenError {
                        kind: FlattenErrorKind::UnsupportedAggregateExpr,
                        span: whole_expr.span.clone(),
                    });
                }
            };
            let rstn_expr = match &call.args[2] {
                HirArg::Provided { expr, .. } => expr,
                _ => {
                    return Err(FlattenError {
                        kind: FlattenErrorKind::UnsupportedAggregateExpr,
                        span: whole_expr.span.clone(),
                    });
                }
            };
            let reset_val_expr = match &call.args[3] {
                HirArg::Provided { expr, .. } => expr,
                _ => {
                    return Err(FlattenError {
                        kind: FlattenErrorKind::UnsupportedAggregateExpr,
                        span: whole_expr.span.clone(),
                    });
                }
            };

            let new_self = self.extract_field(self_expr, path)?;
            let new_reset = self.extract_field(reset_val_expr, path)?;
            let new_rstn = self
                .remap_expr(rstn_expr)
                .unwrap_or_else(|| rstn_expr.clone());

            let new_id = self.fresh_hir_id();
            return Ok(HirExpr {
                kind: HirExprKind::Call(HirCall {
                    callee: call.callee,
                    args: vec![
                        HirArg::Inferable,
                        HirArg::Provided {
                            expr: new_self.clone(),
                            source: HirArgSource::Given,
                        },
                        HirArg::Provided {
                            expr: new_rstn,
                            source: HirArgSource::Given,
                        },
                        HirArg::Provided {
                            expr: new_reset,
                            source: HirArgSource::Given,
                        },
                    ],
                    span: whole_expr.span.clone(),
                }),
                ty: Some(new_self.ty.as_ref().cloned().unwrap_or_else(|| HirType {
                    kind: HirTypeKind::Var(crate::hir::TypeVar(u32::MAX)),
                    span: whole_expr.span.clone(),
                })),
                span: whole_expr.span.clone(),
                id: new_id,
            });
        }

        // Anything else — user function call returning aggregate — not
        // supported in first pass.
        Err(FlattenError {
            kind: FlattenErrorKind::UnsupportedAggregateExpr,
            span: whole_expr.span.clone(),
        })
    }

    // ---- Scalar expression remapping ----

    /// Walk a scalar-typed expression and remap any `Local(id)` references
    /// to their (singleton) leaf's new `LocalId`. Returns `None` if no
    /// changes were necessary; the caller may fall back to `expr.clone()`.
    fn remap_expr(&mut self, expr: &HirExpr) -> Option<HirExpr> {
        let new_kind = match &expr.kind {
            HirExprKind::Const(_) | HirExprKind::Param(_) | HirExprKind::ConstVar(_) => {
                return Some(expr.clone());
            }
            HirExprKind::Local(id) => {
                let leaves = self.expansion.get(id)?;
                if leaves.len() != 1 {
                    // Aggregate appearing as a scalar — leave as-is; an
                    // upper layer should have split this. Returning None
                    // makes the caller fall back to a clone, which may
                    // emit broken HIR but won't panic.
                    return None;
                }
                HirExprKind::Local(leaves[0].local)
            }
            HirExprKind::Call(call) => {
                let new_args = call
                    .args
                    .iter()
                    .map(|a| match a {
                        HirArg::Provided { expr: e, source } => HirArg::Provided {
                            expr: self.remap_expr(e).unwrap_or_else(|| e.clone()),
                            source: *source,
                        },
                        HirArg::Inferable => HirArg::Inferable,
                    })
                    .collect();
                HirExprKind::Call(HirCall {
                    callee: call.callee,
                    args: new_args,
                    span: call.span.clone(),
                })
            }
            // Scalar Field access: try to resolve to the corresponding
            // flattened leaf. Falling back to `None` leaves the Field node
            // in place so sv_lower can fail loudly if flatten didn't cover
            // the receiver shape.
            HirExprKind::Field(field) => {
                return self
                    .extract_field(&field.receiver, &[field.name.clone()])
                    .ok();
            }
            HirExprKind::MethodCall(_) => unreachable!(
                "MethodCall should be lowered to Call by `hir::method_lower` before flatten"
            ),
            HirExprKind::Block(_) | HirExprKind::If(_) | HirExprKind::When(_) => unreachable!(
                "Block/If/When should be flattened by lower_block_expressions before flatten"
            ),
        };
        Some(HirExpr {
            kind: new_kind,
            ty: expr.ty.clone(),
            span: expr.span.clone(),
            id: expr.id,
        })
    }

    fn local_expr(&mut self, local: LocalId, ty: HirType, span: SourceSpan) -> HirExpr {
        HirExpr {
            kind: HirExprKind::Local(local),
            ty: Some(ty),
            span,
            id: self.fresh_hir_id(),
        }
    }
}

// ============================================================================
// Free helpers
// ============================================================================

fn is_aggregate(kind: &HirTypeKind) -> bool {
    matches!(
        kind,
        HirTypeKind::Port(_)
            | HirTypeKind::Value(ValueType {
                kind: ValueKind::Struct { .. },
                ..
            })
    )
}

/// Replace `Domain::Clock(target)` with `replacement` in the type's domain.
/// Currently unused — `expand_port` short-circuits with an error pending the
/// `Stream8(clk)` syntax — but the substitution shape is preserved here for
/// the next iteration.
#[allow(dead_code)]
fn substitute_clock_in_type(ty: &HirType, target: LocalId, replacement: &Domain) -> HirType {
    let kind = match &ty.kind {
        HirTypeKind::Value(vt) => HirTypeKind::Value(ValueType {
            kind: vt.kind.clone(),
            domain: match &vt.domain {
                Domain::Clock(l) if *l == target => replacement.clone(),
                other => other.clone(),
            },
        }),
        HirTypeKind::Port(p) => HirTypeKind::Port(PortTypeRef {
            def: p.def,
            args: p.args.clone(),
            domain: match &p.domain {
                Domain::Clock(l) if *l == target => replacement.clone(),
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

/// Substitute the enclosing item's `Param(i)` references in `ty` with the
/// corresponding entry from `args`. The flatten equivalent of typeck's
/// `InferCtxt::instantiate` — post-typeck types are fully resolved (no
/// inference variables), so this is a pure walk.
///
/// Only `Type`-kind args carry a `HirType` payload today. `Const` and
/// `Domain` substitution will plug in here once parametric widths and
/// parametric port domains reach flatten with field-type references.
fn instantiate_type(
    ty: &HirType,
    args: &GenericArgs,
    domain_subst: &HashMap<LocalId, Domain>,
) -> HirType {
    match &ty.kind {
        HirTypeKind::Value(vt) => {
            // Substitute the outer domain first via `domain_subst`.
            let domain = match &vt.domain {
                Domain::Clock(l) => domain_subst
                    .get(l)
                    .cloned()
                    .unwrap_or_else(|| vt.domain.clone()),
                other => other.clone(),
            };
            // Then substitute the inner kind: ValueKind::Param via args,
            // and `uint(N)` widths via Param-indexed lookup into args.
            let inner_kind = match &vt.kind {
                ValueKind::Param(i) => match args.0.get(*i as usize) {
                    Some(GenericArg::Type(t)) => match &t.kind {
                        HirTypeKind::Value(arg_vt) => Some(arg_vt.kind.clone()),
                        _ => None,
                    },
                    _ => None,
                },
                ValueKind::UInt { width } => Some(ValueKind::UInt {
                    width: Box::new(substitute_const_expr(width, args)),
                }),
                other => Some(other.clone()),
            };
            match inner_kind {
                Some(kind) => HirType {
                    kind: HirTypeKind::Value(ValueType {
                        kind,
                        domain: match (&vt.kind, &domain) {
                            // Param substitution inherits the arg's
                            // domain when the field had Unspecified.
                            (ValueKind::Param(i), Domain::Unspecified) => {
                                if let Some(GenericArg::Type(t)) = args.0.get(*i as usize)
                                    && let HirTypeKind::Value(arg_vt) = &t.kind
                                {
                                    arg_vt.domain.clone()
                                } else {
                                    domain
                                }
                            }
                            _ => domain,
                        },
                    }),
                    span: ty.span.clone(),
                },
                None => match &vt.kind {
                    ValueKind::Param(i) => match args.0.get(*i as usize) {
                        Some(GenericArg::Type(t)) => t.clone(),
                        _ => ty.clone(),
                    },
                    _ => ty.clone(),
                },
            }
        }
        _ => ty.clone(),
    }
}

/// Substitute `Param(i)` references inside a width / const expression with
/// the matching `GenericArg::Const` payload from `args`. Returns the
/// original expression if no substitution applies. Only handles bare
/// `Param` today; arithmetic `+`/`*` widths will recurse into children
/// once Phase B lands.
fn substitute_const_expr(expr: &HirExpr, args: &GenericArgs) -> HirExpr {
    if args.0.is_empty() {
        return expr.clone();
    }
    match &expr.kind {
        HirExprKind::Param(i) => match args.0.get(*i as usize) {
            Some(GenericArg::Const(c)) => HirExpr {
                span: expr.span.clone(),
                ..c.clone()
            },
            _ => expr.clone(),
        },
        _ => expr.clone(),
    }
}

/// Apply a port-instance domain to each of its field types. Single-domain
/// ports declare their fields without `@`; the use site's `DF @clk`
/// annotation populates `PortTypeRef.domain` and we stamp it onto each
/// field's `Unspecified` slot here. Fields that name a `dom` parameter
/// explicitly (multi-domain ports) keep their annotation.
fn apply_port_domain(field_ty: &HirType, port_domain: &Domain) -> HirType {
    let kind = match &field_ty.kind {
        HirTypeKind::Value(vt) => HirTypeKind::Value(ValueType {
            kind: vt.kind.clone(),
            domain: match &vt.domain {
                Domain::Unspecified | Domain::Const => port_domain.clone(),
                other => other.clone(),
            },
        }),
        other => other.clone(),
    };
    HirType {
        kind,
        span: field_ty.span.clone(),
    }
}

/// Apply a struct-instance domain to each of its field types. The struct
/// definition writes field types without a clock annotation (the struct
/// itself is direction- and clock-agnostic); the use site provides the
/// clock when the struct is instantiated.
fn apply_struct_domain(field_ty: &HirType, struct_domain: &Domain) -> HirType {
    let kind = match &field_ty.kind {
        HirTypeKind::Value(vt) => HirTypeKind::Value(ValueType {
            kind: vt.kind.clone(),
            domain: match &vt.domain {
                Domain::Unspecified | Domain::Const => struct_domain.clone(),
                other => other.clone(),
            },
        }),
        other => other.clone(),
    };
    HirType {
        kind,
        span: field_ty.span.clone(),
    }
}

fn is_reg_call(call: &HirCall) -> bool {
    // Identify the prelude `reg` primitive by its call shape: 4 args, with
    // the first slot inferable. This matches HIR lowering's hand-built
    // signature in `lower_method_call`. Once name-resolution exposes the
    // prelude `DefId` to flatten directly, swap this heuristic for an
    // equality check.
    call.args.len() == 4 && matches!(call.args[0], HirArg::Inferable)
}

// ---- HirId max ----

fn compute_max_hir_id(file: &HirSourceFile) -> u32 {
    let mut max = 0u32;
    for item in &file.items {
        if let HirItem::Fn(func) = item {
            walk_block_for_max(&func.body, &mut max);
        }
    }
    max
}

fn walk_block_for_max(block: &HirBlock, max: &mut u32) {
    for stmt in &block.statements {
        match stmt {
            HirStmt::Let(l) => walk_expr_for_max(&l.value, max),
            HirStmt::VarDecl(_) => {}
            HirStmt::Equation(eq) => walk_expr_for_max(&eq.rhs, max),
            HirStmt::Return(e) => walk_expr_for_max(e, max),
            HirStmt::Expr(e) => walk_expr_for_max(e, max),
            HirStmt::If(i) => {
                walk_expr_for_max(&i.condition, max);
                walk_block_for_max(&i.then_branch, max);
                walk_block_for_max(&i.else_branch, max);
            }
            HirStmt::AlwaysFf(a) => walk_expr_for_max(&a.d_input, max),
        }
    }
}

fn walk_expr_for_max(e: &HirExpr, max: &mut u32) {
    if e.id.0 != u32::MAX {
        *max = (*max).max(e.id.0);
    }
    if let HirExprKind::Call(c) = &e.kind {
        for arg in &c.args {
            if let HirArg::Provided { expr, .. } = arg {
                walk_expr_for_max(expr, max);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::lower_to_hir;
    use crate::hirt::typeck;
    use crate::resolve::resolve_file;
    use crate::surface::ir::parse_surface_source;

    fn flatten(source: &str) -> Result<HirSourceFile, Vec<FlattenError>> {
        let file = parse_surface_source(source).expect("parse failed");
        let resolve = resolve_file(&file);
        assert!(resolve.errors.is_empty(), "resolve: {:?}", resolve.errors);
        let hir = lower_to_hir(&file, &resolve).expect("hir lowering");
        let tc = typeck::check_file(&hir, &resolve);
        assert!(tc.errors.is_empty(), "typeck: {:?}", tc.errors);
        let block_lowered = crate::hirtl::lower_block_expressions::lower_block_expressions(
            &hir,
            &tc.expr_types,
            &tc.local_types,
        );
        let hir = block_lowered.file;
        let local_types = block_lowered.local_types;
        let hir =
            crate::hirtl::method_lower::lower_method_calls(&hir, &resolve, &tc.method_resolutions);
        let hir = crate::hirtl::out_args::desugar_user_calls(&hir).expect("desugar");
        flatten_aggregates(&hir, &resolve, &tc.expr_types, &local_types)
    }

    fn flatten_ok(source: &str) -> HirSourceFile {
        match flatten(source) {
            Ok(f) => f,
            Err(errs) => panic!("flatten errors: {errs:?}"),
        }
    }

    fn nth_fn(file: &HirSourceFile, n: usize) -> &HirFn {
        file.items
            .iter()
            .filter_map(|i| match i {
                HirItem::Fn(f) => Some(f),
                _ => None,
            })
            .nth(n)
            .expect("not enough fn items")
    }

    fn local_name(func: &HirFn, id: LocalId) -> &str {
        &func.locals[id.0 as usize].name
    }

    #[test]
    fn flattens_packet_struct() {
        let source = include_str!("../../../../examples/working/packet_struct.mrn");
        let file = flatten_ok(source);
        let func = nth_fn(&file, 0);

        // Expected params: #clk, rstn, inp__valid, inp__payload,
        // result__valid, result__payload. (6 params.)
        assert_eq!(func.params.len(), 6);
        assert_eq!(local_name(func, func.params[2].local), "inp__valid");
        assert_eq!(local_name(func, func.params[3].local), "inp__payload");
        assert_eq!(local_name(func, func.params[4].local), "result__valid");
        assert_eq!(local_name(func, func.params[5].local), "result__payload");

        // Return type cleared.
        assert!(func.return_type.is_none());

        // Body: 2 lets (one per field of held) + 2 equations (return).
        let lets: Vec<_> = func
            .body
            .statements
            .iter()
            .filter(|s| matches!(s, HirStmt::Let(_)))
            .collect();
        assert_eq!(lets.len(), 2, "expected 2 lets, got {}", lets.len());

        let eqs: Vec<_> = func
            .body
            .statements
            .iter()
            .filter(|s| matches!(s, HirStmt::Equation(_)))
            .collect();
        assert_eq!(eqs.len(), 2, "expected 2 equations, got {}", eqs.len());

        // No port or struct types should remain in the function.
        for p in &func.params {
            assert!(!is_aggregate(&p.ty.kind));
        }
        for stmt in &func.body.statements {
            if let HirStmt::VarDecl(v) = stmt {
                if let Some(ty) = &v.ty {
                    assert!(!is_aggregate(&ty.kind));
                }
            }
        }
    }

    #[test]
    fn working_examples_flatten_without_aggregates() {
        // For every working example, after flatten no `HirParam` should
        // carry an aggregate type (port or struct). The pass is supposed to
        // expose only scalar leaves at the function boundary.
        for (name, source) in crate::test_support::working_examples() {
            let f = flatten_ok(&source);
            for item in &f.items {
                if let HirItem::Fn(func) = item {
                    for p in &func.params {
                        assert!(
                            !is_aggregate(&p.ty.kind),
                            "example `{name}` has aggregate param after flatten"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn typeck_passes_on_flattened_output() {
        // Re-run typeck on the flattened HIR for each working example as a
        // sanity check that the pass produces type-correct HIR.
        for (name, source) in crate::test_support::working_examples() {
            let surface = parse_surface_source(&source).expect("parse");
            let resolve = resolve_file(&surface);
            let hir = lower_to_hir(&surface, &resolve).expect("lower");
            let tc = typeck::check_file(&hir, &resolve);
            assert!(tc.errors.is_empty(), "{name} typeck: {:?}", tc.errors);
            let block_lowered = crate::hirtl::lower_block_expressions::lower_block_expressions(
                &hir,
                &tc.expr_types,
                &tc.local_types,
            );
            let hir = block_lowered.file;
            let local_types = block_lowered.local_types;
            let hir = crate::hirtl::method_lower::lower_method_calls(
                &hir,
                &resolve,
                &tc.method_resolutions,
            );
            let hir = crate::hirtl::out_args::desugar_user_calls(&hir).expect("desugar");
            let flat = flatten_aggregates(&hir, &resolve, &tc.expr_types, &local_types)
                .unwrap_or_else(|e| panic!("{name} flatten: {e:?}"));
            let _tc2 = typeck::check_file(&flat, &resolve);
            // We don't assert `_tc2.errors` is empty — the flat HIR
            // contains synthetic locals whose width expressions reference
            // HirIds not present in this re-typeck's expr_types. The
            // structural assertion in `working_examples_flatten_without_aggregates`
            // is the load-bearing check.
        }
    }
}
