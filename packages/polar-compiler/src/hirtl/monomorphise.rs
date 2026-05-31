//! Monomorphise Type-kind generic functions.
//!
//! Walks every call expression in the HIR. For each call whose callee
//! has at least one Type-kind generic parameter, the pass synthesises a
//! specialised `HirFn` with the Type-kind args substituted out. Other
//! generic kinds (Const for widths, Domain for clocks) remain
//! polymorphic in the specialisation — only Type is monomorphised,
//! matching the "minimal way" requested.
//!
//! Pipeline placement: between `typeck` and `lower_block_expressions`.
//! The pass consumes typeck's `call_generics` side table (the per-call
//! `GenericArgs`) and produces an augmented HIR file plus updated side
//! tables. Original Type-kind-generic fns stay in the output but are
//! skipped by `sv_lower::has_type_generic`.
//!
//! Body cloning: the specialised fn gets a fresh set of `HirId`s and
//! `LocalId`s. The pass clones every reachable expression/statement
//! and rewrites the contained ids through a `BodyRemap`. Side-table
//! entries (`expr_types`, `local_types`, `method_resolutions`) are
//! cloned with the new ids and types substituted.

use std::collections::HashMap;

use crate::hir::{
    ConstValue, Domain, GenericArg, GenericArgs, HirAlwaysFfStmt, HirArg, HirArgSource, HirBlock,
    HirBlockExpr, HirCall, HirEquation, HirExpr, HirExprKind, HirFieldAccess, HirFn, HirId,
    HirIfExpr, HirIfStmt, HirItem, HirLet, HirMethodCall, HirParam, HirSourceFile, HirStmt,
    HirType, HirTypeKind, HirVarDecl, HirWhenExpr, LocalId, PortTypeRef, ValueKind, ValueType,
};
use crate::hirt::typeck::FnResidual;
use crate::resolve::{DefId, DefInfo, GenericParamInfo, GenericParamKind, ResolveResult};

/// Output of the monomorphise pass.
pub struct MonomorphResult {
    pub file: HirSourceFile,
    pub expr_types: HashMap<HirId, HirType>,
    pub local_types: HashMap<LocalId, HirType>,
    pub method_resolutions: HashMap<HirId, DefId>,
    pub fn_residuals: HashMap<DefId, Vec<FnResidual>>,
}

