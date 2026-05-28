//! Flattened HIR → SV IR.
//!
//! Per `planning/system_verilog_backend.md`, this pass walks every `HirFn` in
//! a flattened HIR file and builds one `SvModule` per function. The transform
//! is structural: there is no analysis here, just shape mapping.
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
    HirSourceFile, HirStmt, HirTypeKind, LocalId, ValueKind, ValueType,
};
use crate::resolve::{DefId, ResolveResult};
use crate::surface_ir::Direction;
use crate::sv_ir::{
    SvAlwaysFf, SvBinOp, SvExpr, SvFile, SvItem, SvLogicDecl, SvModule, SvParameter, SvPort,
    SvPortDirection, SvSeqAssign, SvType,
};

/// Lower a flattened HIR file to SV IR. `resolve` is used only to identify
/// prelude defs (`reg`, `+`, `*`).
pub fn lower_to_sv(file: &HirSourceFile, resolve: &ResolveResult) -> SvFile {
    let defs = BackendDefs {
        reg: resolve.def_id("reg"),
        add: resolve.def_id("+"),
        mul: resolve.def_id("*"),
    };
    let mut modules = Vec::new();
    for item in &file.items {
        if let HirItem::Fn(func) = item {
            modules.push(lower_fn(func, &defs));
        }
    }
    SvFile { modules }
}

#[derive(Debug, Clone, Copy)]
struct BackendDefs {
    reg: Option<DefId>,
    add: Option<DefId>,
    mul: Option<DefId>,
}

// ============================================================================
// Per-function lowering
// ============================================================================

fn lower_fn(func: &HirFn, defs: &BackendDefs) -> SvModule {
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
                if param.is_const {
                    // const param → SV parameter, no port.
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
                // pass total. Real code should have fully resolved types.
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
    lower_block(
        &func.body,
        func,
        defs,
        &clock_names,
        &local_names,
        &mut local_types,
        &mut items,
    );

    SvModule {
        name: func.name.clone(),
        parameters,
        ports,
        items,
    }
}

fn lower_block(
    block: &HirBlock,
    func: &HirFn,
    defs: &BackendDefs,
    clock_names: &HashMap<LocalId, String>,
    local_names: &HashMap<LocalId, String>,
    local_types: &mut HashMap<LocalId, SvType>,
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
            items,
        );
    }
}

fn lower_stmt(
    stmt: &HirStmt,
    func: &HirFn,
    defs: &BackendDefs,
    clock_names: &HashMap<LocalId, String>,
    local_names: &HashMap<LocalId, String>,
    local_types: &mut HashMap<LocalId, SvType>,
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
        HirStmt::Expr(_) => {
            // Side-effecting expressions are not in first-pass scope. Skip.
        }
    }
}

/// Emit either an `assign` or an `always_ff`, depending on whether `value`
/// is a `.reg(...)` call. `lhs_name` is the SV identifier of the destination.
fn lower_assignment_into(
    value: &HirExpr,
    lhs_name: &str,
    func: &HirFn,
    defs: &BackendDefs,
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
                reset,
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
    defs: &BackendDefs,
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
        HirExprKind::Call(call) => lower_call(call, func, defs, local_names),
    }
}

fn lower_call(
    call: &HirCall,
    func: &HirFn,
    defs: &BackendDefs,
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
    defs: &BackendDefs,
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
    defs: &BackendDefs,
    local_names: &HashMap<LocalId, String>,
) -> SvType {
    match &vt.kind {
        ValueKind::Bool | ValueKind::Reset => SvType::bit(),
        ValueKind::Usize => SvType::bit(),
        ValueKind::UInt { width } => {
            let w = lower_width_expr(width, func, defs, local_names);
            SvType::uint(w)
        }
        ValueKind::Struct { .. } => {
            // Should not survive flattening; fall back to 1-bit.
            SvType::bit()
        }
    }
}

fn lower_width_expr(
    width: &HirExpr,
    func: &HirFn,
    defs: &BackendDefs,
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
    }
}

/// Compute the SV type of an HIR expression, walking known shapes. Returns
/// 1-bit if the type can't be determined; the caller is expected to use this
/// for `logic` declarations where 1-bit is a safe default.
fn infer_sv_type(
    expr: &HirExpr,
    func: &HirFn,
    defs: &BackendDefs,
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
        let flat = flatten_aggregates(&hir, &tc.expr_types).expect("flatten");
        lower_to_sv(&flat, &resolve)
    }

    #[test]
    fn lowers_accumulator() {
        let sv = lower(include_str!("../../../examples/accumulator.plr"));
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
        let sv = lower(include_str!("../../../examples/counter.plr"));
        let m = &sv.modules[0];
        assert_eq!(m.parameters.len(), 1, "{:?}", m.parameters);
        assert_eq!(m.parameters[0].name, "bits");
    }

    #[test]
    fn lowers_simple_port_with_only_assigns() {
        let sv = lower(include_str!("../../../examples/simple_port.plr"));
        let m = &sv.modules[0];
        // Three assigns from the flattened port equation.
        let assigns: usize = m
            .items
            .iter()
            .filter(|i| matches!(i, SvItem::Assign { .. }))
            .count();
        assert_eq!(assigns, 3);
        // No always_ff — pure combinational.
        assert!(m.items.iter().all(|i| !matches!(i, SvItem::AlwaysFf(_))));
    }

    #[test]
    fn lowers_packet_struct_with_two_always_ff() {
        let sv = lower(include_str!("../../../examples/packet_struct.plr"));
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
    fn lowers_all_examples() {
        let examples = [
            include_str!("../../../examples/accumulator.plr"),
            include_str!("../../../examples/add_constant.plr"),
            include_str!("../../../examples/counter.plr"),
            include_str!("../../../examples/mult_add.plr"),
            include_str!("../../../examples/packet_struct.plr"),
            include_str!("../../../examples/pipeline.plr"),
            include_str!("../../../examples/shift_register.plr"),
            include_str!("../../../examples/simple_port.plr"),
        ];
        for src in examples {
            let _sv = lower(src);
        }
    }
}
