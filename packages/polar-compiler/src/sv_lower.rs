//! Flattened HIR → SV IR.
//!
//! Walks every `HirFn` in a flattened HIR file and builds one `SvModule` per
//! function. The transform is structural: there is no analysis here, just
//! shape mapping. See `planning/ir_pipeline.md` for the wider pipeline.
//!
//! Notable rules:
//!
//! - `Clock` params become input ports of single-bit width; the first one
//!   becomes the module's clock signal for any `always_ff` block.
//! - `const` params become SV `parameter int` declarations.
//! - Scalar value params become `input`/`output logic [W-1:0]` ports based on
//!   the param's HIR direction.
//! - Scalar return types yield a synthetic `output logic [W-1:0] result`
//!   port; the function's `Return e` becomes `assign result = e`.
//! - `let lhs = .reg(...)` and `var lhs … ; lhs = .reg(...)` become
//!   `logic` + `always_ff` (synchronous active-low reset).
//! - Other `let`s and equations become `logic` + continuous `assign`.

use std::collections::HashMap;

use crate::hir::{
    ConstValue, Domain, HirArg, HirBlock, HirCall, HirExpr, HirExprKind, HirFn, HirId, HirItem,
    HirSourceFile, HirStmt, HirTypeKind, LocalId, ParamKind, ValueKind, ValueType,
};
use crate::normal_const::{NormalConst, NormalVar};
use crate::resolve::{DefId, ResolveResult};
use crate::surface_ir::Direction;
use crate::sv_ir::{
    SvAlwaysFf, SvBinOp, SvExpr, SvFile, SvInstance, SvItem, SvLogicDecl, SvModule, SvParameter,
    SvPort, SvPortDirection, SvSeqAssign, SvType,
};
use crate::typeck::FnResidual;

/// Lower a flattened HIR file to SV IR. `resolve` is used to identify
/// prelude defs (`reg`, `+`, `*`) and to qualify method names with their
/// owner type. `fn_residuals` carries Phase D's per-fn residual
/// constraints, which become elaboration-time `initial assert(…)` items
/// on the matching module.
pub fn lower_to_sv(
    file: &HirSourceFile,
    resolve: &ResolveResult,
    fn_residuals: &std::collections::HashMap<DefId, Vec<FnResidual>>,
) -> SvFile {
    // Prelude `HirFn`s (today: `reg`) carry a signature for typeck and
    // method_lower but must not become SV modules — their call sites
    // lower inline (e.g. `reg` → `always_ff`). Skip them everywhere we'd
    // emit module-shaped output.
    let mut user_fns: HashMap<DefId, &HirFn> = HashMap::new();
    for item in &file.items {
        if let HirItem::Fn(func) = item {
            if func.is_prelude {
                continue;
            }
            user_fns.insert(func.def_id, func);
        }
    }
    let mut module_names: HashMap<DefId, String> = HashMap::new();
    for item in &file.items {
        if let HirItem::Fn(func) = item {
            if func.is_prelude {
                continue;
            }
            module_names.insert(func.def_id, sv_module_name(func, resolve));
        }
    }
    let defs = BackendDefs {
        reg: resolve.def_id("reg"),
        add: resolve.def_id("+"),
        mul: resolve.def_id("*"),
        user_fns,
        module_names,
        resolve,
    };
    let mut modules = Vec::new();
    for item in &file.items {
        if let HirItem::Fn(func) = item {
            if func.is_prelude {
                continue;
            }
            let residuals = fn_residuals
                .get(&func.def_id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            modules.push(lower_fn(func, &defs, residuals));
        }
    }
    SvFile { modules }
}

/// Build the SV module name for a Polar function. Methods are qualified by
/// their impl owner (e.g. `Option::reg` → `Option__reg`); free functions
/// keep their bare name.
fn sv_module_name(func: &HirFn, resolve: &ResolveResult) -> String {
    let info = resolve.def_info(func.def_id);
    match info.kind {
        crate::resolve::DefKind::Method { owner } => {
            let owner_name = &resolve.def_info(owner).name;
            format!("{owner_name}__{}", func.name)
        }
        _ => func.name.clone(),
    }
}

#[derive(Debug, Clone)]
struct BackendDefs<'a> {
    reg: Option<DefId>,
    add: Option<DefId>,
    mul: Option<DefId>,
    /// Post-flatten user-defined functions, keyed by their `DefId`. The SV
    /// instance lowering path reads this to find the callee's flattened
    /// port list (names, directions, types).
    user_fns: HashMap<DefId, &'a HirFn>,
    /// SV module name for each user fn (qualified for methods). Used both
    /// at the module-definition site and at every instance use site.
    module_names: HashMap<DefId, String>,
    /// Used to look up the enclosing def's `generic_params` when emitting
    /// `HirExprKind::Param(i)` inside widths (e.g. `uint(N)` where N is a
    /// `param N: usize` on the parametric def).
    resolve: &'a ResolveResult,
}