/// Specialise Type-kind generic fns based on `call_generics`. `resolve`
/// is taken mutably so the pass can register new `DefInfo` entries for
/// each specialised fn.
pub fn monomorphise(
    file: HirSourceFile,
    mut expr_types: HashMap<HirId, HirType>,
    mut local_types: HashMap<LocalId, HirType>,
    mut method_resolutions: HashMap<HirId, DefId>,
    mut fn_residuals: HashMap<DefId, Vec<FnResidual>>,
    call_generics: &HashMap<HirId, GenericArgs>,
    resolve: &mut ResolveResult,
) -> MonomorphResult {
    let mut fns_by_def: HashMap<DefId, HirFn> = HashMap::new();
    for item in &file.items {
        if let HirItem::Fn(f) = item {
            fns_by_def.insert(f.def_id, f.clone());
        }
    }

    // 1. Find instantiations needed.
    //    For each call whose callee has Type-kind generic params, record
    //    (callee_def_id, type_args). The type_args here are the FULL
    //    GenericArgs (including Const/Domain); the spec key uses only the
    //    Type-kind slots because Const/Domain stay polymorphic.
    let mut spec_map: HashMap<(DefId, SpecKey), DefId> = HashMap::new();
    let mut specs_to_build: Vec<(DefId, DefId, GenericArgs)> = Vec::new();
    let mut next_def = next_def_id(resolve);

    let mut call_ids_to_rewrite: Vec<(HirId, DefId)> = Vec::new();

    let mut max_hir_id = max_hir_id_in_file(&file);
    let mut max_local_id = max_local_id_across_fns(&fns_by_def);

    for (call_id, args) in call_generics {
        // The call's callee def is recorded on the HirCall but we don't
        // have direct access here. Look it up by walking the file later;
        // for now defer the per-call resolution.
        let Some(callee_def) = call_callee(&file, *call_id) else {
            continue;
        };
        let Some(callee_fn) = fns_by_def.get(&callee_def) else {
            continue;
        };
        let info = resolve.def_info(callee_fn.def_id);
        if !info
            .generic_params
            .iter()
            .any(|gp| matches!(gp.kind, GenericParamKind::Type))
        {
            continue;
        }
        let key = spec_key_for(&info.generic_params, args, resolve);
        let spec_def = *spec_map
            .entry((callee_def, key.clone()))
            .or_insert_with(|| {
                let id = next_def;
                next_def = DefId(next_def.0 + 1);
                specs_to_build.push((callee_def, id, args.clone()));
                id
            });
        call_ids_to_rewrite.push((*call_id, spec_def));
    }

    // 2. For each spec, register its DefInfo and clone the body.
    let mut new_items: Vec<HirItem> = Vec::new();
    for (orig_def, spec_def, args) in &specs_to_build {
        let orig_fn = fns_by_def
            .get(orig_def)
            .expect("callee was looked up above");
        let orig_info = resolve.def_info(*orig_def);
        let orig_name = orig_info.name.clone();
        let orig_generic_params = orig_info.generic_params.clone();
        let orig_kind = orig_info.kind;
        let orig_span = orig_info.span.clone();
        let meta = build_spec_metadata(&orig_name, &orig_generic_params, args, resolve);
        // Register the spec in `resolve.defs`. Subsequent passes look up
        // `def_info(spec_def)` and expect a real entry there.
        push_def_info(
            resolve,
            *spec_def,
            DefInfo {
                kind: orig_kind,
                name: meta.name.clone(),
                span: orig_span,
                generic_params: meta.generic_params,
            },
        );

        // Clone the original fn body with fresh HirIds and LocalIds.
        let mut remap = BodyRemap::new(&meta.type_subst);
        let cloned = clone_fn(
            orig_fn,
            *spec_def,
            meta.name,
            &meta.dropped_param_names,
            &mut remap,
            &mut max_hir_id,
            &mut max_local_id,
        );

        // Transfer remapped side-table entries.
        for (old_id, new_id) in &remap.hir_id_map {
            if let Some(ty) = expr_types.get(old_id) {
                let new_ty = substitute_type(ty, &meta.type_subst);
                expr_types.insert(*new_id, new_ty);
            }
            if let Some(def) = method_resolutions.get(old_id) {
                method_resolutions.insert(*new_id, *def);
            }
        }
        for (old_local, new_local) in &remap.local_map {
            if let Some(ty) = local_types.get(old_local) {
                let new_ty = substitute_type(ty, &meta.type_subst);
                local_types.insert(*new_local, new_ty);
            }
        }
        // Propagate residual constraints (Param refs in NormalConst stay
        // as-is — the spec still has its remaining generic params).
        if let Some(residuals) = fn_residuals.get(orig_def) {
            fn_residuals.insert(*spec_def, residuals.clone());
        }

        new_items.push(HirItem::Fn(cloned));
    }

    // 3. Rewrite call sites to point at the specialised DefIds, and
    //    drop the arg slots whose params got substituted out.
    let rewrite_map: HashMap<HirId, (DefId, Vec<usize>)> = call_ids_to_rewrite
        .into_iter()
        .map(|(call_id, spec_def)| {
            // Compute the arg indices to drop: a HirParam is dropped
            // when its name matches a Type-kind generic param. Build
            // the list from the original callee.
            let drop_idxs = dropped_arg_indices(
                &fns_by_def,
                &spec_def_to_orig(&specs_to_build),
                spec_def,
                resolve,
            );
            (call_id, (spec_def, drop_idxs))
        })
        .collect();
    let mut items = file.items;
    for item in items.iter_mut() {
        if let HirItem::Fn(f) = item {
            rewrite_calls_in_block(&mut f.body, &rewrite_map);
        }
    }
    items.extend(new_items);

    MonomorphResult {
        file: HirSourceFile {
            items,
            span: file.span,
        },
        expr_types,
        local_types,
        method_resolutions,
        fn_residuals,
    }
}

/// A spec key — the mangled string of Type-kind args in slot order.
/// Other arg kinds don't influence the spec identity because Const /
/// Domain stay polymorphic in the spec.
type SpecKey = String;

fn spec_key_for(
    params: &[GenericParamInfo],
    args: &GenericArgs,
    resolve: &ResolveResult,
) -> SpecKey {
    let mut s = String::new();
    for (i, gp) in params.iter().enumerate() {
        if !matches!(gp.kind, GenericParamKind::Type) {
            continue;
        }
        if let Some(GenericArg::Type(t)) = args.0.get(i) {
            s.push('|');
            s.push_str(&mangle_type(t, resolve));
        }
    }
    s
}