/// Find the `LocalId` for the i-th generic param of `func`. The generic
/// param's name (recorded in `resolve.def_info`) matches one of `func`'s
/// HirParam slots; we use that to look up the LocalId.
fn local_for_generic_param(i: u32, func: &HirFn, defs: &BackendDefs<'_>) -> Option<LocalId> {
    let gp = defs
        .resolve
        .def_info(func.def_id)
        .generic_params
        .get(i as usize)?;
    func.params.iter().find_map(|p| {
        let name = &func.locals.get(p.local.0 as usize)?.name;
        if name == &gp.name {
            Some(p.local)
        } else {
            None
        }
    })
}

// ============================================================================
// Per-function lowering
// ============================================================================

fn lower_fn(func: &HirFn, defs: &BackendDefs<'_>, residuals: &[FnResidual]) -> SvModule {
    let mut parameters = Vec::new();
    let mut ports = Vec::new();
    let mut items = Vec::new();
    let mut local_types: HashMap<LocalId, SvType> = HashMap::new();
    // Map from a clock-domain local (the `#clk` param's LocalId) to the
    // identifier the SV emitter uses for it. Set when we lower the Clock
    // param; reused by every `always_ff` block.
    let mut clock_names: HashMap<LocalId, String> = HashMap::new();

    // Disambiguate Polar identifier names for the SV-side namespace. Polar
    // allows `let` shadowing (`let data = ... ; let data = ...`), so two
    // distinct `LocalId`s can share a `name`. SV identifiers must be unique
    // within a module, so we suffix duplicates with `_1`, `_2`, … in source
    // order; the first occurrence keeps its original name.
    let local_names = build_local_name_map(func);

    // --- Params ---
    for param in &func.params {
        let name = local_names
            .get(&param.local)
            .cloned()
            .unwrap_or_else(|| local_name(func, param.local).to_owned());
        match &param.ty.kind {
            HirTypeKind::Clock => {
                ports.push(SvPort {
                    direction: SvPortDirection::Input,
                    ty: SvType::bit(),
                    name: name.clone(),
                });
                local_types.insert(param.local, SvType::bit());
                clock_names.insert(param.local, name);
            }
            HirTypeKind::Value(vt) => {
                if matches!(param.kind, ParamKind::Param) {
                    // `param`-kind binding → SV parameter, no port.
                    parameters.push(SvParameter {
                        name: name.clone(),
                        default: param
                            .default
                            .as_ref()
                            .map(|e| lower_expr(e, func, defs, &local_names)),
                    });
                    local_types.insert(param.local, SvType::bit());
                } else {
                    let sv_ty = sv_type_for_value(vt, func, defs, &local_names);
                    let direction = match param.direction {
                        Some(Direction::Out) => SvPortDirection::Output,
                        _ => SvPortDirection::Input,
                    };
                    ports.push(SvPort {
                        direction,
                        ty: sv_ty.clone(),
                        name: name.clone(),
                    });
                    local_types.insert(param.local, sv_ty);
                }
            }
            HirTypeKind::Port(_) => {
                // Should have been flattened away. Skip with no port emitted.
            }
            HirTypeKind::Var(_) => {
                // Unresolved inference variable; treat as 1-bit to keep the
                // SV lowering total. Real code should reach here with
                // concrete value types only.
                local_types.insert(param.local, SvType::bit());
            }
        }
    }

    // --- Scalar return → synthetic `result` port. Aggregate returns are
    //     already represented as out-direction params by the flattening pass.
    if let Some(rt) = &func.return_type {
        if let HirTypeKind::Value(vt) = &rt.kind {
            let sv_ty = sv_type_for_value(vt, func, defs, &local_names);
            ports.push(SvPort {
                direction: SvPortDirection::Output,
                ty: sv_ty,
                name: "result".to_owned(),
            });
        }
    }

    // --- Body ---
    let mut instance_counts: HashMap<String, u32> = HashMap::new();
    lower_block(
        &func.body,
        func,
        defs,
        &clock_names,
        &local_names,
        &mut local_types,
        &mut instance_counts,
        &mut items,
    );

    // Phase D′: emit a residual constraint as `initial begin assert (lhs
    // == rhs); end`. Each residual `(NormalConst, NormalConst)` reduces to
    // a sum-of-monomials predicate over the module's SV parameters. If
    // the difference normalises to zero the constraint is statically
    // satisfied and we emit nothing.
    for r in residuals {
        let diff = r.lhs.clone().sub(r.rhs.clone());
        if diff.is_ground() && diff.constant == 0 {
            continue;
        }
        if let (Some(lhs), Some(rhs)) = (
            normal_const_to_sv(&r.lhs, func, &defs),
            normal_const_to_sv(&r.rhs, func, &defs),
        ) {
            items.push(SvItem::InitialAssert {
                cond: SvExpr::BinOp(SvBinOp::Eq, Box::new(lhs), Box::new(rhs)),
            });
        }
    }

    let module_name = defs
        .module_names
        .get(&func.def_id)
        .cloned()
        .unwrap_or_else(|| func.name.clone());

    SvModule {
        name: module_name,
        parameters,
        ports,
        items,
    }
}

/// Render a `NormalConst` as an SV expression. `Param(i)` becomes the
/// SV parameter name from the module's generic_params; constants and
/// scaled vars build through `BinOp(Add)` / `BinOp(Mul)`. Returns `None`
/// if any term references a variable that can't be named (e.g. a
/// `ConstVar` that wasn't resolved — shouldn't happen post-finalisation).
fn normal_const_to_sv(nc: &NormalConst, func: &HirFn, defs: &BackendDefs<'_>) -> Option<SvExpr> {
    let mut parts: Vec<SvExpr> = Vec::new();
    for (coeff, var) in &nc.terms {
        let name = match var {
            NormalVar::Param(i) => {
                let local = local_for_generic_param(*i, func, defs)?;
                let info = func.locals.get(local.0 as usize)?;
                SvExpr::Ident(info.name.clone())
            }
            // ConstVar/Local shouldn't appear in a finalised residual.
            NormalVar::ConstVar(_) | NormalVar::Local(_) => return None,
        };
        let term = if *coeff == 1 {
            name
        } else {
            SvExpr::BinOp(
                SvBinOp::Mul,
                Box::new(SvExpr::Lit(coeff.to_string())),
                Box::new(name),
            )
        };
        parts.push(term);
    }
    if nc.constant != 0 || parts.is_empty() {
        parts.push(SvExpr::Lit(nc.constant.to_string()));
    }
    parts
        .into_iter()
        .reduce(|acc, e| SvExpr::BinOp(SvBinOp::Add, Box::new(acc), Box::new(e)))
}

fn lower_block(
    block: &HirBlock,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    clock_names: &HashMap<LocalId, String>,
    local_names: &HashMap<LocalId, String>,
    local_types: &mut HashMap<LocalId, SvType>,
    instance_counts: &mut HashMap<String, u32>,
    items: &mut Vec<SvItem>,
) {
    for stmt in &block.statements {
        lower_stmt(
            stmt,
            func,
            defs,
            clock_names,
            local_names,
            local_types,
            instance_counts,
            items,
        );
    }
}