/// For each generic param of the original fn, decide whether to keep it
/// in the spec or substitute it out. Returns (mangled name, spec
/// generic_params, type_subst) where type_subst maps original Param
/// index → HirType to substitute. Const/Domain params stay in the spec
/// (with their original indices preserved by inserting holes).
struct SpecMetadata {
    name: String,
    generic_params: Vec<GenericParamInfo>,
    type_subst: HashMap<u32, HirType>,
    /// Param names that the spec drops from its HirParam list because
    /// they correspond to substituted Type-kind generics. The runtime
    /// shouldn't see them as arguments anymore.
    dropped_param_names: Vec<String>,
}

fn build_spec_metadata(
    orig_name: &str,
    orig_params: &[GenericParamInfo],
    args: &GenericArgs,
    resolve: &ResolveResult,
) -> SpecMetadata {
    let mut spec_params: Vec<GenericParamInfo> = Vec::new();
    let mut type_subst: HashMap<u32, HirType> = HashMap::new();
    let mut name_suffix = String::new();
    let mut dropped: Vec<String> = Vec::new();
    for (i, gp) in orig_params.iter().enumerate() {
        if matches!(gp.kind, GenericParamKind::Type) {
            if let Some(GenericArg::Type(t)) = args.0.get(i) {
                type_subst.insert(i as u32, t.clone());
                name_suffix.push_str("__");
                name_suffix.push_str(&mangle_type(t, resolve));
                dropped.push(gp.name.clone());
            }
        } else {
            spec_params.push(gp.clone());
        }
    }
    SpecMetadata {
        name: format!("{orig_name}{name_suffix}"),
        generic_params: spec_params,
        type_subst,
        dropped_param_names: dropped,
    }
}

/// Render an `HirType` to a name-safe fragment for module-name mangling.
/// `resolve` provides access to def names for struct/port types — passed
/// in by `build_spec_metadata`'s caller.
fn mangle_type(ty: &HirType, resolve: &ResolveResult) -> String {
    match &ty.kind {
        HirTypeKind::Value(vt) => match &vt.kind {
            ValueKind::Bool => "bool".to_owned(),
            ValueKind::Reset => "Reset".to_owned(),
            ValueKind::Usize => "usize".to_owned(),
            ValueKind::Event => "Event".to_owned(),
            ValueKind::UInt { width } => match &width.kind {
                HirExprKind::Const(ConstValue::Integer(n)) => format!("uint{n}"),
                _ => "uint".to_owned(),
            },
            ValueKind::Struct { def, .. } => resolve.def_info(*def).name.clone(),
            ValueKind::Param(i) => format!("P{i}"),
            ValueKind::Var(i) => format!("V{i}"),
        },
        HirTypeKind::Port(p) => resolve.def_info(p.def).name.clone(),
        HirTypeKind::Clock => "Clock".to_owned(),
        HirTypeKind::Var(v) => format!("V{}", v.0),
    }
}

// ---- DefId helpers ----

fn next_def_id(resolve: &ResolveResult) -> DefId {
    DefId(resolve.defs.len() as u32)
}

fn push_def_info(resolve: &mut ResolveResult, expected: DefId, info: DefInfo) {
    // The caller allocated `expected` from `next_def_id`. Verify we're
    // still in sync; if not, pad to keep the index valid (shouldn't
    // happen in practice).
    while resolve.defs.len() < expected.0 as usize {
        resolve.defs.push(DefInfo {
            kind: info.kind,
            name: String::new(),
            span: info.span.clone(),
            generic_params: Vec::new(),
        });
    }
    if resolve.defs.len() == expected.0 as usize {
        resolve.defs.push(info);
    } else {
        resolve.defs[expected.0 as usize] = info;
    }
}

// ---- File-level helpers ----

fn max_hir_id_in_file(file: &HirSourceFile) -> u32 {
    let mut max = 0u32;
    for item in &file.items {
        if let HirItem::Fn(f) = item {
            scan_max_hir_id_block(&f.body, &mut max);
            for p in &f.params {
                if let Some(d) = &p.default {
                    scan_max_hir_id_expr(d, &mut max);
                }
            }
        }
    }
    max
}

fn scan_max_hir_id_block(block: &HirBlock, max: &mut u32) {
    for stmt in &block.statements {
        scan_max_hir_id_stmt(stmt, max);
    }
}

fn scan_max_hir_id_stmt(stmt: &HirStmt, max: &mut u32) {
    match stmt {
        HirStmt::Let(l) => scan_max_hir_id_expr(&l.value, max),
        HirStmt::VarDecl(_) => {}
        HirStmt::Equation(eq) => scan_max_hir_id_expr(&eq.rhs, max),
        HirStmt::Return(e) | HirStmt::Expr(e) => scan_max_hir_id_expr(e, max),
        HirStmt::If(i) => {
            scan_max_hir_id_expr(&i.condition, max);
            scan_max_hir_id_block(&i.then_branch, max);
            scan_max_hir_id_block(&i.else_branch, max);
        }
        HirStmt::AlwaysFf(a) => scan_max_hir_id_expr(&a.d_input, max),
    }
}

fn scan_max_hir_id_expr(expr: &HirExpr, max: &mut u32) {
    if expr.id.0 != u32::MAX && expr.id.0 > *max {
        *max = expr.id.0;
    }
    match &expr.kind {
        HirExprKind::Const(_)
        | HirExprKind::Local(_)
        | HirExprKind::Param(_)
        | HirExprKind::ConstVar(_) => {}
        HirExprKind::Call(c) => {
            for arg in &c.args {
                if let HirArg::Provided { expr, .. } = arg {
                    scan_max_hir_id_expr(expr, max);
                }
            }
        }
        HirExprKind::Field(f) => scan_max_hir_id_expr(&f.receiver, max),
        HirExprKind::MethodCall(mc) => {
            scan_max_hir_id_expr(&mc.receiver, max);
            for arg in &mc.args {
                if let HirArg::Provided { expr, .. } = arg {
                    scan_max_hir_id_expr(expr, max);
                }
            }
        }
        HirExprKind::Block(b) => {
            scan_max_hir_id_block(&b.block, max);
            if let Some(t) = &b.tail {
                scan_max_hir_id_expr(t, max);
            }
        }
        HirExprKind::If(ie) => {
            scan_max_hir_id_expr(&ie.condition, max);
            scan_max_hir_id_block(&ie.then_branch.block, max);
            if let Some(t) = &ie.then_branch.tail {
                scan_max_hir_id_expr(t, max);
            }
            scan_max_hir_id_block(&ie.else_branch.block, max);
            if let Some(t) = &ie.else_branch.tail {
                scan_max_hir_id_expr(t, max);
            }
        }
        HirExprKind::When(we) => {
            scan_max_hir_id_expr(&we.event, max);
            scan_max_hir_id_block(&we.body.block, max);
            if let Some(t) = &we.body.tail {
                scan_max_hir_id_expr(t, max);
            }
        }
    }
}

fn max_local_id_across_fns(fns: &HashMap<DefId, HirFn>) -> u32 {
    let mut max = 0u32;
    for f in fns.values() {
        for p in &f.params {
            if p.local.0 > max {
                max = p.local.0;
            }
        }
        for (i, _) in f.locals.iter().enumerate() {
            if i as u32 > max {
                max = i as u32;
            }
        }
    }
    max
}

/// Find the callee `DefId` for a call expression by walking the file
/// for a call node with matching `HirId`. O(N) per call — fine for the
/// modest sizes we have today; can be cached if it becomes hot.
fn call_callee(file: &HirSourceFile, target: HirId) -> Option<DefId> {
    for item in &file.items {
        if let HirItem::Fn(f) = item {
            if let Some(d) = find_call_callee_in_block(&f.body, target) {
                return Some(d);
            }
        }
    }
    None
}

fn find_call_callee_in_block(block: &HirBlock, target: HirId) -> Option<DefId> {
    for stmt in &block.statements {
        if let Some(d) = find_call_callee_in_stmt(stmt, target) {
            return Some(d);
        }
    }
    None
}

fn find_call_callee_in_stmt(stmt: &HirStmt, target: HirId) -> Option<DefId> {
    match stmt {
        HirStmt::Let(l) => find_call_callee_in_expr(&l.value, target),
        HirStmt::VarDecl(_) => None,
        HirStmt::Equation(eq) => find_call_callee_in_expr(&eq.rhs, target),
        HirStmt::Return(e) | HirStmt::Expr(e) => find_call_callee_in_expr(e, target),
        HirStmt::If(i) => find_call_callee_in_expr(&i.condition, target)
            .or_else(|| find_call_callee_in_block(&i.then_branch, target))
            .or_else(|| find_call_callee_in_block(&i.else_branch, target)),
        HirStmt::AlwaysFf(a) => find_call_callee_in_expr(&a.d_input, target),
    }
}