fn lower_stmt(
    stmt: &HirStmt,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    clock_names: &HashMap<LocalId, String>,
    local_names: &HashMap<LocalId, String>,
    local_types: &mut HashMap<LocalId, SvType>,
    instance_counts: &mut HashMap<String, u32>,
    items: &mut Vec<SvItem>,
) {
    match stmt {
        HirStmt::Let(l) => {
            // Declare the LHS as a logic of the value's type.
            let sv_ty = infer_sv_type(&l.value, func, defs, local_names, local_types);
            local_types.insert(l.local, sv_ty.clone());
            let lhs_name = sv_name(local_names, func, l.local);
            items.push(SvItem::Logic(SvLogicDecl {
                ty: sv_ty,
                name: lhs_name.clone(),
            }));
            lower_assignment_into(
                &l.value,
                &lhs_name,
                func,
                defs,
                clock_names,
                local_names,
                items,
            );
        }
        HirStmt::VarDecl(v) => {
            let sv_ty = if let Some(ty) = &v.ty {
                if let HirTypeKind::Value(vt) = &ty.kind {
                    sv_type_for_value(vt, func, defs, local_names)
                } else {
                    SvType::bit()
                }
            } else {
                SvType::bit()
            };
            local_types.insert(v.local, sv_ty.clone());
            items.push(SvItem::Logic(SvLogicDecl {
                ty: sv_ty,
                name: sv_name(local_names, func, v.local),
            }));
        }
        HirStmt::Equation(eq) => {
            let lhs_name = sv_name(local_names, func, eq.lhs);
            lower_assignment_into(
                &eq.rhs,
                &lhs_name,
                func,
                defs,
                clock_names,
                local_names,
                items,
            );
        }
        HirStmt::Return(e) => {
            // Scalar return → drive `result`.
            lower_assignment_into(e, "result", func, defs, clock_names, local_names, items);
        }
        HirStmt::Expr(e) => {
            // A `HirStmt::Expr` carrying a call to a known user function is a
            // module instance — the `out_args` pre-flatten pass converts
            // every user-fn call into this shape, with the binding(s) as
            // trailing out-arguments. Anything else here is a side-effecting
            // expression we don't support yet (skip).
            if let HirExprKind::Call(call) = &e.kind
                && let Some(callee) = defs.user_fns.get(&call.callee).copied()
            {
                lower_user_instance(
                    call,
                    callee,
                    func,
                    defs,
                    local_names,
                    instance_counts,
                    items,
                );
            }
        }
        HirStmt::AlwaysFf(a) => {
            // `always_ff @(posedge clk) dest <= d_input;` — no reset.
            // The dest var should already have a `logic` declaration
            // (emitted by the VarDecl that preceded this in the late
            // lowering pass).
            let clock_name = sv_name(local_names, func, a.clock);
            let dest_name = sv_name(local_names, func, a.dest);
            let d_input = lower_expr(&a.d_input, func, defs, local_names);
            items.push(SvItem::AlwaysFf(SvAlwaysFf {
                clock: clock_name,
                reset: None,
                reset_body: Vec::new(),
                clocked_body: vec![SvSeqAssign {
                    lhs: SvExpr::Ident(dest_name),
                    rhs: d_input,
                }],
            }));
        }
        HirStmt::If(i) => {
            // `if`-statements (introduced by lower_block_expressions for
            // every if-expression used as a value) emit as a single
            // `always_comb begin if (cond) … else … end` block.
            let cond_expr = lower_expr(&i.condition, func, defs, local_names);
            let then_body = lower_comb_branch(&i.then_branch, func, defs, clock_names, local_names);
            let else_body = lower_comb_branch(&i.else_branch, func, defs, clock_names, local_names);
            items.push(SvItem::AlwaysComb(crate::sv_ir::SvAlwaysComb {
                body: vec![crate::sv_ir::SvCombStmt::If(crate::sv_ir::SvCombIf {
                    cond: cond_expr,
                    then_branch: then_body,
                    else_branch: else_body,
                })],
            }));
        }
    }
}

/// Lower one branch of a `HirStmt::If` into a `Vec<SvCombStmt>` for the
/// `always_comb` body. Statements inside the branch are either equations
/// (→ blocking assigns) or nested ifs (→ nested SV ifs). Anything else
/// would be a flatten / out_args artifact and is skipped.
fn lower_comb_branch(
    block: &HirBlock,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    clock_names: &HashMap<LocalId, String>,
    local_names: &HashMap<LocalId, String>,
) -> Vec<crate::sv_ir::SvCombStmt> {
    let mut out = Vec::new();
    for stmt in &block.statements {
        match stmt {
            HirStmt::Equation(eq) => {
                let lhs_name = sv_name(local_names, func, eq.lhs);
                let rhs = lower_expr(&eq.rhs, func, defs, local_names);
                out.push(crate::sv_ir::SvCombStmt::Assign {
                    lhs: SvExpr::Ident(lhs_name),
                    rhs,
                });
            }
            HirStmt::If(inner) => {
                let cond = lower_expr(&inner.condition, func, defs, local_names);
                let then_body =
                    lower_comb_branch(&inner.then_branch, func, defs, clock_names, local_names);
                let else_body =
                    lower_comb_branch(&inner.else_branch, func, defs, clock_names, local_names);
                out.push(crate::sv_ir::SvCombStmt::If(crate::sv_ir::SvCombIf {
                    cond,
                    then_branch: then_body,
                    else_branch: else_body,
                }));
            }
            // Let / VarDecl / Return / Expr shouldn't appear inside the
            // synthesised branches; lower_block_expressions only emits
            // Equations and nested Ifs. Skip if they do.
            _ => {}
        }
    }
    out
}