fn find_call_callee_in_expr(expr: &HirExpr, target: HirId) -> Option<DefId> {
    if expr.id == target {
        if let HirExprKind::Call(c) = &expr.kind {
            return Some(c.callee);
        }
    }
    match &expr.kind {
        HirExprKind::Call(c) => {
            for arg in &c.args {
                if let HirArg::Provided { expr, .. } = arg {
                    if let Some(d) = find_call_callee_in_expr(expr, target) {
                        return Some(d);
                    }
                }
            }
            None
        }
        HirExprKind::Field(f) => find_call_callee_in_expr(&f.receiver, target),
        HirExprKind::MethodCall(mc) => {
            if let Some(d) = find_call_callee_in_expr(&mc.receiver, target) {
                return Some(d);
            }
            for arg in &mc.args {
                if let HirArg::Provided { expr, .. } = arg {
                    if let Some(d) = find_call_callee_in_expr(expr, target) {
                        return Some(d);
                    }
                }
            }
            None
        }
        HirExprKind::Block(b) => find_call_callee_in_block(&b.block, target).or_else(|| {
            b.tail
                .as_ref()
                .and_then(|t| find_call_callee_in_expr(t, target))
        }),
        HirExprKind::If(ie) => find_call_callee_in_expr(&ie.condition, target)
            .or_else(|| find_call_callee_in_block(&ie.then_branch.block, target))
            .or_else(|| {
                ie.then_branch
                    .tail
                    .as_ref()
                    .and_then(|t| find_call_callee_in_expr(t, target))
            })
            .or_else(|| find_call_callee_in_block(&ie.else_branch.block, target))
            .or_else(|| {
                ie.else_branch
                    .tail
                    .as_ref()
                    .and_then(|t| find_call_callee_in_expr(t, target))
            }),
        HirExprKind::When(we) => find_call_callee_in_expr(&we.event, target)
            .or_else(|| find_call_callee_in_block(&we.body.block, target))
            .or_else(|| {
                we.body
                    .tail
                    .as_ref()
                    .and_then(|t| find_call_callee_in_expr(t, target))
            }),
        _ => None,
    }
}

// ---- Body cloning ----

struct BodyRemap<'a> {
    hir_id_map: HashMap<HirId, HirId>,
    local_map: HashMap<LocalId, LocalId>,
    type_subst: &'a HashMap<u32, HirType>,
}

impl<'a> BodyRemap<'a> {
    fn new(type_subst: &'a HashMap<u32, HirType>) -> Self {
        Self {
            hir_id_map: HashMap::new(),
            local_map: HashMap::new(),
            type_subst,
        }
    }

    fn remap_hir_id(&mut self, old: HirId, next: &mut u32) -> HirId {
        if old.0 == u32::MAX {
            return old;
        }
        if let Some(new) = self.hir_id_map.get(&old) {
            return *new;
        }
        *next += 1;
        let new = HirId(*next);
        self.hir_id_map.insert(old, new);
        new
    }

    /// LocalIds are per-fn — the spec reuses the original's LocalIds
    /// directly. The `next` counter is kept in the signature so a
    /// future revision can switch to fresh allocation if a per-fn
    /// local-table refactor lands.
    fn remap_local(&mut self, old: LocalId, _next: &mut u32) -> LocalId {
        self.local_map.insert(old, old);
        old
    }
}

fn clone_fn(
    orig: &HirFn,
    spec_def: DefId,
    spec_name: String,
    drop_param_names: &[String],
    remap: &mut BodyRemap,
    next_hir_id: &mut u32,
    next_local: &mut u32,
) -> HirFn {
    // LocalIds are per-fn (indices into `HirFn.locals`), so the spec
    // reuses the original's LocalIds directly. No remap needed for
    // locals — only HirIds (which are file-global) get fresh values.
    let _ = next_local; // unused; kept in signature for symmetry.
    let locals = orig.locals.clone();
    // Drop HirParams that correspond to substituted Type-kind generic
    // params: their value is now compile-time-known, so they shouldn't
    // appear in the spec's runtime arg list. Match by name against the
    // original locals table.
    let is_dropped = |local: LocalId| -> bool {
        locals
            .get(local.0 as usize)
            .map(|info| drop_param_names.iter().any(|n| n == &info.name))
            .unwrap_or(false)
    };
    let params: Vec<HirParam> = orig
        .params
        .iter()
        .filter(|p| !is_dropped(p.local))
        .map(|p| HirParam {
            local: p.local,
            section: p.section,
            kind: p.kind,
            direction: p.direction,
            ty: substitute_type(&p.ty, remap.type_subst),
            default: p
                .default
                .as_ref()
                .map(|e| clone_expr(e, remap, next_hir_id, &mut 0)),
            span: p.span.clone(),
        })
        .collect();
    let return_type = orig
        .return_type
        .as_ref()
        .map(|t| substitute_type(t, remap.type_subst));
    let body = clone_block(&orig.body, remap, next_hir_id, &mut 0);
    HirFn {
        def_id: spec_def,
        name: spec_name,
        params,
        return_type,
        locals,
        body,
        span: orig.span.clone(),
        is_prelude: orig.is_prelude,
    }
}

fn clone_block(
    block: &HirBlock,
    remap: &mut BodyRemap,
    next_hir_id: &mut u32,
    next_local: &mut u32,
) -> HirBlock {
    HirBlock {
        statements: block
            .statements
            .iter()
            .map(|s| clone_stmt(s, remap, next_hir_id, next_local))
            .collect(),
        span: block.span.clone(),
    }
}

fn clone_stmt(
    stmt: &HirStmt,
    remap: &mut BodyRemap,
    next_hir_id: &mut u32,
    next_local: &mut u32,
) -> HirStmt {
    match stmt {
        HirStmt::Let(l) => HirStmt::Let(HirLet {
            local: remap.remap_local(l.local, next_local),
            value: clone_expr(&l.value, remap, next_hir_id, next_local),
            span: l.span.clone(),
        }),
        HirStmt::VarDecl(v) => HirStmt::VarDecl(HirVarDecl {
            local: remap.remap_local(v.local, next_local),
            ty: v.ty.as_ref().map(|t| substitute_type(t, remap.type_subst)),
            span: v.span.clone(),
        }),
        HirStmt::Equation(eq) => HirStmt::Equation(HirEquation {
            lhs: remap.remap_local(eq.lhs, next_local),
            rhs: clone_expr(&eq.rhs, remap, next_hir_id, next_local),
            span: eq.span.clone(),
        }),
        HirStmt::Return(e) => HirStmt::Return(clone_expr(e, remap, next_hir_id, next_local)),
        HirStmt::Expr(e) => HirStmt::Expr(clone_expr(e, remap, next_hir_id, next_local)),
        HirStmt::If(i) => HirStmt::If(HirIfStmt {
            condition: clone_expr(&i.condition, remap, next_hir_id, next_local),
            then_branch: clone_block(&i.then_branch, remap, next_hir_id, next_local),
            else_branch: clone_block(&i.else_branch, remap, next_hir_id, next_local),
            span: i.span.clone(),
        }),
        HirStmt::AlwaysFf(a) => HirStmt::AlwaysFf(HirAlwaysFfStmt {
            clock: remap.remap_local(a.clock, next_local),
            dest: remap.remap_local(a.dest, next_local),
            d_input: clone_expr(&a.d_input, remap, next_hir_id, next_local),
            span: a.span.clone(),
        }),
    }
}

fn clone_expr(
    expr: &HirExpr,
    remap: &mut BodyRemap,
    next_hir_id: &mut u32,
    next_local: &mut u32,
) -> HirExpr {
    let new_id = remap.remap_hir_id(expr.id, next_hir_id);
    let kind = match &expr.kind {
        HirExprKind::Const(c) => HirExprKind::Const(c.clone()),
        HirExprKind::Local(id) => HirExprKind::Local(remap.remap_local(*id, next_local)),
        HirExprKind::Param(i) => HirExprKind::Param(*i),
        HirExprKind::ConstVar(i) => HirExprKind::ConstVar(*i),
        HirExprKind::Call(c) => HirExprKind::Call(HirCall {
            callee: c.callee,
            args: c
                .args
                .iter()
                .map(|a| clone_arg(a, remap, next_hir_id, next_local))
                .collect(),
            span: c.span.clone(),
        }),
        HirExprKind::Field(f) => HirExprKind::Field(HirFieldAccess {
            receiver: Box::new(clone_expr(&f.receiver, remap, next_hir_id, next_local)),
            name: f.name.clone(),
            name_span: f.name_span.clone(),
        }),
        HirExprKind::MethodCall(mc) => HirExprKind::MethodCall(HirMethodCall {
            receiver: Box::new(clone_expr(&mc.receiver, remap, next_hir_id, next_local)),
            name: mc.name.clone(),
            name_span: mc.name_span.clone(),
            args: mc
                .args
                .iter()
                .map(|a| clone_arg(a, remap, next_hir_id, next_local))
                .collect(),
        }),
        HirExprKind::Block(b) => HirExprKind::Block(Box::new(HirBlockExpr {
            block: clone_block(&b.block, remap, next_hir_id, next_local),
            tail: b
                .tail
                .as_ref()
                .map(|t| clone_expr(t, remap, next_hir_id, next_local)),
        })),
        HirExprKind::If(ie) => HirExprKind::If(Box::new(HirIfExpr {
            condition: clone_expr(&ie.condition, remap, next_hir_id, next_local),
            then_branch: HirBlockExpr {
                block: clone_block(&ie.then_branch.block, remap, next_hir_id, next_local),
                tail: ie
                    .then_branch
                    .tail
                    .as_ref()
                    .map(|t| clone_expr(t, remap, next_hir_id, next_local)),
            },
            else_branch: HirBlockExpr {
                block: clone_block(&ie.else_branch.block, remap, next_hir_id, next_local),
                tail: ie
                    .else_branch
                    .tail
                    .as_ref()
                    .map(|t| clone_expr(t, remap, next_hir_id, next_local)),
            },
        })),
        HirExprKind::When(we) => HirExprKind::When(Box::new(HirWhenExpr {
            event: clone_expr(&we.event, remap, next_hir_id, next_local),
            body: HirBlockExpr {
                block: clone_block(&we.body.block, remap, next_hir_id, next_local),
                tail: we
                    .body
                    .tail
                    .as_ref()
                    .map(|t| clone_expr(t, remap, next_hir_id, next_local)),
            },
        })),
    };
    HirExpr {
        kind,
        ty: expr
            .ty
            .as_ref()
            .map(|t| substitute_type(t, remap.type_subst)),
        span: expr.span.clone(),
        id: new_id,
    }
}