/// Emit a `SvItem::Instance` for a user-fn call. The callee's flattened
/// param list dictates the port order; the call's args are paired against
/// the params positionally. Inferable args (`#clk` slots) inherit the
/// caller's binding for the same clock.
fn lower_user_instance(
    call: &HirCall,
    callee: &HirFn,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    local_names: &HashMap<LocalId, String>,
    instance_counts: &mut HashMap<String, u32>,
    items: &mut Vec<SvItem>,
) {
    let callee_module_name = defs
        .module_names
        .get(&callee.def_id)
        .cloned()
        .unwrap_or_else(|| callee.name.clone());
    let instance_name = pick_instance_name(&callee_module_name, instance_counts);
    let mut ports: Vec<(String, SvExpr)> = Vec::with_capacity(callee.params.len());
    for (param, arg) in callee.params.iter().zip(call.args.iter()) {
        let port_name = callee
            .locals
            .get(param.local.0 as usize)
            .map(|li| li.name.clone())
            .unwrap_or_else(|| format!("p{}", param.local.0));
        let port_expr = match arg {
            HirArg::Provided { expr, .. } => lower_expr(expr, func, defs, local_names),
            HirArg::Inferable => {
                // An inferable arg is conventionally a `dom clk` — bind it
                // to the caller's clock-typed local of matching type. The
                // first-pass functions have a single clock; the surrounding
                // module's clock port is the destination.
                SvExpr::Ident(caller_clock_name(func, local_names))
            }
        };
        ports.push((port_name, port_expr));
    }
    items.push(SvItem::Instance(SvInstance {
        module: callee_module_name,
        name: instance_name,
        ports,
    }));
}

/// Choose a name for a SV instance. First-pass scheme: `<callee>_<i>`,
/// where `i` counts instances per callee within the enclosing module.
/// TODO: derive the name from the let-binding when possible (`let delay1 =
/// …` should produce `delay1`); requires threading the binding name from
/// `out_args` through `flatten` to here.
fn pick_instance_name(callee_name: &str, instance_counts: &mut HashMap<String, u32>) -> String {
    let n = instance_counts.entry(callee_name.to_owned()).or_insert(0);
    let name = if *n == 0 {
        callee_name.to_owned()
    } else {
        format!("{callee_name}_{n}")
    };
    *n += 1;
    name
}

/// Pick a SV identifier for the caller's clock signal. Today functions have
/// exactly one `Clock`-typed param; that's the only candidate.
fn caller_clock_name(func: &HirFn, local_names: &HashMap<LocalId, String>) -> String {
    for param in &func.params {
        if matches!(param.ty.kind, HirTypeKind::Clock) {
            return sv_name(local_names, func, param.local);
        }
    }
    "clk".to_owned()
}

/// Emit either an `assign` or an `always_ff`, depending on whether `value`
/// is a `.reg(...)` call. `lhs_name` is the SV identifier of the destination.
fn lower_assignment_into(
    value: &HirExpr,
    lhs_name: &str,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    clock_names: &HashMap<LocalId, String>,
    local_names: &HashMap<LocalId, String>,
    items: &mut Vec<SvItem>,
) {
    if let HirExprKind::Call(call) = &value.kind {
        if defs.reg == Some(call.callee) && call.args.len() == 4 {
            // .reg(rstn, reset_val): always_ff block.
            let self_expr = match &call.args[1] {
                HirArg::Provided { expr, .. } => expr,
                _ => return,
            };
            let rstn_expr = match &call.args[2] {
                HirArg::Provided { expr, .. } => expr,
                _ => return,
            };
            let reset_val_expr = match &call.args[3] {
                HirArg::Provided { expr, .. } => expr,
                _ => return,
            };
            let clock = clock_for_reg(rstn_expr, func, clock_names).unwrap_or_else(|| {
                // Fall back to the first clock_names entry — single-clock
                // first-pass functions always have exactly one.
                clock_names
                    .values()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "clk".to_owned())
            });
            let reset = match &rstn_expr.kind {
                HirExprKind::Local(id) => sv_name(local_names, func, *id),
                _ => "rstn".to_owned(),
            };
            items.push(SvItem::AlwaysFf(SvAlwaysFf {
                clock,
                reset: Some(reset),
                reset_body: vec![SvSeqAssign {
                    lhs: SvExpr::Ident(lhs_name.to_owned()),
                    rhs: lower_expr(reset_val_expr, func, defs, local_names),
                }],
                clocked_body: vec![SvSeqAssign {
                    lhs: SvExpr::Ident(lhs_name.to_owned()),
                    rhs: lower_expr(self_expr, func, defs, local_names),
                }],
            }));
            return;
        }
    }

    // Default: continuous assign.
    items.push(SvItem::Assign {
        lhs: SvExpr::Ident(lhs_name.to_owned()),
        rhs: lower_expr(value, func, defs, local_names),
    });
}