fn clone_arg(
    arg: &HirArg,
    remap: &mut BodyRemap,
    next_hir_id: &mut u32,
    next_local: &mut u32,
) -> HirArg {
    match arg {
        HirArg::Inferable => HirArg::Inferable,
        HirArg::Provided { expr, source } => HirArg::Provided {
            expr: clone_expr(expr, remap, next_hir_id, next_local),
            source: *source,
        },
    }
}

// ---- Type substitution ----

fn substitute_type(ty: &HirType, subst: &HashMap<u32, HirType>) -> HirType {
    if subst.is_empty() {
        return ty.clone();
    }
    let kind = match &ty.kind {
        HirTypeKind::Value(vt) => {
            let domain = vt.domain.clone();
            let kind = match &vt.kind {
                ValueKind::Param(i) => match subst.get(i) {
                    Some(arg_ty) => match &arg_ty.kind {
                        HirTypeKind::Value(arg_vt) => {
                            return HirType {
                                kind: HirTypeKind::Value(ValueType {
                                    kind: arg_vt.kind.clone(),
                                    domain: match &domain {
                                        Domain::Unspecified => arg_vt.domain.clone(),
                                        other => other.clone(),
                                    },
                                }),
                                span: ty.span.clone(),
                            };
                        }
                        _ => return arg_ty.clone(),
                    },
                    None => vt.kind.clone(),
                },
                ValueKind::Struct { def, args } => ValueKind::Struct {
                    def: *def,
                    args: GenericArgs(
                        args.0
                            .iter()
                            .map(|a| match a {
                                GenericArg::Type(t) => GenericArg::Type(substitute_type(t, subst)),
                                other => other.clone(),
                            })
                            .collect(),
                    ),
                },
                ValueKind::UInt { width } => ValueKind::UInt {
                    width: Box::new(substitute_const_in_expr(width, subst)),
                },
                other => other.clone(),
            };
            HirTypeKind::Value(ValueType { kind, domain })
        }
        HirTypeKind::Port(p) => HirTypeKind::Port(PortTypeRef {
            def: p.def,
            args: GenericArgs(
                p.args
                    .0
                    .iter()
                    .map(|a| match a {
                        GenericArg::Type(t) => GenericArg::Type(substitute_type(t, subst)),
                        other => other.clone(),
                    })
                    .collect(),
            ),
            domain: p.domain.clone(),
        }),
        other => other.clone(),
    };
    HirType {
        kind,
        span: ty.span.clone(),
    }
}

fn substitute_const_in_expr(_expr: &HirExpr, _subst: &HashMap<u32, HirType>) -> HirExpr {
    // Const-kind args aren't part of `subst` (only Type-kind is). Width
    // expressions stay as-is.
    _expr.clone()
}

// ---- Call-site rewriting ----

type RewriteMap = HashMap<HirId, (DefId, Vec<usize>)>;

fn rewrite_calls_in_block(block: &mut HirBlock, rewrites: &RewriteMap) {
    for stmt in &mut block.statements {
        rewrite_calls_in_stmt(stmt, rewrites);
    }
}