/// The clock signal name for a `.reg` call. Derives from `rstn`'s
/// `Reset @clk` domain — the `@clk` is a `Clock`-typed local whose name is
/// the SV clock identifier.
fn clock_for_reg(
    rstn_expr: &HirExpr,
    _func: &HirFn,
    clock_names: &HashMap<LocalId, String>,
) -> Option<String> {
    let ty = rstn_expr.ty.as_ref()?;
    if let HirTypeKind::Value(ValueType {
        domain: Domain::Clock(clk_local),
        ..
    }) = &ty.kind
    {
        clock_names.get(clk_local).cloned()
    } else {
        None
    }
}

// ============================================================================
// Expression lowering
// ============================================================================

fn lower_expr(
    expr: &HirExpr,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    local_names: &HashMap<LocalId, String>,
) -> SvExpr {
    match &expr.kind {
        HirExprKind::Const(c) => match c {
            ConstValue::Integer(n) => SvExpr::Lit(format!("{n}")),
            ConstValue::Bool(b) => SvExpr::Lit(if *b {
                "1'b1".to_owned()
            } else {
                "1'b0".to_owned()
            }),
        },
        HirExprKind::Local(id) => SvExpr::Ident(sv_name(local_names, func, *id)),
        // `Param(i)` references the i-th generic param of the enclosing
        // def. In a parametric module's own body, this becomes the SV
        // parameter's name (translated via `local_for_generic_param`).
        // If the def has no matching generic param, fall back to a
        // placeholder so downstream tooling can flag it.
        HirExprKind::Param(i) => match local_for_generic_param(*i, func, defs) {
            Some(local) => SvExpr::Ident(sv_name(local_names, func, local)),
            None => SvExpr::Ident("__unsubstituted_param__".to_owned()),
        },
        // `ConstVar` reaching here is a typeck residual — the var should
        // have been resolved before SV lowering. Emit a placeholder so
        // downstream tooling can flag it.
        HirExprKind::ConstVar(_) => SvExpr::Ident("__unresolved_const_var__".to_owned()),
        HirExprKind::Call(call) => lower_call(call, func, defs, local_names),
        // After `flatten_aggregates` runs, every field access on a flattened
        // aggregate is rewritten to a `Local`. A `Field` reaching here means
        // flatten didn't cover this shape; emit a placeholder so downstream
        // tooling sees it but doesn't silently produce wrong SV.
        HirExprKind::Field(_) => SvExpr::Ident("UNRESOLVED_FIELD".to_owned()),
        HirExprKind::MethodCall(_) => unreachable!(
            "MethodCall should be lowered to Call by `hir::method_lower` before sv_lower"
        ),
        HirExprKind::Block(_) | HirExprKind::If(_) | HirExprKind::When(_) => {
            unreachable!(
                "Block/If/When should be flattened by lower_block_expressions before sv_lower"
            )
        }
    }
}

fn lower_call(
    call: &HirCall,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    local_names: &HashMap<LocalId, String>,
) -> SvExpr {
    // Binary operators (+, *) — two positional args.
    if defs.add == Some(call.callee) || defs.mul == Some(call.callee) {
        if call.args.len() == 2 {
            let lhs = arg_expr(&call.args[0], func, defs, local_names);
            let rhs = arg_expr(&call.args[1], func, defs, local_names);
            let op = if defs.add == Some(call.callee) {
                SvBinOp::Add
            } else {
                SvBinOp::Mul
            };
            return SvExpr::BinOp(op, Box::new(lhs), Box::new(rhs));
        }
    }
    // `.reg(...)` at expression position is handled at the assignment site;
    // if it reached here, lower the receiver as a fallback so the SV still
    // builds (the always_ff path is the correct route).
    if defs.reg == Some(call.callee) && call.args.len() == 4 {
        if let HirArg::Provided { expr, .. } = &call.args[1] {
            return lower_expr(expr, func, defs, local_names);
        }
    }
    // Unknown call shape — emit a placeholder identifier so the file still
    // parses; downstream passes can flag it.
    SvExpr::Ident("__unknown_call__".to_owned())
}

fn arg_expr(
    arg: &HirArg,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    local_names: &HashMap<LocalId, String>,
) -> SvExpr {
    match arg {
        HirArg::Provided { expr, .. } => lower_expr(expr, func, defs, local_names),
        HirArg::Inferable => SvExpr::Lit("/* inferable */".to_owned()),
    }
}

// ============================================================================
// Type helpers
// ============================================================================

fn sv_type_for_value(
    vt: &ValueType,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    local_names: &HashMap<LocalId, String>,
) -> SvType {
    match &vt.kind {
        ValueKind::Bool | ValueKind::Reset => SvType::bit(),
        ValueKind::Usize => SvType::bit(),
        ValueKind::UInt { width } => {
            let w = lower_width_expr(width, func, defs, local_names);
            SvType::uint(w)
        }
        // `Event` has no runtime representation — `when EVENT { … }`
        // consumes it at lower_block_expressions time, so by sv_lower an
        // Event-typed value should never reach a port/decl. Fall back to
        // 1-bit if one ever slips through.
        ValueKind::Event => SvType::bit(),
        ValueKind::Struct { .. } => {
            // Should not survive flattening; fall back to 1-bit.
            SvType::bit()
        }
        ValueKind::Param(_) | ValueKind::Var(_) => {
            // Should have been substituted out by typeck/flatten; fall
            // back to 1-bit if a placeholder slips through.
            SvType::bit()
        }
    }
}

fn lower_width_expr(
    width: &HirExpr,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    local_names: &HashMap<LocalId, String>,
) -> SvExpr {
    // Width expressions are usize-typed; reuse the standard expr lowering
    // and then peel off any `Lit("1'bX")` form (which only arises for
    // booleans, irrelevant for widths).
    match &width.kind {
        HirExprKind::Const(ConstValue::Integer(n)) => SvExpr::Lit(format!("{n}")),
        HirExprKind::Local(id) => SvExpr::Ident(sv_name(local_names, func, *id)),
        HirExprKind::Call(_) => lower_expr(width, func, defs, local_names),
        HirExprKind::Const(ConstValue::Bool(_)) => SvExpr::Lit("1".to_owned()),
        // `Param(i)` in a parametric module's own widths becomes the SV
        // parameter's identifier (e.g. `uint(N)` in a `param N: usize`
        // module emits `[N-1:0]`).
        HirExprKind::Param(i) => match local_for_generic_param(*i, func, defs) {
            Some(local) => SvExpr::Ident(sv_name(local_names, func, local)),
            None => SvExpr::Lit("0".to_owned()),
        },
        // `ConstVar` should have been resolved by typeck. Fall back to "0"
        // so the file still parses.
        HirExprKind::ConstVar(_) => SvExpr::Lit("0".to_owned()),
        // Widths are `usize`, so field access (a struct/port field result)
        // cannot appear here in well-typed HIR; pick a safe placeholder.
        HirExprKind::Field(_) => SvExpr::Lit("0".to_owned()),
        HirExprKind::MethodCall(_) => SvExpr::Lit("0".to_owned()),
        HirExprKind::Block(_) | HirExprKind::If(_) | HirExprKind::When(_) => {
            SvExpr::Lit("0".to_owned())
        }
    }
}

/// Compute the SV type of an HIR expression, walking known shapes. Returns
/// 1-bit if the type can't be determined; the caller is expected to use this
/// for `logic` declarations where 1-bit is a safe default.
fn infer_sv_type(
    expr: &HirExpr,
    func: &HirFn,
    defs: &BackendDefs<'_>,
    local_names: &HashMap<LocalId, String>,
    local_types: &HashMap<LocalId, SvType>,
) -> SvType {
    if let Some(ty) = &expr.ty {
        if let HirTypeKind::Value(vt) = &ty.kind {
            return sv_type_for_value(vt, func, defs, local_names);
        }
    }
    match &expr.kind {
        HirExprKind::Const(ConstValue::Bool(_)) => SvType::bit(),
        HirExprKind::Const(ConstValue::Integer(_)) => SvType::bit(),
        HirExprKind::Param(_) | HirExprKind::ConstVar(_) => SvType::bit(),
        HirExprKind::Local(id) => local_types.get(id).cloned().unwrap_or_else(SvType::bit),
        HirExprKind::Call(call) => {
            // Binary op result has the same type as either operand.
            if call.args.len() >= 2 {
                if let HirArg::Provided { expr, .. } = &call.args[0] {
                    return infer_sv_type(expr, func, defs, local_names, local_types);
                }
            }
            // `.reg(...)` — self is at slot 1.
            if call.args.len() == 4 {
                if let HirArg::Provided { expr, .. } = &call.args[1] {
                    return infer_sv_type(expr, func, defs, local_names, local_types);
                }
            }
            SvType::bit()
        }
        // Field access after flatten should have been rewritten to a Local.
        // If one slips through, fall back to a bit-wide type.
        HirExprKind::Field(_) => SvType::bit(),
        HirExprKind::MethodCall(_) => SvType::bit(),
        HirExprKind::Block(_) | HirExprKind::If(_) | HirExprKind::When(_) => SvType::bit(),
    }
}

fn local_name(func: &HirFn, id: LocalId) -> &str {
    func.locals
        .get(id.0 as usize)
        .map(|l| l.name.as_str())
        .unwrap_or("__unknown_local__")
}