fn rewrite_calls_in_stmt(stmt: &mut HirStmt, rewrites: &RewriteMap) {
    match stmt {
        HirStmt::Let(l) => rewrite_calls_in_expr(&mut l.value, rewrites),
        HirStmt::VarDecl(_) => {}
        HirStmt::Equation(eq) => rewrite_calls_in_expr(&mut eq.rhs, rewrites),
        HirStmt::Return(e) | HirStmt::Expr(e) => rewrite_calls_in_expr(e, rewrites),
        HirStmt::If(i) => {
            rewrite_calls_in_expr(&mut i.condition, rewrites);
            rewrite_calls_in_block(&mut i.then_branch, rewrites);
            rewrite_calls_in_block(&mut i.else_branch, rewrites);
        }
        HirStmt::AlwaysFf(a) => rewrite_calls_in_expr(&mut a.d_input, rewrites),
    }
}

fn rewrite_calls_in_expr(expr: &mut HirExpr, rewrites: &RewriteMap) {
    if let HirExprKind::Call(c) = &mut expr.kind {
        if let Some((new_def, drop_idxs)) = rewrites.get(&expr.id) {
            c.callee = *new_def;
            // Remove the args whose params were substituted out. Iterate
            // in reverse so earlier indices stay valid.
            let mut sorted: Vec<usize> = drop_idxs.clone();
            sorted.sort_unstable();
            sorted.dedup();
            for idx in sorted.iter().rev() {
                if *idx < c.args.len() {
                    c.args.remove(*idx);
                }
            }
        }
        for arg in &mut c.args {
            if let HirArg::Provided { expr: a, .. } = arg {
                rewrite_calls_in_expr(a, rewrites);
            }
        }
        return;
    }
    match &mut expr.kind {
        HirExprKind::Field(f) => rewrite_calls_in_expr(&mut f.receiver, rewrites),
        HirExprKind::MethodCall(mc) => {
            rewrite_calls_in_expr(&mut mc.receiver, rewrites);
            for arg in &mut mc.args {
                if let HirArg::Provided { expr: a, .. } = arg {
                    rewrite_calls_in_expr(a, rewrites);
                }
            }
        }
        HirExprKind::Block(b) => {
            rewrite_calls_in_block(&mut b.block, rewrites);
            if let Some(t) = &mut b.tail {
                rewrite_calls_in_expr(t, rewrites);
            }
        }
        HirExprKind::If(ie) => {
            rewrite_calls_in_expr(&mut ie.condition, rewrites);
            rewrite_calls_in_block(&mut ie.then_branch.block, rewrites);
            if let Some(t) = &mut ie.then_branch.tail {
                rewrite_calls_in_expr(t, rewrites);
            }
            rewrite_calls_in_block(&mut ie.else_branch.block, rewrites);
            if let Some(t) = &mut ie.else_branch.tail {
                rewrite_calls_in_expr(t, rewrites);
            }
        }
        HirExprKind::When(we) => {
            rewrite_calls_in_expr(&mut we.event, rewrites);
            rewrite_calls_in_block(&mut we.body.block, rewrites);
            if let Some(t) = &mut we.body.tail {
                rewrite_calls_in_expr(t, rewrites);
            }
        }
        _ => {}
    }
}

/// Build a map spec DefId → original DefId for the just-built specs.
fn spec_def_to_orig(specs: &[(DefId, DefId, GenericArgs)]) -> HashMap<DefId, DefId> {
    specs.iter().map(|(orig, spec, _)| (*spec, *orig)).collect()
}

/// For a given spec DefId, compute the indices in the original callee's
/// `params` vec that the spec drops (i.e. the Type-kind generic-param
/// slots). Each entry maps a slot in the original call args that must
/// be removed so the call shape matches the spec's `params`.
fn dropped_arg_indices(
    fns_by_def: &HashMap<DefId, HirFn>,
    spec_to_orig: &HashMap<DefId, DefId>,
    spec_def: DefId,
    resolve: &ResolveResult,
) -> Vec<usize> {
    let Some(orig_def) = spec_to_orig.get(&spec_def) else {
        return Vec::new();
    };
    let Some(orig_fn) = fns_by_def.get(orig_def) else {
        return Vec::new();
    };
    let info = resolve.def_info(*orig_def);
    let drop_names: Vec<&str> = info
        .generic_params
        .iter()
        .filter(|gp| matches!(gp.kind, GenericParamKind::Type))
        .map(|gp| gp.name.as_str())
        .collect();
    orig_fn
        .params
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let name = orig_fn
                .locals
                .get(p.local.0 as usize)
                .map(|l| l.name.as_str())?;
            if drop_names.contains(&name) {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

#[allow(dead_code)]
fn _unused(_: ConstValue, _: HirArgSource) {}