/// Look up the SV-side identifier for a `LocalId`, falling back to the raw
/// HIR name if the map has no entry (shouldn't happen in practice).
fn sv_name(local_names: &HashMap<LocalId, String>, func: &HirFn, id: LocalId) -> String {
    local_names
        .get(&id)
        .cloned()
        .unwrap_or_else(|| local_name(func, id).to_owned())
}

/// Build a `LocalId → unique SV identifier` map for a function. Polar
/// allows `let` shadowing, so multiple `HirLocalInfo` entries can share a
/// `name`. SV requires identifiers be unique within a module, so we keep
/// the first occurrence's name and suffix later ones with `_1`, `_2`, ….
/// Order follows the `func.locals` index (source order).
fn build_local_name_map(func: &HirFn) -> HashMap<LocalId, String> {
    let mut used: HashMap<String, u32> = HashMap::new();
    let mut names: HashMap<LocalId, String> = HashMap::new();
    for (i, info) in func.locals.iter().enumerate() {
        let local = LocalId(i as u32);
        let base = info.name.clone();
        let chosen = match used.get_mut(&base) {
            None => {
                used.insert(base.clone(), 0);
                base
            }
            Some(count) => {
                *count += 1;
                format!("{base}_{count}")
            }
        };
        names.insert(local, chosen);
    }
    names
}

// `HirId` import is used in the function signatures above (via
// `expr_types: HashMap<HirId, _>` consumers); silence unused-import warnings
// in module-level via the explicit use list above.
#[allow(dead_code)]
fn _hir_id_marker(_: HirId) {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{flatten_aggregates, lower_to_hir};
    use crate::resolve::resolve_file;
    use crate::surface_ir::parse_surface_source;
    use crate::typeck;

    fn lower(src: &str) -> SvFile {
        let surface = parse_surface_source(src).expect("parse");
        let resolve = resolve_file(&surface);
        assert!(resolve.errors.is_empty(), "resolve: {:?}", resolve.errors);
        let hir = lower_to_hir(&surface, &resolve).expect("lower");
        let tc = typeck::check_file(&hir, &resolve);
        assert!(tc.errors.is_empty(), "typeck: {:?}", tc.errors);
        let block_lowered = crate::hir::lower_block_expressions::lower_block_expressions(
            &hir,
            &tc.expr_types,
            &tc.local_types,
        );
        let hir = block_lowered.file;
        let local_types = block_lowered.local_types;
        let hir = crate::hir::lower_method_calls(&hir, &resolve, &tc.method_resolutions);
        let hir = crate::hir::desugar_user_calls(&hir).expect("desugar");
        let flat =
            flatten_aggregates(&hir, &resolve, &tc.expr_types, &local_types).expect("flatten");
        lower_to_sv(&flat, &resolve, &tc.fn_residuals)
    }

    #[test]
    fn lowers_accumulator() {
        let sv = lower(include_str!("../../../examples/working/accumulator.plr"));
        assert_eq!(sv.modules.len(), 1);
        let m = &sv.modules[0];
        assert_eq!(m.name, "accumulator");
        // Ports: clk, rstn, data, result.
        let names: Vec<&str> = m.ports.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"clk"), "ports={names:?}");
        assert!(names.contains(&"rstn"), "ports={names:?}");
        assert!(names.contains(&"data"), "ports={names:?}");
        assert!(names.contains(&"result"), "ports={names:?}");
        // At least one always_ff block.
        assert!(
            m.items.iter().any(|i| matches!(i, SvItem::AlwaysFf(_))),
            "expected always_ff"
        );
    }

    #[test]
    fn lowers_counter_with_parameter() {
        let sv = lower(include_str!("../../../examples/working/counter.plr"));
        let m = &sv.modules[0];
        assert_eq!(m.parameters.len(), 1, "{:?}", m.parameters);
        assert_eq!(m.parameters[0].name, "bits");
    }

    #[test]
    fn lowers_packet_struct_with_two_always_ff() {
        let sv = lower(include_str!("../../../examples/working/packet_struct.plr"));
        let m = &sv.modules[0];
        let always_ff: usize = m
            .items
            .iter()
            .filter(|i| matches!(i, SvItem::AlwaysFf(_)))
            .count();
        assert_eq!(always_ff, 2);
        // Result ports.
        let names: Vec<&str> = m.ports.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"result__valid"));
        assert!(names.contains(&"result__payload"));
    }

    #[test]
    fn lowers_working_examples() {
        for (_name, source) in crate::test_support::working_examples() {
            let _sv = lower(&source);
        }
    }
}
