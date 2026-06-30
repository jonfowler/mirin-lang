//! HIR → SV IR lowering + the `verilog` driver (`planning/q5_backend.md`).
//!
//! **Q5b scope:** the combinational scalar case — a `fn` over scalar `uint`/`bool`
//! becomes an `SvModule` whose value params + return are ports and whose
//! `let`/`var`/equation/return bodies become `logic` decls + `assign`s.
//!
//! **Q5c scope:** registers and control flow. `e.reg(rst, init)` → an
//! `always_ff` with a synchronous active-low reset (the bound local is the
//! register); `when ev { d }` → a reset-less `always_ff` (`d` is the D-input);
//! `if c { a } else { b }` → an `always_comb` mux. The last two write a synthetic
//! `__block_N` whose value the surrounding assignment reads. Shadowed `let`s are
//! uniquified (`data` / `data_1` / `data_2`) the way the reference compiler does.
//!
//! **Q5d scope:** flatten + instantiation. Struct/port-typed params, return, and
//! locals erase to per-field scalar leaves (`base__field`) via [`flatten_leaves`]:
//! field access projects, record literals rebuild, `.reg` on an aggregate emits
//! one `always_ff` per field, and a port equation becomes one connection per
//! field (the sink chosen by each leaf's module direction). A user `fn`/method
//! call becomes a submodule [`SvInstance`](crate::backend::ir::SvInstance):
//! positional params match `[receiver?] ++ args`, named params match the call's
//! named section, `out`-args bind callee `out` params to caller places, and the
//! return wires to the binding / `result` / a fresh `__call_N`. Methods qualify
//! their module name by owner (`Option::reg` → `Option__reg`).
//!
//! **Q5-mono scope:** parametric widths/types. A Const-kind generic becomes an
//! SV `#(parameter int N)`, and a symbolic width `uint(N)` renders `[N-1:0]`
//! (via [`sv_type`]). When [`flatten_leaves`] descends a struct/port with generic
//! args, it substitutes the def's `Param`/`ConstArg::Param`/`Domain::Param` with
//! the use-site args ([`subst_type`]) — a `Bus(uint(8))` field `data: A` becomes
//! `uint(8)`. A **type-generic `fn`** is not emitted directly: [`verilog`] skips
//! it, and each call ([`SvLower::emit_instance`]) binds its Type params from the
//! actual arg types ([`match_type`]), names a specialised copy `Callee__Arg`
//! ([`mono_name`]), and the driver emits one module per unique instance via
//! [`build_module`] with a `self_subst`.

use std::collections::{HashMap, HashSet};

use crate::backend::ir::{
    SvAlwaysComb, SvAlwaysFf, SvBinOp, SvCombAssert, SvCombIf, SvCombStmt, SvExpr, SvFile,
    SvFunction, SvGenerateFor, SvGenerateIf, SvInstance, SvItem, SvLogicDecl, SvModule, SvPort,
    SvPortDirection, SvSeqAssign, SvType,
};
use crate::base::db::SourceRoot;
use crate::hir::body::{Body, LocalKind, VerilogSegment, VerilogTemplate, body};
use crate::hir::check::{check_drivers, completeness, directions};
use crate::hir::infer::{Inference, infer};
use crate::hir::sig::{Signature, sig_of};
use crate::hir::types::{
    ConstArg, ConstOp, Direction, Domain, Folder, GenericArgs, GenericParam, LocalId, Term,
    TermKind, Type, ValueKind, subst_const_opt,
};
use crate::mir::ir::{
    BuiltinMethod, Conn, MBlock, MExprId, MExprKind, MNamedArg, MStmt, Mir, Place, Projection,
};
use crate::mir::lower::mir_of;
use crate::nameres::def_map::{CrateDefMap, crate_def_map};
use crate::nameres::ids::{DefId, DefKind, Namespace};
use crate::syntax::ast_id::ast_id_map;
use crate::syntax::syntax_errors::syntax_errors;

/// QUERY: lower one fn/method to a SystemVerilog module (combinational scalar
/// subset). Non-fn defs yield an empty module. (A type-generic fn lowers with no
/// substitution — its concrete copies come from [`verilog`]'s mono collector.)
#[salsa::tracked(returns(ref))]
pub fn sv_module<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> SvModule {
    let name = module_name(crate_def_map(db, krate), def);
    build_module(db, krate, def, &[], name).0
}

/// A request to monomorphise a type-generic callee at concrete type args: the
/// callee def, the substitution for its own generics, and the specialised module
/// name (`pipeline_para__Write`).
struct MonoReq<'db> {
    callee: DefId<'db>,
    subst: Vec<Option<Term<'db>>>,
    name: String,
}

/// Lower a def to one `SvModule` named `name`, substituting the def's own
/// generics by `self_subst` (empty for a plain fn; a Type-kind binding for a
/// monomorphised copy). Returns the module plus any type-generic callees it
/// instantiated (for the driver to emit specialised copies of).
fn build_module<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    self_subst: &[Option<Term<'db>>],
    name: String,
) -> (SvModule, Vec<MonoReq<'db>>) {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return (SvModule::default(), Vec::new());
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return (SvModule::default(), Vec::new());
    }
    let sig = sig_of(db, krate, def);
    if is_const_only_fn(sig) {
        // A const fn (integer-returning, or all-integer params/outs) is
        // compile-time only — its results reach hardware through evaluated
        // widths, never as a module.
        return (SvModule::default(), Vec::new());
    }
    let body = body(db, krate, def);

    // Ports: `dom` generics → clock inputs; value params and the return type are
    // flattened per-field (`inp: Packet @clk` → `inp__valid` / `inp__payload`),
    // each field's module direction folding the param/return direction with the
    // port-field direction. `self_subst` resolves the def's own type generics.
    let mut ports = Vec::new();
    for g in &sig.generic_params {
        // The lifted `__Dom` is checking-only — no clock port (a pure fn is
        // combinational).
        if matches!(g.kind, TermKind::Domain(_)) && !g.is_lifted_dom() {
            ports.push(SvPort {
                direction: SvPortDirection::Input,
                ty: SvType::bit(),
                name: g.name.clone(),
            });
        }
    }
    for p in &sig.params {
        let ty = ground_widths(db, krate, def, &subst_type(&p.ty, self_subst));
        if is_integer(&ty) {
            continue; // compile-time only — no port
        }
        let drives = p.direction == Some(Direction::Out);
        for leaf in flatten_leaves(db, krate, def, &ty, drives, &sig.generic_params) {
            ports.push(SvPort {
                direction: if leaf.drives {
                    SvPortDirection::Output
                } else {
                    SvPortDirection::Input
                },
                ty: leaf.ty,
                name: join(&p.name, &leaf.suffix),
            });
        }
    }
    // Result ports, one group per result place: an unnamed `return` is
    // `result__…`, a named result/tuple part uses its bound name as the base
    // (`output__valid`, `sum`) — planning/return_variable.md.
    for place in &sig.result_places {
        let pty = ground_widths(db, krate, def, &subst_type(&place.ty, self_subst));
        // A compile-time integer result is not a port.
        if is_integer(&pty) {
            continue;
        }
        // `drives=true`: the module produces the result. A returned PORT's
        // consumer-side (`in`) fields fold to `drives=false` — they are
        // inputs to this module (the downstream's backpressure), exactly as
        // for an `out` port parameter. Respect the per-leaf flag rather than
        // forcing every result leaf to an output.
        for leaf in flatten_leaves(db, krate, def, &pty, true, &sig.generic_params) {
            ports.push(SvPort {
                direction: if leaf.drives {
                    SvPortDirection::Output
                } else {
                    SvPortDirection::Input
                },
                ty: leaf.ty,
                name: join(&place.sv_base, &leaf.suffix),
            });
        }
    }

    let inf = infer(db, krate, def);
    let mir = mir_of(db, krate, def);
    let mut lower = SvLower {
        db,
        krate,
        def,
        map,
        body,
        inf,
        mir,
        sig,
        self_subst: self_subst.to_vec(),
        local_names: unique_local_names(body),
        items: Vec::new(),
        synth: 0,
        index_asserts: std::collections::HashSet::new(),
        instance_counts: HashMap::new(),
        declared: HashSet::new(),
        mono_reqs: Vec::new(),
        promoted: HashMap::new(),
        fns_emitted: HashSet::new(),
        prefix: String::new(),
        inline_depth: 0,
    };
    if let Some(template) = body.verilog() {
        lower
            .items
            .push(SvItem::Verbatim(render_verilog(template, sig)));
    } else {
        // Emission lowers from MIR. The native walker covers the whole hardware
        // corpus (proven byte-for-byte by `golden_sv_snapshot`); the HIR
        // statement-lowering path is retired. An unhandled construct is a loud
        // `todo!` in the walker, not a silent fallback.
        lower.lower_top_block(mir.block());
    }

    // Discharge symbolic width obligations (`uint(n)` vs `uint(m)`) as
    // `initial assert (n == m)`, rendering each param index via its name.
    let name_of = |i: u32| {
        sig.generic_params
            .get(i as usize)
            .map(|g| g.name.clone())
            .unwrap_or_default()
    };
    for (a, b) in inf.const_residuals() {
        // Param-Param residuals are dischargeable at elaboration; other
        // symbolic shapes wait for const_eval (Q4c).
        if let (ConstArg::Param(i), ConstArg::Param(j)) = (a, b) {
            lower.items.push(SvItem::InitialAssert {
                cond: SvExpr::BinOp(
                    SvBinOp::Eq,
                    Box::new(SvExpr::Ident(name_of(*i))),
                    Box::new(SvExpr::Ident(name_of(*j))),
                ),
            });
        }
    }

    // Literal-fit residuals (a literal against a still-symbolic width)
    // discharge at elaboration: `initial assert (255 < (1 << n));`.
    for fit in inf.fit_residuals() {
        if let ConstArg::Param(i) = &fit.width {
            lower.items.push(SvItem::InitialAssert {
                cond: SvExpr::Lit(format!("{} < (1 << {})", fit.value, name_of(*i))),
            });
        }
    }

    // Const-kind generics become SV `#(parameter int N)`, in declaration order.
    let parameters = sig
        .generic_params
        .iter()
        .filter(|g| g.kind == TermKind::Const)
        .map(|g| crate::backend::ir::SvParameter {
            name: g.name.clone(),
            default: None,
        })
        .collect();

    (
        SvModule {
            name,
            parameters,
            ports,
            items: lower.items,
        },
        lower.mono_reqs,
    )
}

/// Lower a const-only (`integer`-in, `integer`-out) `fn` to an in-module SV
/// `function automatic int`. Used when such a fn is called in a constant
/// position (`let w = f(N)`), where a module instance is illegal. Returns
/// `None` if the callee is not a lowerable const fn. (const_net_duality.md.)
fn build_const_function<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    name: &str,
) -> Option<SvFunction> {
    let map = crate_def_map(db, krate);
    let data = map.def_data(def)?;
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return None;
    }
    let sig = sig_of(db, krate, def);
    let body = body(db, krate, def);
    let inf = infer(db, krate, def);
    let mir = mir_of(db, krate, def);
    let local_names = unique_local_names(body);
    let mut lower = SvLower {
        db,
        krate,
        def,
        map,
        body,
        inf,
        mir,
        sig,
        self_subst: Vec::new(),
        local_names,
        items: Vec::new(),
        synth: 0,
        index_asserts: std::collections::HashSet::new(),
        instance_counts: HashMap::new(),
        declared: HashSet::new(),
        mono_reqs: Vec::new(),
        promoted: HashMap::new(),
        fns_emitted: HashSet::new(),
        prefix: String::new(),
        inline_depth: 0,
    };
    // The integer params are the function's input args; record them as
    // "promoted" so a `Local` use in a width/loop-bound renders as the arg name
    // (`range(n)` → `i < n`) rather than panicking on an ungrounded `Local`.
    for p in &sig.params {
        let arg = lower.local_names[p.local.0 as usize].clone();
        lower.promoted.insert(p.local, arg);
    }
    Some(lower.lower_const_function(name))
}

/// `true` if a def has any Type-kind generic param (so it is not emitted
/// directly — only its monomorphised copies are).
fn is_type_generic(sig: &Signature<'_>) -> bool {
    sig.generic_params.iter().any(|g| g.kind == TermKind::Type)
}

/// The specialised module name for a type-generic callee at `subst`:
/// `Callee__Arg` per bound Type-kind generic (`pipeline_para__Write`).
fn mono_name<'db>(
    map: &CrateDefMap<'db>,
    callee: DefId<'db>,
    sig: &Signature<'db>,
    subst: &[Option<Term<'db>>],
) -> String {
    let mut name = module_name(map, callee);
    for (i, g) in sig.generic_params.iter().enumerate() {
        if g.kind == TermKind::Type
            && let Some(Term::Type(t)) = subst.get(i).and_then(|o| o.as_ref())
        {
            name.push_str("__");
            name.push_str(&type_arg_name(map, t));
        }
    }
    name
}

/// A short name for a concrete type arg, for the monomorphised module name.
fn type_arg_name<'db>(map: &CrateDefMap<'db>, ty: &Type<'db>) -> String {
    match ty {
        Type::Port { def, .. } => map
            .def_data(*def)
            .map(|d| d.name.clone())
            .unwrap_or_default(),
        Type::Value {
            kind: ValueKind::UInt {
                width: ConstArg::Lit(w),
            },
            ..
        } => format!("uint{w}"),
        Type::Value {
            kind: ValueKind::SInt {
                width: ConstArg::Lit(w),
            },
            ..
        } => format!("sint{w}"),
        Type::Value {
            kind: ValueKind::Bits {
                width: ConstArg::Lit(w),
            },
            ..
        } => format!("bits{w}"),
        Type::Value {
            kind: ValueKind::Bool,
            ..
        } => "bool".to_owned(),
        _ => "T".to_owned(),
    }
}

/// Bind a type-generic callee's Type-kind params by matching its (declared)
/// param types against the call's actual arg types — `w: Bus(A)` vs the actual
/// `Bus(Write)` binds `A := Write`. Indexed by the callee's generic position.
fn match_type<'db>(callee: &Type<'db>, actual: &Type<'db>, subst: &mut [Option<Term<'db>>]) {
    match (callee, actual) {
        (
            Type::Value {
                kind: ValueKind::Param(i),
                ..
            },
            _,
        ) => {
            if let Some(slot) = subst.get_mut(*i as usize) {
                *slot = Some(Term::Type(actual.clone()));
            }
        }
        (
            Type::Port {
                def: cd, args: ca, ..
            },
            Type::Port {
                def: ad, args: aa, ..
            },
        ) if cd == ad => match_args(ca, aa, subst),
        // `Vec(N, A)` binds its element type param `A`; the length `N` is a
        // Const-kind generic that rides the `#(...)` parameter (like a Port's
        // const args), so it is NOT bound into the mono subst here.
        (Type::Vec { elem: ce, .. }, Type::Vec { elem: ae, .. }) => match_type(ce, ae, subst),
        // A tuple binds each element type param positionally (the per-arity
        // tuple impls' element types). Same arity is guaranteed by header match.
        (Type::Tuple(ce), Type::Tuple(ae)) if ce.len() == ae.len() => {
            for (c, a) in ce.iter().zip(ae) {
                match_type(c, a, subst);
            }
        }
        _ => {}
    }
}

fn match_args<'db>(
    callee: &GenericArgs<'db>,
    actual: &GenericArgs<'db>,
    subst: &mut [Option<Term<'db>>],
) {
    for (c, a) in callee.0.iter().zip(&actual.0) {
        if let (Term::Type(ct), Term::Type(at)) = (c, a) {
            match_type(ct, at, subst);
        }
    }
}

/// A def's SV module name: a `fn` keeps its name; a `method` is qualified by its
/// owner type (`Option::reg` → `Option__reg`), matching the reference compiler.
fn module_name<'db>(map: &CrateDefMap<'db>, def: DefId<'db>) -> String {
    let Some(data) = map.def_data(def) else {
        return String::new();
    };
    if data.kind == DefKind::Method
        && let Some(owner) = data.owner.and_then(|o| map.def_data(o))
    {
        // A trait-impl method carries the trait in its module name so two
        // traits' same-named methods on one owner can't collide.
        if let Some(t) = map.trait_of_method(def).and_then(|t| map.def_data(t)) {
            return format!("{}__{}__{}", owner.name, t.name, data.name);
        }
        return format!("{}__{}", owner.name, data.name);
    }
    data.name.clone()
}

/// A method's `self` type: the owner struct/port the `impl` is on. `self`'s
/// structural type is left `Error` in the signature (it is method-dispatch's job
/// in `infer`), but flatten only needs the aggregate shape, so the domain is
/// irrelevant here.
/// One SV name per local, uniquified so shadowing `let`s don't collide: the
/// first use of a name keeps it, later uses get `_1`, `_2`, … (matching the
/// reference compiler's `data` / `data_1` / `data_2`).
fn unique_local_names(body: &Body<'_>) -> Vec<String> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut names = Vec::with_capacity(body.locals().len());
    for l in body.locals() {
        let n = seen.entry(l.name.clone()).or_insert(0);
        names.push(if *n == 0 {
            l.name.clone()
        } else {
            format!("{}_{}", l.name, n)
        });
        *n += 1;
    }
    names
}

/// QUERY: the crate's SystemVerilog as text — the deterministic print of
/// [`sv_file`].
#[salsa::tracked(returns(ref))]
pub fn verilog(db: &dyn salsa::Database, krate: SourceRoot) -> String {
    sv_file(db, krate).to_string()
}

/// QUERY: the crate's SystemVerilog IR — every `fn`/method as a module. (Driver:
/// "force `verilog` for each top-level item.") Modules are erased before codegen,
/// so every `fn`/method in the crate (at the root or nested in a `mod`/`impl`)
/// becomes a top-level SV module, emitted in **source order** (across files by
/// path, within a file by byte position) to match the reference compiler.
#[salsa::tracked(returns(ref))]
pub fn sv_file(db: &dyn salsa::Database, krate: SourceRoot) -> SvFile {
    // Emission ASSUMES a well-typed crate: every type is concrete and
    // flattenable, every width grounds to a literal or a generic param. So a
    // crate that still has front-end diagnostics emits NOTHING — the diagnostics
    // are the report, and the unrenderable-type/width cases in `sv_type` /
    // `width_expr` become genuine invariant violations (hard errors) rather than
    // states reachable from bad input.
    if !crate_emittable(db, krate) {
        return SvFile::default();
    }
    let map = crate_def_map(db, krate);
    let prelude = map.prelude();
    // Concrete fns/methods in source order; a type-generic fn is *not* emitted
    // directly — only its monomorphised copies (collected from call sites).
    let mut fns: Vec<(String, usize, DefId)> = map
        .defs()
        .filter_map(|d| map.def_data(d).map(|data| (d, data)))
        .filter(|(_, data)| {
            matches!(data.kind, DefKind::Fn | DefKind::Method) && data.module != prelude
        })
        // `#[inline]` fns are spliced at call sites, never emitted as modules.
        .filter(|(_, data)| !data.inline)
        // A trait's method DECLS have no bodies — only impls emit modules.
        .filter(|(d, _)| !map.is_trait_method_decl(*d))
        .filter(|(d, _)| !is_type_generic(sig_of(db, krate, *d)))
        .map(|(d, _)| {
            let file = d.file(db);
            let start = ast_id_map(db, file)
                .range_of(d.ast_id(db))
                .map(|(s, _)| s)
                .unwrap_or(0);
            (file.path(db).to_string_lossy().into_owned(), start, d)
        })
        .collect();
    fns.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));

    let mut modules = Vec::new();
    let mut reqs: Vec<MonoReq> = Vec::new();
    let fns: Vec<_> = fns
        .into_iter()
        .filter(|(_, _, d)| !is_const_only_fn(sig_of(db, krate, *d)))
        .collect();
    for (_, _, def) in &fns {
        let (m, r) = build_module(db, krate, *def, &[], module_name(map, *def));
        modules.push(m);
        reqs.extend(r);
    }
    // Emit one specialised module per unique monomorphised instance (a worklist:
    // a mono copy may itself instantiate further generic callees). Appended after
    // the source-ordered concrete modules, name-sorted for determinism.
    let mut seen: HashSet<String> = HashSet::new();
    let mut mono: Vec<SvModule> = Vec::new();
    while let Some(req) = reqs.pop() {
        if !seen.insert(req.name.clone()) {
            continue;
        }
        let (m, r) = build_module(db, krate, req.callee, &req.subst, req.name);
        mono.push(m);
        reqs.extend(r);
    }
    mono.sort_by(|a, b| a.name.cmp(&b.name));
    modules.extend(mono);
    SvFile { modules }
}

/// Is the crate free of front-end diagnostics, so emission may proceed? Covers
/// syntax, name resolution, signatures, bodies, inference, drivers, completeness
/// and directions — the same set the CLI reports before it would emit. It does
/// NOT include `reserved_words`, which is itself computed from the emitted SV
/// (`sv_file`) and so cannot gate it. The gate is what lets the SV-rendering
/// helpers treat an unrenderable type/width as a hard error: on a clean crate
/// they cannot occur.
fn crate_emittable(db: &dyn salsa::Database, krate: SourceRoot) -> bool {
    for &file in krate.files(db) {
        if !syntax_errors(db, file).is_empty() {
            return false;
        }
    }
    let map = crate_def_map(db, krate);
    if !map.diagnostics().is_empty() {
        return false;
    }
    for def in map.defs().collect::<Vec<_>>() {
        match map.def_data(def).map(|d| d.kind) {
            Some(DefKind::Fn | DefKind::Method) => {
                if !sig_of(db, krate, def).diagnostics.is_empty()
                    || !body(db, krate, def).diagnostics().is_empty()
                    || !infer(db, krate, def).diagnostics().is_empty()
                    || !check_drivers(db, krate, def).is_empty()
                    || !completeness(db, krate, def).is_empty()
                    || !directions(db, krate, def).is_empty()
                {
                    return false;
                }
            }
            // Struct/port/impl HEADERS carry only signature diagnostics.
            Some(DefKind::Struct | DefKind::Port | DefKind::Impl) => {
                if !sig_of(db, krate, def).diagnostics.is_empty() {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

struct SvLower<'a, 'db> {
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    /// The def being lowered (to resolve its `self` param's owner type).
    def: DefId<'db>,
    map: &'a CrateDefMap<'db>,
    body: &'a Body<'db>,
    inf: &'a Inference<'db>,
    /// The def's MIR — the single lowering source (types-on-node, resolved
    /// dispatch and places). Note: MIR types are inference-recorded, NOT
    /// mono-ground — callers still apply `self_subst` + `ground_widths`.
    mir: &'a Mir<'db>,
    sig: &'a Signature<'db>,
    /// Substitution for the def's own generics (a Type-kind binding when lowering
    /// a monomorphised copy; empty otherwise). Applied to every type read from
    /// the signature/inference before flattening.
    self_subst: Vec<Option<Term<'db>>>,
    /// Uniquified SV name per [`LocalId`].
    local_names: Vec<String>,
    items: Vec<SvItem>,
    /// Counter for synthetic `__block_N` (`when`/`if`) and `__call_N` (nested
    /// call-value) locals.
    synth: u32,
    /// Dynamic-index bounds asserts already emitted (dedup per module).
    index_asserts: std::collections::HashSet<String>,
    /// Per-callee instance counter (first instance bare, then `_1`, `_2`, …).
    instance_counts: HashMap<String, u32>,
    /// Locals already given a `logic` declaration (so an out-target reused as a
    /// later input is declared once).
    declared: HashSet<LocalId>,
    /// Type-generic callees instantiated here — specialised copies the driver
    /// must emit.
    mono_reqs: Vec<MonoReq<'db>>,
    /// Const body locals promoted to `localparam`s (a symbolic `let w = …` that
    /// sizes a signal). A `ConstArg::Local(l)` for one of these renders as the
    /// localparam's name; the const fn it calls is emitted as an in-module
    /// function. Keyed by local; value is the SV localparam name.
    promoted: HashMap<LocalId, String>,
    /// In-module SV `function`s already emitted (dedup by name).
    fns_emitted: HashSet<String>,
    /// Name prefix for every net/instance this lower mints. Empty for the
    /// top-level module (so its emission is byte-identical to before MIR-inline);
    /// an inline splice runs a *nested* `SvLower` over the callee with a unique
    /// `__inl{site}__` prefix so its locals, synth blocks, and nested instances
    /// merge into the caller without colliding (planning/inline_bodies.md).
    prefix: String,
    /// Inline-splice nesting depth (a recursion guard against `#[inline]` cycles).
    inline_depth: u32,
}

impl<'db> SvLower<'_, 'db> {
    // --- const-fn lowering -------------------------------------------------
    // A const-only def lowers to an SV `function automatic int`: integer params →
    // input args, `let`/accumulator locals → `int` locals, the body's procedural
    // shape (assigns, a loop-carried fold) → `SvCombStmt`s, the tail/`return` →
    // the return expression. Scalar-integer only (the common width helper).

    /// Lower this (const-only) def's body to an SV `function`.
    fn lower_const_function(&mut self, name: &str) -> SvFunction {
        let params: Vec<String> = self
            .sig
            .params
            .iter()
            .map(|p| self.local_name(p.local))
            .collect();
        let block = self.mir.block().clone();
        let mut locals = Vec::new();
        let mut body = Vec::new();
        self.const_stmts(&block.stmts, &mut locals, &mut body);
        let ret = if let Some(t) = block.tail {
            self.expr_value(t)
        } else if let Some(rhs) = self.result_equation_rhs(&block) {
            self.expr_value(rhs)
        } else if let Some(MStmt::Return { value }) = block
            .stmts
            .iter()
            .find(|s| matches!(s, MStmt::Return { .. }))
        {
            self.expr_value(*value)
        } else {
            SvExpr::Lit("0".to_owned())
        };
        SvFunction {
            name: name.to_owned(),
            params,
            locals,
            body,
            ret,
        }
    }

    /// The RHS of the whole-result
    /// equation (`result = EXPR`) — a const fn's return value with no tail.
    fn result_equation_rhs(&self, block: &MBlock) -> Option<MExprId> {
        block.stmts.iter().find_map(|s| match s {
            MStmt::Equation { lhs, rhs }
                if lhs.projections.is_empty() && self.mir.local(lhs.base).result_base.is_some() =>
            {
                Some(*rhs)
            }
            _ => None,
        })
    }

    /// Does a loop body reassign `acc`?
    fn for_carries(&self, body: &MBlock, acc: LocalId) -> bool {
        body.stmts
            .iter()
            .any(|s| matches!(s, MStmt::Equation { lhs, .. } if lhs.base == acc))
    }

    /// Lower a const fn's statements to procedural `SvCombStmt`s, collecting `int` locals.
    fn const_stmts(
        &mut self,
        stmts: &[MStmt],
        locals: &mut Vec<String>,
        out: &mut Vec<SvCombStmt>,
    ) {
        let mut i = 0;
        while i < stmts.len() {
            // `let mut acc = init;` + carrying `for`/assigns → an accumulator.
            if let MStmt::Let { local, value } = &stmts[i]
                && self.mir.local(*local).mutable
            {
                let (acc, init) = (*local, *value);
                let mut steps: Vec<MStmt> = Vec::new();
                let mut j = i + 1;
                while let Some(stmt) = stmts.get(j) {
                    let carries = match stmt {
                        MStmt::Equation { lhs, .. } => lhs.base == acc,
                        MStmt::For { body, .. } => self.for_carries(body, acc),
                        _ => false,
                    };
                    if !carries {
                        break;
                    }
                    steps.push(stmt.clone());
                    j += 1;
                }
                if !steps.is_empty() {
                    let acc_name = self.local_name(acc);
                    locals.push(acc_name.clone());
                    let init_val = self.expr_value(init);
                    out.push(SvCombStmt::Assign {
                        lhs: SvExpr::Ident(acc_name),
                        rhs: init_val,
                    });
                    self.const_fold_steps(&steps, out);
                    i = j;
                    continue;
                }
            }
            // A plain `let x = e;` → `int x; x = e;`.
            if let MStmt::Let { local, value } = &stmts[i] {
                let nm = self.local_name(*local);
                locals.push(nm.clone());
                let v = self.expr_value(*value);
                out.push(SvCombStmt::Assign {
                    lhs: SvExpr::Ident(nm),
                    rhs: v,
                });
            }
            i += 1;
        }
    }

    /// The LHS is a [`Place`]; a const
    /// fold is scalar so it is a bare local (no projections).
    fn const_fold_steps(&mut self, steps: &[MStmt], out: &mut Vec<SvCombStmt>) {
        for step in steps {
            match step {
                MStmt::Equation { lhs, rhs } => {
                    let lhs = SvExpr::Ident(self.local_name(lhs.base));
                    let rhs = self.expr_value(*rhs);
                    out.push(SvCombStmt::Assign { lhs, rhs });
                }
                MStmt::For {
                    index,
                    elem,
                    iter,
                    body,
                } => {
                    let Some((bound, var)) = self.loop_bound_var(*index, *elem, *iter) else {
                        continue;
                    };
                    let mut inner = Vec::new();
                    for stmt in &body.stmts {
                        if let MStmt::Equation { lhs, rhs } = stmt {
                            let lhs = SvExpr::Ident(self.local_name(lhs.base));
                            let rhs = self.expr_value(*rhs);
                            inner.push(SvCombStmt::Assign { lhs, rhs });
                        }
                    }
                    out.push(SvCombStmt::For {
                        var,
                        bound,
                        body: inner,
                    });
                }
                _ => {}
            }
        }
    }

    /// A named generate-for. The bound comes from the iterable's MIR-node type;
    /// the elem is the genvar (a `range`) or an `assign x = v[i]` binding.
    fn lower_for(&mut self, index: Option<LocalId>, elem: LocalId, iter: MExprId, body: &MBlock) {
        let it = {
            let t = ground_widths(
                self.db,
                self.krate,
                self.def,
                &subst_type(&self.mir.expr(iter).ty, &self.self_subst),
            );
            self.subst_promoted(&t)
        };
        let (len, is_bits) = match &it {
            Type::Vec { len, .. } => (len.clone(), false),
            Type::Value {
                kind: ValueKind::Bits { width },
                ..
            } => (width.clone(), true),
            _ => return,
        };
        let bound = width_expr(&len, &self.sig.generic_params);
        let elem_is_genvar = matches!(self.mir.local(elem).kind, LocalKind::ForBound);
        let var = match (elem_is_genvar, index) {
            (true, _) => self.local_name(elem),
            (false, Some(i)) => self.local_name(i),
            (false, None) => {
                let v = format!("__i{}", self.synth);
                self.synth += 1;
                v
            }
        };
        let label = format!("g_{}", self.local_name(elem));
        let saved = std::mem::take(&mut self.items);
        if elem_is_genvar {
            // no binding — the genvar is the element
        } else if is_bits {
            let base = self.expr_value(iter);
            self.declare_local(elem);
            let name = self.local_name(elem);
            self.items.push(SvItem::Assign {
                lhs: SvExpr::Ident(name),
                rhs: SvExpr::Lit(format!("{base}[{var}]")),
            });
        } else {
            self.declare_local(elem);
            let elem_base = self.local_name(elem);
            for (suffix, e) in self.expr_leaves(iter) {
                self.items.push(SvItem::Assign {
                    lhs: SvExpr::Ident(join(&elem_base, &suffix)),
                    rhs: SvExpr::Lit(format!("{e}[{var}]")),
                });
            }
        }
        self.lower_stmts(&body.stmts);
        let items = std::mem::replace(&mut self.items, saved);
        self.items.push(SvItem::GenerateFor(SvGenerateFor {
            var,
            bound,
            label,
            items,
        }));
    }

    /// True if a const (`integer`) local is symbolic — its value depends on a
    /// generic param, so it cannot fold to a literal and must ride a
    /// `localparam` / the SV elaborator (vs. a concrete one folded inline).
    fn is_symbolic_const(&self, local: LocalId) -> bool {
        matches!(
            crate::hir::const_eval::eval_width(
                self.db,
                self.krate,
                self.def,
                &ConstArg::Local(local),
            ),
            crate::hir::const_eval::WidthEval::Symbolic,
        )
    }

    /// Render a const-expression RHS (`N + N`, `N`) inline as an SV expression.
    fn const_rhs(&mut self, value: MExprId) -> SvExpr {
        if self.is_instance_call(value) {
            return self.emit_const_call(value);
        }
        self.expr_value(value)
    }

    /// Emit a call to a const SV `function` with its integer args.
    fn emit_const_call(&mut self, m: MExprId) -> SvExpr {
        let MExprKind::Call {
            callee,
            substs,
            args,
            ..
        } = &self.mir.expr(m).kind
        else {
            unreachable!("emit_const_call on a non-Call node");
        };
        let (callee, substs, args) = (*callee, substs.clone(), args.clone());
        let (def, _) = self.mir_call_target(callee, &substs);
        let fname = module_name(self.map, def);
        if self.fns_emitted.insert(fname.clone())
            && let Some(fun) = build_const_function(self.db, self.krate, def, &fname)
        {
            self.items.push(SvItem::Function(fun));
        }
        let arg_strs: Vec<String> = args
            .iter()
            .filter_map(|a| match a {
                Conn::In(e) => Some(self.expr_value(*e).to_string()),
                Conn::Out(_) => None,
            })
            .collect();
        SvExpr::Lit(format!("{fname}({})", arg_strs.join(", ")))
    }

    /// Replace each promoted-local `ConstArg::Local(l)` in a type with its
    /// `localparam` name (`uint(w)` → `[w-1:0]`), so a body var sized by a
    /// promoted const renders against the localparam rather than panicking on
    /// an ungrounded `Local`.
    fn subst_promoted(&self, ty: &Type<'db>) -> Type<'db> {
        if self.promoted.is_empty() {
            return ty.clone();
        }
        PromotedFolder {
            promoted: &self.promoted,
        }
        .fold_type(ty)
    }

    /// As [`Self::subst_promoted`], for a bare const (an inline-verilog
    /// `${to}` splice whose generic bound to a promoted local).
    fn subst_promoted_const(&self, c: &ConstArg<'db>) -> ConstArg<'db> {
        if self.promoted.is_empty() {
            return c.clone();
        }
        PromotedFolder {
            promoted: &self.promoted,
        }
        .fold_const(c)
    }

    // ----- statements -----
    // lower_top_block / lower_stmts / lower_let / lower_equation / drive_result
    // walk the MIR block, reusing the id-agnostic helpers (declare_local,
    // local_leaves[_dir], push_assign, flatten_leaves) for the common paths and
    // dedicated handlers for the let-mut fold, reg, instance, when, record, and
    // place projections.

    fn lower_top_block(&mut self, block: &MBlock) {
        self.lower_stmts(&block.stmts);
        if let Some(tail) = block.tail {
            self.drive_result(tail);
        }
    }

    fn lower_stmts(&mut self, stmts: &[MStmt]) {
        let mut i = 0;
        while i < stmts.len() {
            // `let mut acc = init;` followed by the contiguous run of statements
            // that reassign `acc` (a straight-line `acc = …` or a carrying `for`)
            // is a loop-carried fold → one procedural `always_comb`.
            if let MStmt::Let { local, value } = &stmts[i]
                && self.mir.local(*local).mutable
            {
                let (acc, init) = (*local, *value);
                let mut steps: Vec<MStmt> = Vec::new();
                let mut j = i + 1;
                while let Some(stmt) = stmts.get(j) {
                    if !self.mir_carries(stmt, acc) {
                        break;
                    }
                    steps.push(stmt.clone());
                    j += 1;
                }
                if !steps.is_empty() {
                    self.lower_mut_fold(acc, init, &steps);
                    i = j;
                    continue;
                }
            }
            self.lower_one_stmt(&stmts[i]);
            i += 1;
        }
    }

    /// Does a statement reassign `acc` (a loop-carried fold step)?
    fn mir_carries(&self, stmt: &MStmt, acc: LocalId) -> bool {
        match stmt {
            MStmt::Equation { lhs, .. } => lhs.base == acc,
            MStmt::For { body, .. } => body
                .stmts
                .iter()
                .any(|s| matches!(s, MStmt::Equation { lhs, .. } if lhs.base == acc)),
            _ => false,
        }
    }

    /// `let mut acc` + carrying steps → one
    /// procedural `always_comb` (init, then blocking reassignments / a
    /// procedural `for`).
    fn lower_mut_fold(&mut self, acc: LocalId, init: MExprId, steps: &[MStmt]) {
        self.declare_local(acc);
        let mut comb: Vec<SvCombStmt> = Vec::new();
        let acc_leaves = self.local_leaves(acc);
        let init_leaves = self.expr_leaves(init);
        for ((_, lp), (_, rv)) in acc_leaves.into_iter().zip(init_leaves) {
            comb.push(SvCombStmt::Assign { lhs: lp, rhs: rv });
        }
        for step in steps {
            match step {
                MStmt::Equation { lhs, rhs } => {
                    for a in self.blocking_assigns(lhs, *rhs) {
                        comb.push(a);
                    }
                }
                MStmt::For {
                    index,
                    elem,
                    iter,
                    body,
                } => {
                    let Some((bound, var)) = self.loop_bound_var(*index, *elem, *iter) else {
                        continue;
                    };
                    let mut inner: Vec<SvCombStmt> = Vec::new();
                    let elem_is_genvar = matches!(self.mir.local(*elem).kind, LocalKind::ForBound);
                    if !elem_is_genvar {
                        self.declare_local(*elem);
                        let elem_base = self.local_name(*elem);
                        for (suffix, e) in self.expr_leaves(*iter) {
                            inner.push(SvCombStmt::Assign {
                                lhs: SvExpr::Ident(join(&elem_base, &suffix)),
                                rhs: SvExpr::Lit(format!("{e}[{var}]")),
                            });
                        }
                    }
                    let body = body.clone();
                    for stmt in &body.stmts {
                        if let MStmt::Equation { lhs, rhs } = stmt {
                            for a in self.blocking_assigns(lhs, *rhs) {
                                inner.push(a);
                            }
                        }
                    }
                    comb.push(SvCombStmt::For {
                        var,
                        bound,
                        body: inner,
                    });
                }
                _ => {}
            }
        }
        self.items
            .push(SvItem::AlwaysComb(SvAlwaysComb { body: comb }));
    }

    /// Per-leaf blocking assignments for one `lhs = rhs` (inside a fold).
    fn blocking_assigns(&mut self, lhs: &Place, rhs: MExprId) -> Vec<SvCombStmt> {
        let lhs_leaves = self.place_leaves_dir(lhs);
        let rhs_leaves = self.value_leaves_dir(rhs);
        lhs_leaves
            .into_iter()
            .zip(rhs_leaves)
            .map(|((lp, _), (rp, _))| SvCombStmt::Assign { lhs: lp, rhs: rp })
            .collect()
    }

    /// `(bound, genvar-name)` from a MIR iterable.
    fn loop_bound_var(
        &mut self,
        index: Option<LocalId>,
        elem: LocalId,
        iter: MExprId,
    ) -> Option<(SvExpr, String)> {
        let it = {
            let t = ground_widths(
                self.db,
                self.krate,
                self.def,
                &subst_type(&self.mir.expr(iter).ty, &self.self_subst),
            );
            self.subst_promoted(&t)
        };
        let len = match &it {
            Type::Vec { len, .. } => len.clone(),
            Type::Value {
                kind: ValueKind::Bits { width },
                ..
            } => width.clone(),
            _ => return None,
        };
        let bound = width_expr(&len, &self.sig.generic_params);
        let elem_is_genvar = matches!(self.mir.local(elem).kind, LocalKind::ForBound);
        let var = match (elem_is_genvar, index) {
            (true, _) => self.local_name(elem),
            (false, Some(i)) => self.local_name(i),
            (false, None) => {
                let v = format!("__i{}", self.synth);
                self.synth += 1;
                v
            }
        };
        Some((bound, var))
    }

    fn lower_one_stmt(&mut self, stmt: &MStmt) {
        match stmt {
            MStmt::Let { local, value } => self.lower_let(*local, *value),
            MStmt::VarDecl { local } => self.declare_local(*local),
            MStmt::Equation { lhs, rhs } => {
                // An integer (compile-time) bare-local drive is no hardware.
                if lhs.projections.is_empty() && self.is_integer_local(lhs.base) {
                    return;
                }
                let (lhs, rhs) = (lhs.clone(), *rhs);
                self.lower_equation(&lhs, rhs);
            }
            MStmt::Return { value } => self.drive_result(*value),
            MStmt::Expr(e) => self.lower_call_stmt(*e),
            MStmt::When { event, body, init } => {
                let (event, body) = (*event, body.clone());
                let init = init.clone();
                self.lower_when_stmt(event, &body, init.as_ref());
            }
            MStmt::For {
                index,
                elem,
                iter,
                body,
            } => {
                let (index, elem, iter) = (*index, *elem, *iter);
                let body = body.clone();
                self.lower_for(index, elem, iter, &body);
            }
        }
    }

    fn lower_let(&mut self, local: LocalId, value: MExprId) {
        if self.is_integer_local(local) {
            // A symbolic const local (e.g. `let w = f(N)`) that sizes a signal
            // becomes a `localparam`; a concrete one folds to literals (nothing).
            if self.is_symbolic_const(local) {
                let name = self.local_name(local);
                let val = self.const_rhs(value);
                self.items.push(SvItem::LocalParam {
                    name: name.clone(),
                    value: val,
                });
                self.promoted.insert(local, name);
            }
            return;
        }
        if let Some((d_input, reset, init)) = self.as_reg(value) {
            // A register is typed by its D-input.
            let leaves = self.expr_type_leaves(d_input);
            let base = self.local_name(local);
            let clock = self.clock_of_type(self.inf.local_type(local));
            self.emit_registers(&base, &leaves, d_input, reset, init, clock, true);
            return;
        }
        if self.is_instance_call(value) {
            // `let x = f(args)` — `x` is the callee's (flattened) result.
            self.declare_local(local);
            let target = self.local_leaves(local);
            self.emit_instance_from(value, target);
            return;
        }
        self.declare_local(local);
        let target = self.local_leaves(local);
        let value_leaves = self.expr_leaves(value);
        // Match by suffix, not position (a record's `=>` fields are absent).
        for (suf, place) in target {
            if let Some((_, v)) = value_leaves.iter().find(|(s, _)| *s == suf) {
                self.push_assign(place, v.clone());
            }
        }
        // A record value's `field => target` out-connections drive their targets
        // from the just-bound local's fields.
        let base = self.local_name(local);
        for (suf, target) in self.record_out_conns(value) {
            self.push_assign(target, SvExpr::Ident(join(&base, &suf)));
        }
    }

    fn lower_equation(&mut self, lhs: &Place, rhs: MExprId) {
        let bare_local = lhs.projections.is_empty();
        if bare_local && let Some((d_input, reset, init)) = self.as_reg(rhs) {
            let l = lhs.base;
            let leaves = self.local_type_leaves(l);
            let base = self.local_name(l);
            let clock = self.clock_of_type(self.inf.local_type(l));
            self.emit_registers(&base, &leaves, d_input, reset, init, clock, false);
            return;
        }
        // `mem = when E { … }` — the local IS the register: always_ff directly on
        // its leaves so `init mem = …` takes effect and the RAM stays one array.
        if bare_local && let MExprKind::When { event, body, init } = &self.mir.expr(rhs).kind {
            let (event, b, init) = (*event, body.clone(), *init);
            let l = lhs.base;
            let clock = self.clock_of_event(event);
            let base = self.local_name(l);
            if let Some(init) = init {
                let init_leaves = self.expr_leaves(init);
                let assigns = init_leaves
                    .into_iter()
                    .map(|(suffix, v)| (SvExpr::Ident(join(&base, &suffix)), v))
                    .collect();
                self.items.push(SvItem::Initial(assigns));
            }
            let d = self.block_leaves(&b);
            let clocked_body = d
                .into_iter()
                .map(|(suffix, dv)| SvSeqAssign::new(SvExpr::Ident(join(&base, &suffix)), dv))
                .collect();
            self.items.push(SvItem::AlwaysFf(SvAlwaysFf {
                clock,
                reset: None,
                reset_body: Vec::new(),
                clocked_body,
            }));
            return;
        }
        if bare_local && self.is_instance_call(rhs) {
            // `place = f(args)` — the callee's result drives `place`.
            let target = self.local_leaves(lhs.base);
            self.emit_instance_from(rhs, target);
            return;
        }
        // A bare-local target driven by a record: its `=>` fields drive their
        // targets, and (for a record RHS) the local's leaves are assigned
        // suffix-matched (`=>` fields are absent from the forward leaves).
        if bare_local {
            let base = self.local_name(lhs.base);
            for (suf, target) in self.record_out_conns(rhs) {
                self.push_assign(target, SvExpr::Ident(join(&base, &suf)));
            }
            if matches!(self.mir.expr(rhs).kind, MExprKind::Record { .. }) {
                let target = self.local_leaves(lhs.base);
                let value_leaves = self.expr_leaves(rhs);
                for (suf, place) in target {
                    if let Some((_, v)) = value_leaves.iter().find(|(s, _)| *s == suf) {
                        self.push_assign(place, v.clone());
                    }
                }
                return;
            }
        }
        // General: place leaves zipped with value leaves, direction-aware (the
        // body-driven leaf is the sink).
        let lhs_leaves = self.place_leaves_dir(lhs);
        let rhs_leaves = self.value_leaves_dir(rhs);
        for ((lp, ld), (rp, rd)) in lhs_leaves.into_iter().zip(rhs_leaves) {
            let (sink, src) = match (ld, rd) {
                (true, _) => (lp, rp),
                (false, true) => (rp, lp),
                (false, false) => (lp, rp),
            };
            self.push_assign(sink, src);
        }
    }

    fn drive_result(&mut self, value: MExprId) {
        let Some(rt) = self.sig.return_type.clone() else {
            // A unit fn whose tail/return is a side-effecting (void) call still
            // needs its instance — and the drives it carries — emitted.
            self.lower_call_stmt(value);
            return;
        };
        let rt = subst_type(&rt, &self.self_subst);
        let result_leaves = flatten_leaves(
            self.db,
            self.krate,
            self.def,
            &rt,
            true,
            &self.sig.generic_params,
        );
        // `return f(args)` — connect the callee's result straight to `result`.
        if self.is_instance_call(value) {
            let target = result_leaves
                .into_iter()
                .map(|l| (l.suffix.clone(), SvExpr::Ident(join("result", &l.suffix))))
                .collect();
            self.emit_instance_from(value, target);
            return;
        }
        let value_leaves = self.expr_leaves(value);
        for rl in result_leaves {
            if let Some((_, v)) = value_leaves.iter().find(|(s, _)| *s == rl.suffix) {
                let result_leaf = SvExpr::Ident(join("result", &rl.suffix));
                if rl.drives {
                    self.push_assign(result_leaf, v.clone());
                } else {
                    self.push_assign(v.clone(), result_leaf);
                }
            }
        }
        // A record return's `field => target` out-connections drive from result.
        for (suf, target) in self.record_out_conns(value) {
            self.push_assign(target, SvExpr::Ident(join("result", &suf)));
        }
    }

    /// The SV part-select range string for a slice/slice-set of base type
    /// `base_ty`. Offset form (`width` set) → the uniform indexed part-select
    /// `[off +: w]` (base may be runtime). Two-endpoint → type-directed:
    /// `bits` packed `[high-1:low]`, `Vec` unpacked ascending `[low:high-1]`;
    /// an elided end defaults from the base length. Shared by slice reads
    /// (`MExprKind::Slice`) and slice-sets (`Projection::BitRange`).
    fn slice_range_sv(
        &mut self,
        base_ty: &Type<'db>,
        lo: Option<MExprId>,
        hi: Option<MExprId>,
        width: Option<MExprId>,
    ) -> String {
        if let Some(w_e) = width {
            let off = self.expr_value(lo.expect("slice offset base"));
            let w = self.render_const(&self.mir_const_arg(w_e));
            return format!("[{off} +: {w}]");
        }
        // Two-endpoint, **ascending (low-first)** for both `bits` and `Vec`
        // (decision 2026-06-26), emitted as the indexed part-select
        // `[low +: width]` uniformly (no `[msb:lo]` special case). An elided low
        // defaults to 0, an elided high to the base length `N`.
        let n = match base_ty {
            Type::Value {
                kind: ValueKind::Bits { width: n },
                ..
            } => n.clone(),
            Type::Vec { len: n, .. } => n.clone(),
            _ => panic!("MIR: slice base is neither bits nor vec"),
        };
        let low = lo
            .map(|e| self.mir_const_arg(e))
            .unwrap_or(ConstArg::Lit(0));
        let high = hi.map(|e| self.mir_const_arg(e)).unwrap_or(n);
        let width = self.render_const(&ConstArg::Op(
            ConstOp::Sub,
            Box::new(high),
            Box::new(low.clone()),
        ));
        let low_s = self.render_const(&low);
        format!("[{low_s} +: {width}]")
    }

    /// The (substituted) width `ConstArg` of a slice/slice-set, for this
    /// instantiation. Mirrors `slice_range_sv`'s width computation (ascending,
    /// low-first): the offset form's `width`, else `high - low` with elided ends
    /// from the base length. Returns `None` if the base is neither bits nor vec.
    fn slice_width_const(
        &self,
        base_ty: &Type<'db>,
        lo: Option<MExprId>,
        hi: Option<MExprId>,
        width: Option<MExprId>,
    ) -> Option<ConstArg<'db>> {
        let w = match width {
            Some(w_e) => self.mir_const_arg(w_e),
            None => {
                let n = match base_ty {
                    Type::Value {
                        kind: ValueKind::Bits { width: n },
                        ..
                    } => n.clone(),
                    Type::Vec { len: n, .. } => n.clone(),
                    _ => return None,
                };
                let low = lo
                    .map(|e| self.mir_const_arg(e))
                    .unwrap_or(ConstArg::Lit(0));
                let high = hi.map(|e| self.mir_const_arg(e)).unwrap_or(n);
                ConstArg::Op(ConstOp::Sub, Box::new(high), Box::new(low))
            }
        };
        Some(subst_const_opt(&w, &self.self_subst))
    }

    /// Does a slice/slice-set width fold to exactly 0 at this instantiation? Used
    /// by the compiler-applied zero guard on both sides: to skip a zero-width
    /// slice-set drive, and to emit a zero-width slice READ as the empty value.
    fn slice_width_is_zero(
        &self,
        base_ty: &Type<'db>,
        lo: Option<MExprId>,
        hi: Option<MExprId>,
        width: Option<MExprId>,
    ) -> bool {
        self.slice_width_const(base_ty, lo, hi, width)
            .and_then(|w| crate::hir::const_eval::eval_const(self.db, self.krate, self.def, &w))
            == Some(0)
    }

    /// Lower a SYMBOLIC-width slice read to a per-leaf `generate if` (the zero
    /// guard for an unknown width — planning/slice_guards.md). Each base leaf
    /// drives a fresh wire of the result-leaf type: the `width == 0` arm is the
    /// empty value (`'0` / `'{default:'0}`), the else arm the `[lo +: w]`
    /// part-select. SV §27.5 elaborates only the selected arm, so a parametric
    /// module instantiated at length 0 never elaborates the out-of-range slice.
    #[allow(clippy::too_many_arguments)]
    fn slice_generate(
        &mut self,
        base: MExprId,
        base_ty: &Type<'db>,
        result_ty: &Type<'db>,
        lo: Option<MExprId>,
        hi: Option<MExprId>,
        width: Option<MExprId>,
        w: Option<ConstArg<'db>>,
    ) -> Vec<(String, SvExpr)> {
        let range = self.slice_range_sv(base_ty, lo, hi, width);
        let base_leaves = self.expr_leaves(base);
        let rty = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(result_ty, &self.self_subst),
        );
        let result_leaves = flatten_leaves(
            self.db,
            self.krate,
            self.def,
            &rty,
            true,
            &self.sig.generic_params,
        );
        let cond = SvExpr::Lit(format!(
            "({} == 0)",
            w.map(|w| self.render_const(&w))
                .unwrap_or_else(|| "0".to_owned())
        ));
        let label = self.fresh_block();
        let mut then_items = Vec::new();
        let mut else_items = Vec::new();
        let mut out = Vec::new();
        for (i, (suffix, e)) in base_leaves.into_iter().enumerate() {
            let ty = result_leaves
                .get(i)
                .map(|l| l.ty.clone())
                .unwrap_or_else(SvType::bit);
            let name = join(&label, &suffix);
            self.items.push(SvItem::Logic(SvLogicDecl {
                ty: ty.clone(),
                name: name.clone(),
            }));
            then_items.push(SvItem::Assign {
                lhs: SvExpr::Ident(name.clone()),
                rhs: zero_value_for(&ty),
            });
            else_items.push(SvItem::Assign {
                lhs: SvExpr::Ident(name.clone()),
                rhs: SvExpr::Lit(format!("{e}{range}")),
            });
            out.push((suffix, SvExpr::Ident(name)));
        }
        self.items.push(SvItem::GenerateIf(SvGenerateIf {
            cond,
            label: format!("{label}__g"),
            then_items,
            else_items,
        }));
        out
    }

    /// The leaves of the empty/zero-width value of result type `ty` — the value of
    /// a zero-width slice (planning/slice_guards.md). Each flattened leaf is driven
    /// by its uniform zero: `'{default: '0}` for an unpacked-array (`Vec`) leaf —
    /// the array analog of `bits(0)`'s `'0` (a zero-length `Vec` is `[0:-1]`, a
    /// degenerate net that cannot be a part-select but IS fillable by a default
    /// pattern) — and `'0` for a scalar/packed leaf. The type is grounded through
    /// `self_subst` first so a per-instance zero length is seen.
    fn undefined_vec_leaves(&mut self, ty: &Type<'db>) -> Vec<(String, SvExpr)> {
        let ty = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(ty, &self.self_subst),
        );
        flatten_leaves(
            self.db,
            self.krate,
            self.def,
            &ty,
            true,
            &self.sig.generic_params,
        )
        .into_iter()
        .map(|leaf| (leaf.suffix, zero_value_for(&leaf.ty)))
        .collect()
    }

    /// A slice endpoint as a `ConstArg` for rendering. First try to **ground** it
    /// through the MIR const evaluator (so arithmetic, const `let`s, and const-fn
    /// calls all reduce — `v[w..]`, `v[n+1..]`, `v[f()..]`); if it stays symbolic
    /// (a const generic, possibly in arithmetic), lower its structural shape so
    /// `render_const` prints the parametric SV expression (and a mono copy grounds
    /// it via `self_subst`).
    fn mir_const_arg(&self, m: MExprId) -> ConstArg<'db> {
        if let Some(v) = crate::mir::const_eval::eval_int(self.db, self.krate, self.def, m) {
            return ConstArg::Lit(v);
        }
        self.mir_const_structural(m)
    }

    /// The structural symbolic lowering of a const-width MExpr to a `ConstArg`
    /// (`render_const_sv`'s input): const param, width arithmetic, field/assoc.
    /// A shape that is not a const width expression — notably a *symbolic* call
    /// (`Deferred`; bind it with a `let` first) — is a hard error.
    fn mir_const_structural(&self, m: MExprId) -> ConstArg<'db> {
        match &self.mir.expr(m).kind {
            MExprKind::Number(v, _) => ConstArg::Lit(*v),
            MExprKind::ConstParam(i) => ConstArg::Param(*i),
            MExprKind::ConstAssoc { item, self_ty } => ConstArg::Assoc {
                item: *item,
                self_ty: Box::new(self_ty.clone()),
            },
            MExprKind::Field { receiver, field } => ConstArg::Field(
                Box::new(self.mir_const_structural(*receiver)),
                field.clone(),
            ),
            MExprKind::Call {
                callee,
                receiver: Some(r),
                args,
                ..
            } => {
                let op = match self.map.def_data(*callee).map(|d| d.name.as_str()) {
                    Some("add") => ConstOp::Add,
                    Some("sub") => ConstOp::Sub,
                    Some("mul") => ConstOp::Mul,
                    Some("div") => ConstOp::Div,
                    Some("rem") => ConstOp::Rem,
                    _ => panic!("MIR: slice endpoint is not a const width expression"),
                };
                let [Conn::In(b)] = args.as_slice() else {
                    panic!("MIR: slice endpoint operator has unexpected arity");
                };
                ConstArg::Op(
                    op,
                    Box::new(self.mir_const_structural(*r)),
                    Box::new(self.mir_const_structural(*b)),
                )
            }
            _ => panic!("MIR: slice endpoint is not a const width expression"),
        }
    }

    /// Render a const arg as an SV constant string: ground it through the mono
    /// `self_subst` + promoted locals, then fold to a literal or render symbolic
    /// (a param as the emitted module's `#()` name).
    fn render_const(&self, c: &ConstArg<'db>) -> String {
        let c = subst_const_opt(c, &self.self_subst);
        let c = self.subst_promoted_const(&c);
        match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, &c) {
            Some(v) => v.to_string(),
            None => render_const_sv(&c, self.sig),
        }
    }

    /// `Some((d_input, reset, init))` if a MIR node is a `.reg(rst, init)` builtin.
    fn as_reg(&self, m: MExprId) -> Option<(MExprId, MExprId, MExprId)> {
        if let MExprKind::Builtin {
            method: BuiltinMethod::Reg,
            receiver,
            args,
        } = &self.mir.expr(m).kind
            && let [Conn::In(rst), Conn::In(init)] = args.as_slice()
        {
            return Some((*receiver, *rst, *init));
        }
        None
    }

    /// True if a MIR node is a non-inline user call (→ a module instance).
    fn is_instance_call(&self, m: MExprId) -> bool {
        if let MExprKind::Call { callee, substs, .. } = &self.mir.expr(m).kind {
            let (def, _) = self.mir_call_target(*callee, substs);
            return !self.splices_inline(def);
        }
        false
    }

    /// Mirrors the HIR discriminator on the
    /// *outermost* projection: a bare local fans out per leaf with its direction;
    /// an Index-rooted place fans out per base-leaf, indexed (all drive); a
    /// Field-rooted place is a single leaf (HIR's `expr_value` path). Runtime
    /// (uint) index bounds-asserts are NOT replicated here — the predicate keeps
    /// runtime-indexed places on HIR.
    fn place_leaves_dir(&mut self, place: &Place) -> Vec<(SvExpr, bool)> {
        match place.projections.last() {
            None => self.local_leaves_dir(place.base),
            Some(Projection::Index(_)) => self
                .projected_leaves(place)
                .into_iter()
                .map(|(_, e)| (e, true))
                .collect(),
            Some(Projection::Field(_)) => {
                let leaves = self.projected_leaves(place);
                let one = if leaves.len() == 1 {
                    leaves.into_iter().next().unwrap().1
                } else {
                    SvExpr::Lit(format!(
                        "/* mirin: non-scalar value ({} leaves) in scalar position */",
                        leaves.len()
                    ))
                };
                vec![(one, true)]
            }
            // Slice-set `x[a..b] = …`: each base leaf gets the part-select range
            // appended, all driven. Only a sole BitRange (bare-local base) is
            // supported (the predicate enforces it).
            Some(Projection::BitRange { lo, hi, width }) => {
                let (lo, hi, width) = (*lo, *hi, *width);
                let bt = self.mir.local(place.base).ty.clone();
                // Zero-width slice-set drives nothing — the compiler-applied dual
                // of the prelude read guard (a set is an lvalue, not a value, so it
                // can't be a prelude fn). Skip the illegal `[lo +: 0]` part-select
                // (planning/slice_guards.md Phase 2).
                if self.slice_width_is_zero(&bt, lo, hi, width) {
                    return Vec::new();
                }
                let range = self.slice_range_sv(&bt, lo, hi, width);
                self.local_leaves(place.base)
                    .into_iter()
                    .map(|(_, e)| (SvExpr::Lit(format!("{e}{range}")), true))
                    .collect()
            }
        }
    }

    /// The `(suffix, SvExpr)` leaves of a projected place: start from the base
    /// local's leaves, then apply each projection base→leaf (Field strips the
    /// field prefix; Index appends `[idx]`).
    fn projected_leaves(&mut self, place: &Place) -> Vec<(String, SvExpr)> {
        let mut leaves = self.local_leaves(place.base);
        for proj in &place.projections {
            match proj {
                Projection::Field(f) => {
                    leaves = leaves
                        .into_iter()
                        .filter_map(|(suf, e)| strip_field(&suf, f).map(|rest| (rest, e)))
                        .collect();
                }
                Projection::Index(i) => {
                    let idx = self.expr_value(*i);
                    leaves = leaves
                        .into_iter()
                        .map(|(suf, e)| (suf, SvExpr::Lit(format!("{e}[{idx}]"))))
                        .collect();
                }
                // A sole BitRange is handled directly in `place_leaves_dir`;
                // a BitRange composed with other projections is not supported
                // (the predicate keeps it on HIR).
                Projection::BitRange { .. } => {
                    panic!("MIR: BitRange is only supported as a sole place projection")
                }
            }
        }
        leaves
    }

    /// A local carries its direction; anything
    /// else flattens as a driven source.
    fn value_leaves_dir(&mut self, m: MExprId) -> Vec<(SvExpr, bool)> {
        if let MExprKind::Local(l) = self.mir.expr(m).kind {
            return self.local_leaves_dir(l);
        }
        self.expr_leaves(m)
            .into_iter()
            .map(|(_, v)| (v, true))
            .collect()
    }

    /// Declare a `logic` for each of a local's leaves, once per local.
    fn declare_local(&mut self, local: LocalId) {
        // The result place's leaves are the module's result ports, already
        // declared from the signature — never a fresh net.
        if self.is_result_local(local) {
            return;
        }
        if self.is_integer_local(local) || !self.declared.insert(local) {
            return;
        }
        let base = self.local_name(local);
        for leaf in self.local_type_leaves(local) {
            self.items.push(SvItem::Logic(SvLogicDecl {
                ty: leaf.ty,
                name: join(&base, &leaf.suffix),
            }));
        }
    }

    fn push_assign(&mut self, lhs: SvExpr, rhs: SvExpr) {
        self.items.push(SvItem::Assign { lhs, rhs });
    }

    /// A local's SV name (uniquified). A result place (`return`, a named result,
    /// or a named tuple part) emits as its result ports — leaves that ARE the
    /// module's result, declared from the signature. Its base is `result` (or
    /// `result__0`/… for a tuple part), never the source name (`return` is an SV
    /// reserved word; a user name shouldn't rename the port) — see
    /// planning/return_variable.md.
    fn local_name(&self, local: LocalId) -> String {
        if let Some(base) = &self.body.local(local).result_base {
            return base.clone();
        }
        format!("{}{}", self.prefix, self.local_names[local.0 as usize])
    }

    /// A result place — carries an SV result base.
    fn is_result_local(&self, local: LocalId) -> bool {
        self.body.local(local).result_base.is_some()
    }

    /// A local's type: inferred, falling back to declared. A `self` param's
    /// type comes from the signature (the impl's self type, applied at the
    /// binder's generics).
    fn local_ty(&self, local: LocalId) -> Option<Type<'db>> {
        let base = if let Some(p) = self
            .sig
            .params
            .iter()
            .find(|p| p.local == local && p.is_self)
        {
            Some(p.ty.clone())
        } else {
            self.inf
                .local_type(local)
                .cloned()
                .or_else(|| self.body.local(local).declared_ty.clone())
        };
        base.map(|t| {
            let t = subst_type(&t, &self.self_subst);
            let t = ground_widths(self.db, self.krate, self.def, &t);
            // A width that survives grounding as a promoted const local renders
            // against its `localparam` name (`uint(w)` → `[w-1:0]`).
            self.subst_promoted(&t)
        })
    }

    /// The index/base types come from the MIR nodes.
    fn index_bounds_assert(&mut self, base: MExprId, index: MExprId, idx_sv: &SvExpr) {
        let (it, bt) = (
            self.mir.expr(index).ty.clone(),
            self.mir.expr(base).ty.clone(),
        );
        self.index_bounds_assert_tys(&it, &bt, idx_sv);
    }

    /// Id-agnostic core: a dynamic (uint) index gets a simulation-time bounds
    /// assert unless its width provably cannot exceed the length.
    fn index_bounds_assert_tys(&mut self, it: &Type<'db>, bt: &Type<'db>, idx_sv: &SvExpr) {
        let it = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(it, &self.self_subst),
        );
        let Type::Value {
            kind: ValueKind::UInt { width: iw },
            ..
        } = it
        else {
            return; // static (integer/literal) indexes are checked in infer
        };
        let bt = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(bt, &self.self_subst),
        );
        let len = match bt {
            Type::Vec { len, .. } => len,
            Type::Value {
                kind: ValueKind::Bits { width },
                ..
            } => width,
            _ => return,
        };
        // Provably in range: every expressible value is a valid element.
        if let (ConstArg::Lit(w), ConstArg::Lit(n)) = (&iw, &len)
            && *w < 127
            && (1i128 << *w) <= *n
        {
            return;
        }
        // The bound renders like any width/length (literal, param, or symbolic
        // compound expr) — `render_const_sv` covers all three, never a silent
        // `0` default that would weaken the bounds check.
        let len_sv = render_const_sv(&len, self.sig);
        let cond = format!("{idx_sv} < {len_sv}");
        if self.index_asserts.insert(cond.clone()) {
            self.items.push(SvItem::CombAssert(SvCombAssert { cond }));
        }
    }

    /// The ground (literal) bit-width of a type, with the "prefer hex" flag, if
    /// it has one. Id-agnostic: the caller resolves a `Type`, then asks this.
    /// Applies `self_subst` + `ground_widths` (the type is not mono-ground on
    /// its own).
    fn width_of_ty(&self, t: &Type<'db>) -> Option<(u32, bool)> {
        let t = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(t, &self.self_subst),
        );
        match t {
            Type::Value {
                kind:
                    ValueKind::UInt {
                        width: ConstArg::Lit(w),
                    },
                ..
            } if (1..=4096).contains(&w) => Some((w as u32, false)),
            Type::Value {
                kind:
                    ValueKind::Bits {
                        width: ConstArg::Lit(w),
                    },
                ..
            } if (1..=4096).contains(&w) => Some((w as u32, true)),
            _ => None,
        }
    }

    /// Resolve a trait-method decl to its concrete impl, taking the recorded decl
    /// substitution directly (id-agnostic — the caller passes a `Call` node's
    /// `substs`).
    fn resolve_trait_instance_with(
        &self,
        decl: DefId<'db>,
        decl_subst: &[Term<'db>],
    ) -> Option<(DefId<'db>, Vec<Option<Term<'db>>>)> {
        if !self.map.is_trait_method_decl(decl) {
            return None;
        }
        let trait_def = self.map.def_data(decl)?.owner?;
        let Some(Term::Type(self_ty)) = decl_subst.first() else {
            return None;
        };
        let opts: Vec<Option<Term<'db>>> = self.self_subst.clone();
        let self_ty = ground_widths(self.db, self.krate, self.def, &subst_type(self_ty, &opts));
        let head = type_head_def(self.map, &self_ty)?;
        let mname = self.map.def_data(decl)?.name.clone();
        for data in self.map.trait_impls(trait_def) {
            if data.self_def != head {
                continue;
            }
            let hsig = sig_of(self.db, self.krate, data.impl_def);
            let Some(header) = &hsig.return_type else {
                continue;
            };
            let mut binding = vec![None; hsig.generic_params.len()];
            if !crate::hir::types::match_header(&self_ty, header, &mut binding) {
                continue;
            }
            let method = data.methods.iter().find(|(n, _)| *n == mname)?.1;
            // Compose: binder prefix from the header match, then the decl's
            // own generics (everything after its implicit Self), mapped into
            // this module's value space.
            let mut composed = binding;
            for t in &decl_subst[1..] {
                composed.push(Some(subst_term(t, &opts)));
            }
            return Some((method, composed));
        }
        None
    }

    /// Resolve a MIR `Call` node's callee to the concrete instance: a trait
    /// method DECL re-selects to its impl (with the composed subst override);
    /// anything else is itself. The MIR analogue of the
    /// `resolve_trait_instance` step the HIR call paths do.
    fn mir_call_target(
        &self,
        callee: DefId<'db>,
        substs: &[Term<'db>],
    ) -> (DefId<'db>, Option<Vec<Option<Term<'db>>>>) {
        match self.resolve_trait_instance_with(callee, substs) {
            Some((m, ov)) => (m, Some(ov)),
            None => (callee, None),
        }
    }

    /// `integer` values are compile-time only — they never become hardware.
    /// Locals of integer type get no `logic`, no assigns, and a call whose
    /// connections are all integers gets no instance (its results are reached
    /// by `const_eval` through the width trees instead).
    fn is_integer_local(&self, local: LocalId) -> bool {
        self.local_ty(local)
            .is_some_and(|t| self.is_const_only_ty(&t))
    }

    /// `integer`, or a struct whose every field is const-only (a config
    /// record) — values with no hardware representation.
    fn is_const_only_ty(&self, ty: &Type<'db>) -> bool {
        match ty {
            Type::Value {
                kind: ValueKind::Integer,
                ..
            } => true,
            // A `struct` whose every field is const-only is a config record. A
            // `port` is always a hardware boundary, so it is never const-only —
            // gate on the def's `DefKind` (structs_as_ports.md).
            Type::Port { def, .. }
                if self.map.def_data(*def).map(|d| d.kind) == Some(DefKind::Struct) =>
            {
                let sig = sig_of(self.db, self.krate, *def);
                !sig.fields.is_empty() && sig.fields.iter().all(|f| self.is_const_only_ty(&f.ty))
            }
            _ => false,
        }
    }

    /// The scalar leaf types of a MIR node.
    fn expr_type_leaves(&self, m: MExprId) -> Vec<Leaf> {
        match &self.mir.expr(m).kind {
            MExprKind::Local(l) => self.local_type_leaves(*l),
            MExprKind::Field { receiver, field } => {
                let (receiver, field) = (*receiver, field.clone());
                self.expr_type_leaves(receiver)
                    .into_iter()
                    .filter_map(|leaf| {
                        strip_field(&leaf.suffix, &field).map(|rest| Leaf {
                            suffix: rest,
                            ..leaf
                        })
                    })
                    .collect()
            }
            MExprKind::Record { ctor, .. } => {
                match ctor.and_then(|c| self.map.def_data(c).and_then(|d| d.owner)) {
                    Some(owner) => flatten_leaves(
                        self.db,
                        self.krate,
                        self.def,
                        &Type::Port {
                            def: owner,
                            args: GenericArgs(Vec::new()),
                            domain: Domain::Unspecified,
                        },
                        true,
                        &self.sig.generic_params,
                    ),
                    None => vec![Leaf {
                        suffix: String::new(),
                        ty: SvType::bit(),
                        drives: true,
                    }],
                }
            }
            _ => vec![Leaf {
                suffix: String::new(),
                ty: self.sv_type_of(&self.mir.expr(m).ty),
                drives: true,
            }],
        }
    }

    /// The scalar leaves of a local's type (scalar → one bit-typed leaf).
    fn local_type_leaves(&self, local: LocalId) -> Vec<Leaf> {
        match self.local_ty(local) {
            Some(t) => flatten_leaves(
                self.db,
                self.krate,
                self.def,
                &t,
                true,
                &self.sig.generic_params,
            ),
            None => vec![Leaf {
                suffix: String::new(),
                ty: SvType::bit(),
                drives: true,
            }],
        }
    }

    /// A local's leaves as `(suffix, place-ident)` value expressions.
    fn local_leaves(&self, local: LocalId) -> Vec<(String, SvExpr)> {
        let base = self.local_name(local);
        self.local_type_leaves(local)
            .into_iter()
            .map(|leaf| {
                (
                    leaf.suffix.clone(),
                    SvExpr::Ident(join(&base, &leaf.suffix)),
                )
            })
            .collect()
    }

    /// A local's leaves as `(place-ident, drives)`, where `drives` folds the
    /// local's own direction (an `out` param drives; an `in` param reads) with
    /// each port field's direction — used to pick an equation's sink.
    fn local_leaves_dir(&self, local: LocalId) -> Vec<(SvExpr, bool)> {
        let base = self.local_name(local);
        let drives = self.local_base_drives(local);
        match self.local_ty(local) {
            Some(t) => flatten_leaves(
                self.db,
                self.krate,
                self.def,
                &t,
                drives,
                &self.sig.generic_params,
            )
            .into_iter()
            .map(|leaf| (SvExpr::Ident(join(&base, &leaf.suffix)), leaf.drives))
            .collect(),
            None => vec![(SvExpr::Ident(base), drives)],
        }
    }

    /// Does the body drive this local? An `out` value param does; everything
    /// else (an `in`/undirected param, a `let`/`var`) is read or internally
    /// driven, so it defaults to `true` (LHS-sink) for non-port equations.
    fn local_base_drives(&self, local: LocalId) -> bool {
        self.sig
            .params
            .iter()
            .find(|p| p.local == local)
            .map_or(true, |p| p.direction == Some(Direction::Out))
    }

    /// A type's SV type (ground widths, then lower). Id-agnostic: the caller
    /// resolves the `Type`, this grounds its widths and lowers it.
    fn sv_type_of(&self, ty: &Type<'db>) -> SvType {
        sv_type(
            &ground_widths(self.db, self.krate, self.def, ty),
            &self.sig.generic_params,
        )
    }

    /// Id-agnostic core of `emit_registers`: emit one reset/clocked `always_ff`
    /// per leaf from the already-resolved reset name and D/init leaves. Shared by
    /// fed by `emit_registers`.
    #[allow(clippy::too_many_arguments)]
    fn emit_registers_parts(
        &mut self,
        base: &str,
        leaves: &[Leaf],
        reset: String,
        d: Vec<(String, SvExpr)>,
        init: Vec<(String, SvExpr)>,
        clock: String,
        declare: bool,
    ) {
        for (i, leaf) in leaves.iter().enumerate() {
            let name = join(base, &leaf.suffix);
            if declare {
                self.items.push(SvItem::Logic(SvLogicDecl {
                    ty: leaf.ty.clone(),
                    name: name.clone(),
                }));
            }
            let zero = || SvExpr::Lit("0".to_owned());
            let d_in = d.get(i).map(|(_, e)| e.clone()).unwrap_or_else(zero);
            let init_v = init.get(i).map(|(_, e)| e.clone()).unwrap_or_else(zero);
            self.items.push(SvItem::AlwaysFf(SvAlwaysFf {
                clock: clock.clone(),
                reset: Some(reset.clone()),
                reset_body: vec![SvSeqAssign::new(SvExpr::Ident(name.clone()), init_v)],
                clocked_body: vec![SvSeqAssign::new(SvExpr::Ident(name), d_in)],
            }));
        }
    }

    /// Resolve the reset name and D/init leaves
    /// from MIR nodes, then share `emit_registers_parts`.
    #[allow(clippy::too_many_arguments)]
    fn emit_registers(
        &mut self,
        base: &str,
        leaves: &[Leaf],
        d_input: MExprId,
        reset: MExprId,
        init: MExprId,
        clock: String,
        declare: bool,
    ) {
        let reset = match self.expr_value(reset) {
            SvExpr::Ident(s) => s,
            other => other.to_string(),
        };
        let d = self.expr_leaves(d_input);
        let init = self.expr_leaves(init);
        self.emit_registers_parts(base, leaves, reset, d, init, clock, declare);
    }

    fn fresh_block(&mut self) -> String {
        let n = self.synth;
        self.synth += 1;
        format!("{}__block_{n}", self.prefix)
    }

    /// Lower a MIR node in scalar value position to one SV expression. Cases this
    /// cannot reduce to a scalar (a non-reg `Builtin`, `const if`) `todo!` in
    /// negative-space style, so the gap is explicit rather than a silent default.
    fn expr_value(&mut self, m: MExprId) -> SvExpr {
        match &self.mir.expr(m).kind {
            // A literal emits in its source base; sized form when its type has a
            // ground width (`8'hFF`). MIR folded TypedLiteral into Number.
            MExprKind::Number(n, base) => {
                let (n, base) = (*n, *base);
                let ty = self.mir.expr(m).ty.clone();
                SvExpr::Lit(render_literal(n, base, self.width_of_ty(&ty)))
            }
            MExprKind::Bool(b) => SvExpr::Lit(if *b { "1'b1" } else { "1'b0" }.to_owned()),
            MExprKind::Local(l) => SvExpr::Ident(self.local_name(*l)),
            // A const generic used as a value. In a splice the active subst binds
            // it to a caller-frame value (`caller_const` made it a `Lit`/`Symbol`),
            // so render that; otherwise (top-level / parametric module) it is the
            // module's own `#(…)` parameter name. An out-of-range index with no
            // binding is a leaked foreign param — surface it loudly.
            MExprKind::ConstParam(i) => {
                match self.self_subst.get(*i as usize).and_then(Option::as_ref) {
                    Some(Term::Const(c)) => SvExpr::Lit(self.render_const(c)),
                    _ => match self.sig.generic_params.get(*i as usize) {
                        Some(g) => SvExpr::Ident(g.name.clone()),
                        None => panic!(
                            "ConstParam({i}) indexes no generic of the emitted module on \
                             a clean crate — a foreign param leaked into rendering."
                        ),
                    },
                }
            }
            // `A::bit_size`: ground Self with the instance subst, then evaluate.
            MExprKind::ConstAssoc { item, self_ty } => {
                let (item, self_ty) = (*item, self_ty.clone());
                let self_ty = ground_widths(
                    self.db,
                    self.krate,
                    self.def,
                    &subst_type(&self_ty, &self.self_subst),
                );
                let c = ConstArg::Assoc {
                    item,
                    self_ty: Box::new(self_ty),
                };
                match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, &c) {
                    Some(v) => SvExpr::Lit(v.to_string()),
                    None => SvExpr::Lit(render_const_sv(&c, self.sig)),
                }
            }
            MExprKind::Missing => SvExpr::Lit("0".to_owned()),
            MExprKind::Call {
                callee,
                substs,
                receiver,
                args,
                named,
            } => {
                let (callee, substs) = (*callee, substs.clone());
                let (receiver, args, named) = (*receiver, args.clone(), named.clone());
                let (def, ov) = self.mir_call_target(callee, &substs);
                if self.splices_inline(def) {
                    self.inline_call_leaves(def, &substs, ov.as_deref(), receiver, &args, &named)
                        .into_iter()
                        .next()
                        .map(|(_, e)| e)
                        .unwrap_or_else(|| SvExpr::Lit("0".to_owned()))
                } else {
                    // A user call in scalar position: instantiate, take one leaf.
                    self.call_value_leaves(m)
                        .into_iter()
                        .next()
                        .map(|(_, e)| e)
                        .unwrap_or_else(|| SvExpr::Lit("0".to_owned()))
                }
            }
            // `e.reg(rst, init)` in value position: a register into a fresh local.
            MExprKind::Builtin {
                method: BuiltinMethod::Reg,
                ..
            } if self.as_reg(m).is_some() => {
                let (d_input, reset, init) = self.as_reg(m).unwrap();
                let synth = self.fresh_block();
                let ty = self.sv_type_of(&self.mir.expr(m).ty.clone());
                self.items.push(SvItem::Logic(SvLogicDecl {
                    ty,
                    name: synth.clone(),
                }));
                let clock = self.clock_of_type(Some(&self.mir.expr(d_input).ty.clone()));
                let leaf = Leaf {
                    suffix: String::new(),
                    ty: SvType::bit(),
                    drives: true,
                };
                self.emit_registers(
                    &synth,
                    std::slice::from_ref(&leaf),
                    d_input,
                    reset,
                    init,
                    clock,
                    false,
                );
                SvExpr::Ident(synth)
            }
            // posedge/replace/enumerate are not value-position scalars.
            MExprKind::Builtin { .. } => {
                todo!("expr_value: non-reg Builtin in scalar position")
            }
            // `v[i]` in scalar position.
            MExprKind::Index { base, index } => {
                let (base, index) = (*base, *index);
                let b = self.expr_value(base);
                let i = self.expr_value(index);
                self.index_bounds_assert(base, index, &i);
                SvExpr::Lit(format!("{b}[{i}]"))
            }
            MExprKind::When { event, body, init } => {
                let (event, b, init) = (*event, body.clone(), *init);
                self.lower_when(m, event, &b, init)
            }
            MExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let (cond, tb, eb) = (*cond, then_branch.clone(), else_branch.clone());
                self.lower_if(m, cond, &tb, &eb)
            }
            // A `const if` that survived `mir_of` unfolded (its generics were
            // symbolic there) — fold it here against the active subst's const
            // generics. At an inline splice `self.self_subst` is the call's
            // composed subst, so a guard like `const if w == 0` grounds at the
            // call site (planning/slice_guards.md). Still-symbolic ⇒ generate-if
            // (Phase 4), not yet built.
            MExprKind::ConstIf {
                cond,
                then_branch,
                else_branch,
            } => {
                let (cond, tb, eb) = (*cond, then_branch.clone(), else_branch.clone());
                match self.eval_mir_cond(cond) {
                    Some(true) => self.block_value(&tb),
                    Some(false) => self.block_value(&eb),
                    // Symbolic condition (a const generic riding as a `#()` param):
                    // lower to a conditional generate driving a fresh wire.
                    None => self.const_if_generate(m, cond, &tb, &eb),
                }
            }
            // A slice in scalar position (a `bits` slice — one leaf). Vec slices
            // are aggregates and go through `expr_leaves`.
            MExprKind::Slice { .. } => self.one_leaf(m),
            MExprKind::Block(b) => {
                let b = b.clone();
                self.block_value(&b)
            }
            // An aggregate/field/record in scalar position reduces to its single
            // leaf if it has one (`one_leaf` emits a failing marker, never a
            // silent `0`, when it flattens to several).
            MExprKind::VecLit(_)
            | MExprKind::TupleLit(_)
            | MExprKind::VecRepeat { .. }
            | MExprKind::Field { .. }
            | MExprKind::Record { .. } => self.one_leaf(m),
            // A bare `Def` in value position is not a value (matches the HIR
            // path's defensive `0`); never reached on a clean body.
            MExprKind::Def(_) => SvExpr::Lit("0".to_owned()),
        }
    }

    /// The scalar leaves of a (possibly aggregate) MIR node, each tagged with its
    /// `__`-suffix. Aggregate arms reuse the id-agnostic helpers (`local_leaves`,
    /// `strip_field`, `eval_const`); calls, control flow, `replace`/`reg`, and
    /// records have dedicated arms.
    fn expr_leaves(&mut self, m: MExprId) -> Vec<(String, SvExpr)> {
        match &self.mir.expr(m).kind {
            MExprKind::Local(l) => self.local_leaves(*l),
            MExprKind::Field { receiver, field } => {
                let (receiver, field) = (*receiver, field.clone());
                self.expr_leaves(receiver)
                    .into_iter()
                    .filter_map(|(suf, e)| strip_field(&suf, &field).map(|rest| (rest, e)))
                    .collect()
            }
            MExprKind::VecLit(elems) => {
                let elems = elems.clone();
                let per_elem: Vec<Vec<(String, SvExpr)>> =
                    elems.iter().map(|e| self.expr_leaves(*e)).collect();
                let Some(first) = per_elem.first() else {
                    return vec![(String::new(), SvExpr::Lit("'{}".to_owned()))];
                };
                first
                    .iter()
                    .enumerate()
                    .map(|(li, (suffix, _))| {
                        let parts: Vec<String> = per_elem
                            .iter()
                            .map(|leaves| {
                                leaves
                                    .get(li)
                                    .map(|(_, e)| e.to_string())
                                    .unwrap_or_else(|| "0".to_owned())
                            })
                            .collect();
                        (
                            suffix.clone(),
                            SvExpr::Lit(format!("'{{{}}}", parts.join(", "))),
                        )
                    })
                    .collect()
            }
            MExprKind::TupleLit(elems) => {
                let elems = elems.clone();
                let mut out = Vec::new();
                for (i, e) in elems.iter().enumerate() {
                    for (suf, v) in self.expr_leaves(*e) {
                        out.push((join(&i.to_string(), &suf), v));
                    }
                }
                out
            }
            MExprKind::VecRepeat { elem, len } => {
                let (elem, len) = (*elem, len.clone());
                let n =
                    match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, &len) {
                        Some(v) => v.to_string(),
                        None => render_const_sv(&len, self.sig),
                    };
                self.expr_leaves(elem)
                    .into_iter()
                    .map(|(suffix, e)| (suffix, SvExpr::Lit(format!("'{{{n}{{{e}}}}}"))))
                    .collect()
            }
            MExprKind::Record { ctor, fields } => {
                let (ctor, fields) = (*ctor, fields.clone());
                self.record_leaves(ctor, &fields)
            }
            MExprKind::Index { base, index } => {
                let (base, index) = (*base, *index);
                let idx = self.expr_value(index);
                self.index_bounds_assert(base, index, &idx);
                self.expr_leaves(base)
                    .into_iter()
                    .map(|(suffix, e)| (suffix, SvExpr::Lit(format!("{e}[{idx}]"))))
                    .collect()
            }
            MExprKind::Call {
                callee,
                substs,
                receiver,
                args,
                named,
            } => {
                let (callee, substs) = (*callee, substs.clone());
                let (receiver, args, named) = (*receiver, args.clone(), named.clone());
                let (def, ov) = self.mir_call_target(callee, &substs);
                if self.splices_inline(def) {
                    self.inline_call_leaves(def, &substs, ov.as_deref(), receiver, &args, &named)
                } else {
                    self.call_value_leaves(m)
                }
            }
            // `v.replace(i, x)` — a combinational copy with element i swapped
            // (`__repl = v; __repl[i] = x;` per leaf).
            MExprKind::Builtin {
                method: BuiltinMethod::Replace,
                receiver,
                args,
            } if args.len() == 2 => {
                let receiver = *receiver;
                let (Conn::In(i_e), Conn::In(x_e)) = (&args[0], &args[1]) else {
                    return Vec::new();
                };
                let (i_e, x_e) = (*i_e, *x_e);
                let synth = self.fresh_block();
                let idx = self.expr_value(i_e);
                self.index_bounds_assert(receiver, i_e, &idx);
                let recv_leaves = self.expr_leaves(receiver);
                let x_leaves = self.expr_leaves(x_e);
                let tys = self.expr_type_leaves(receiver);
                let mut out = Vec::new();
                let mut body = Vec::new();
                for (k, (suffix, rv)) in recv_leaves.into_iter().enumerate() {
                    let name = join(&synth, &suffix);
                    let ty = tys.get(k).map(|l| l.ty.clone()).unwrap_or_else(SvType::bit);
                    self.items.push(SvItem::Logic(SvLogicDecl {
                        ty,
                        name: name.clone(),
                    }));
                    body.push(SvCombStmt::Assign {
                        lhs: SvExpr::Ident(name.clone()),
                        rhs: rv,
                    });
                    let xv = x_leaves
                        .get(k)
                        .map(|(_, e)| e.clone())
                        .unwrap_or_else(|| SvExpr::Lit("0".to_owned()));
                    body.push(SvCombStmt::Assign {
                        lhs: SvExpr::Lit(format!("{name}[{idx}]")),
                        rhs: xv,
                    });
                    out.push((suffix, SvExpr::Ident(name)));
                }
                self.items.push(SvItem::AlwaysComb(SvAlwaysComb { body }));
                out
            }
            // A scalar builtin in leaf position (a `reg`) — one leaf via the value
            // twin (which handles the register synthesis).
            MExprKind::Builtin { .. } => vec![(String::new(), self.expr_value(m))],
            MExprKind::ConstIf {
                cond,
                then_branch,
                else_branch,
            } => {
                let (cond, tb, eb) = (*cond, then_branch.clone(), else_branch.clone());
                match self.eval_mir_cond(cond) {
                    Some(true) => self.block_leaves(&tb),
                    Some(false) => self.block_leaves(&eb),
                    // Symbolic ⇒ a conditional generate. Scalar result only for
                    // now (one leaf via the value twin); an aggregate-result
                    // symbolic `const if` is a future extension.
                    None => vec![(String::new(), self.const_if_generate(m, cond, &tb, &eb))],
                }
            }
            MExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let (cond, tb, eb) = (*cond, then_branch.clone(), else_branch.clone());
                self.lower_if_leaves(m, cond, &tb, &eb)
            }
            MExprKind::When { event, body, init } => {
                let (event, b, init) = (*event, body.clone(), *init);
                self.lower_when_leaves(m, event, &b, init)
            }
            MExprKind::Block(b) => {
                let b = b.clone();
                self.block_leaves(&b)
            }
            // A slice, per base leaf → an indexed part-select `[lo +: w]`
            // (ascending/low-first for both bits and Vec). Width-directed by the
            // zero guard (planning/slice_guards.md): a width that grounds to 0 is
            // the empty value (`'0` / `'{default:'0}`, no illegal `[lo +: 0]`); a
            // SYMBOLIC width emits a `generate if` so the zero case is the empty
            // value and the nonzero case the part-select (a parametric module
            // instantiated at length 0 would otherwise be an out-of-range slice).
            MExprKind::Slice {
                base,
                lo,
                hi,
                width,
            } => {
                let (base, lo, hi, width) = (*base, *lo, *hi, *width);
                let bt = self.mir.expr(base).ty.clone();
                let rty = self.mir.expr(m).ty.clone();
                let w = self.slice_width_const(&bt, lo, hi, width);
                let w_val = w.as_ref().and_then(|w| {
                    crate::hir::const_eval::eval_const(self.db, self.krate, self.def, w)
                });
                match w_val {
                    // Ground zero: the empty value (no part-select).
                    Some(0) => self.undefined_vec_leaves(&rty),
                    // Ground nonzero: the plain part-select.
                    Some(_) => {
                        let range = self.slice_range_sv(&bt, lo, hi, width);
                        self.expr_leaves(base)
                            .into_iter()
                            .map(|(suffix, e)| (suffix, SvExpr::Lit(format!("{e}{range}"))))
                            .collect()
                    }
                    // Symbolic width: guard each leaf with a `generate if` so the
                    // zero instantiation is total.
                    None => self.slice_generate(base, &bt, &rty, lo, hi, width, w),
                }
            }
            // Scalars: a single empty-suffix leaf via the value twin.
            MExprKind::Number(..)
            | MExprKind::Bool(_)
            | MExprKind::ConstParam(_)
            | MExprKind::ConstAssoc { .. }
            | MExprKind::Def(_)
            | MExprKind::Missing => vec![(String::new(), self.expr_value(m))],
        }
    }

    /// Reduce a MIR node to a single scalar SV leaf.
    fn one_leaf(&mut self, m: MExprId) -> SvExpr {
        let mut leaves = self.expr_leaves(m);
        if leaves.len() == 1 {
            leaves.pop().unwrap().1
        } else {
            SvExpr::Lit(format!(
                "/* mirin: non-scalar value ({} leaves) in scalar position */",
                leaves.len()
            ))
        }
    }

    /// The in-field leaves in declared field order.
    fn record_leaves(
        &mut self,
        ctor: Option<DefId<'db>>,
        fields: &[crate::mir::ir::MRecordField],
    ) -> Vec<(String, SvExpr)> {
        let owner = ctor.and_then(|c| self.map.def_data(c).and_then(|d| d.owner));
        let Some(owner) = owner else {
            return vec![(String::new(), SvExpr::Lit("0".to_owned()))];
        };
        let order: Vec<String> = sig_of(self.db, self.krate, owner)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let mut out = Vec::new();
        for fname in &order {
            if let Some(rf) = fields.iter().find(|rf| &rf.name == fname)
                && let Conn::In(e) = &rf.conn
            {
                for (suf, ev) in self.expr_leaves(*e) {
                    out.push((join(fname, &suf), ev));
                }
            }
        }
        out
    }

    /// Each `field => target` as
    /// `(field_suffix, target_place_leaf)`.
    fn record_out_conns(&mut self, m: MExprId) -> Vec<(String, SvExpr)> {
        let MExprKind::Record { fields, .. } = &self.mir.expr(m).kind else {
            return Vec::new();
        };
        let fields = fields.clone();
        let mut out = Vec::new();
        for rf in &fields {
            if let Conn::Out(place) = &rf.conn {
                for (tsuf, target) in self.projected_leaves(place) {
                    out.push((join(&rf.name, &tsuf), target));
                }
            }
        }
        out
    }

    // ----- control flow -----

    /// Fold a `const if` condition (a MIR expr) against the active subst's const
    /// generics. At an inline splice `self.self_subst` is the call's composed
    /// subst, so the guard grounds at the call site; `None` ⇒ still symbolic
    /// (generate-if, not yet built).
    fn eval_mir_cond(&self, cond: MExprId) -> Option<bool> {
        crate::mir::const_eval::eval_cond_with(
            self.db,
            self.krate,
            self.def,
            cond,
            &self.self_subst,
        )
    }

    /// Lower a `const if` whose condition is still **symbolic** at emit (a const
    /// generic riding as a `#()` parameter) to an `SvItem::GenerateIf` driving a
    /// fresh wire — SV §27.5 elaborates only the selected block, so the dead arm's
    /// (possibly out-of-range) constructs never exist (planning/slice_guards.md
    /// Phase 4). Scalar result: each branch drives one wire. Each branch's own
    /// items are captured into its generate block (not hoisted to module scope).
    fn const_if_generate(&mut self, m: MExprId, cond: MExprId, tb: &MBlock, eb: &MBlock) -> SvExpr {
        let cond_sv = self.expr_value(cond);
        let base = self.fresh_block();
        let ty = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(&self.mir.expr(m).ty, &self.self_subst),
        );
        // Lower each branch into its own item list (swap `self.items` out so the
        // branch's nets/assigns land inside the generate block), then append the
        // wire-driving assign.
        let outer = std::mem::take(&mut self.items);
        let then_val = self.block_value(tb);
        let mut then_items = std::mem::take(&mut self.items);
        then_items.push(SvItem::Assign {
            lhs: SvExpr::Ident(base.clone()),
            rhs: then_val,
        });
        let else_val = self.block_value(eb);
        let mut else_items = std::mem::take(&mut self.items);
        else_items.push(SvItem::Assign {
            lhs: SvExpr::Ident(base.clone()),
            rhs: else_val,
        });
        self.items = outer;
        self.items.push(SvItem::Logic(SvLogicDecl {
            ty: sv_type(&ty, &self.sig.generic_params),
            name: base.clone(),
        }));
        self.items.push(SvItem::GenerateIf(SvGenerateIf {
            cond: cond_sv,
            label: format!("{base}__g"),
            then_items,
            else_items,
        }));
        SvExpr::Ident(base)
    }

    fn block_value(&mut self, block: &MBlock) -> SvExpr {
        self.lower_stmts(&block.stmts);
        match block.tail {
            Some(tail) => self.expr_value(tail),
            None => SvExpr::Lit("0".to_owned()),
        }
    }

    fn block_leaves(&mut self, block: &MBlock) -> Vec<(String, SvExpr)> {
        self.lower_stmts(&block.stmts);
        match block.tail {
            Some(tail) => self.expr_leaves(tail),
            None => Vec::new(),
        }
    }

    /// The SV types of a node's leaves.
    fn expr_leaf_types(&self, m: MExprId) -> Vec<SvType> {
        let t = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(&self.mir.expr(m).ty, &self.self_subst),
        );
        flatten_leaves(
            self.db,
            self.krate,
            self.def,
            &t,
            true,
            &self.sig.generic_params,
        )
        .into_iter()
        .map(|l| l.ty)
        .collect()
    }

    /// A scalar `if` mux into a fresh `__block_N`.
    fn lower_if(&mut self, m: MExprId, cond: MExprId, tb: &MBlock, eb: &MBlock) -> SvExpr {
        let synth = self.fresh_block();
        let ty = self.sv_type_of(&self.mir.expr(m).ty.clone());
        self.items.push(SvItem::Logic(SvLogicDecl {
            ty,
            name: synth.clone(),
        }));
        let cond = self.expr_value(cond);
        let then_v = self.block_value(tb);
        let else_v = self.block_value(eb);
        self.items.push(SvItem::AlwaysComb(SvAlwaysComb {
            body: vec![SvCombStmt::If(SvCombIf {
                cond,
                then_branch: vec![SvCombStmt::Assign {
                    lhs: SvExpr::Ident(synth.clone()),
                    rhs: then_v,
                }],
                else_branch: vec![SvCombStmt::Assign {
                    lhs: SvExpr::Ident(synth.clone()),
                    rhs: else_v,
                }],
            })],
        }));
        SvExpr::Ident(synth)
    }

    /// A per-leaf mux.
    fn lower_if_leaves(
        &mut self,
        m: MExprId,
        cond: MExprId,
        tb: &MBlock,
        eb: &MBlock,
    ) -> Vec<(String, SvExpr)> {
        let synth = self.fresh_block();
        let c = self.expr_value(cond);
        let then_leaves = self.block_leaves(tb);
        let else_leaves = self.block_leaves(eb);
        let tys = self.expr_leaf_types(m);
        let mut out = Vec::new();
        let mut body = Vec::new();
        for (k, (suffix, tv)) in then_leaves.into_iter().enumerate() {
            let name = join(&synth, &suffix);
            let ty = tys.get(k).cloned().unwrap_or_else(SvType::bit);
            self.items.push(SvItem::Logic(SvLogicDecl {
                ty,
                name: name.clone(),
            }));
            let ev = else_leaves
                .get(k)
                .map(|(_, e)| e.clone())
                .unwrap_or_else(|| SvExpr::Lit("0".to_owned()));
            body.push(SvCombStmt::If(SvCombIf {
                cond: c.clone(),
                then_branch: vec![SvCombStmt::Assign {
                    lhs: SvExpr::Ident(name.clone()),
                    rhs: tv,
                }],
                else_branch: vec![SvCombStmt::Assign {
                    lhs: SvExpr::Ident(name.clone()),
                    rhs: ev,
                }],
            }));
            out.push((suffix, SvExpr::Ident(name)));
        }
        self.items.push(SvItem::AlwaysComb(SvAlwaysComb { body }));
        out
    }

    /// The clock of a MIR `when` event (`clk.posedge()` builtin) — the receiver
    /// local's name.
    fn clock_of_event(&self, event: MExprId) -> String {
        if let MExprKind::Builtin {
            method: BuiltinMethod::Posedge,
            receiver,
            ..
        } = &self.mir.expr(event).kind
            && let MExprKind::Local(l) = &self.mir.expr(*receiver).kind
        {
            return self.local_name(*l);
        }
        self.first_clock()
    }

    /// A value-position `when` register into a fresh
    /// `__block_N`.
    fn lower_when(
        &mut self,
        m: MExprId,
        event: MExprId,
        body: &MBlock,
        init: Option<MExprId>,
    ) -> SvExpr {
        let synth = self.fresh_block();
        let ty = self.sv_type_of(&self.mir.expr(m).ty.clone());
        self.items.push(SvItem::Logic(SvLogicDecl {
            ty,
            name: synth.clone(),
        }));
        if let Some(init) = init {
            let v = self.expr_value(init);
            self.items
                .push(SvItem::Initial(vec![(SvExpr::Ident(synth.clone()), v)]));
        }
        let clock = self.clock_of_event(event);
        let d = self.block_value(body);
        self.items.push(SvItem::AlwaysFf(SvAlwaysFf {
            clock,
            reset: None,
            reset_body: Vec::new(),
            clocked_body: vec![SvSeqAssign::new(SvExpr::Ident(synth.clone()), d)],
        }));
        SvExpr::Ident(synth)
    }

    /// A per-leaf register.
    fn lower_when_leaves(
        &mut self,
        m: MExprId,
        event: MExprId,
        body: &MBlock,
        init: Option<MExprId>,
    ) -> Vec<(String, SvExpr)> {
        let synth = self.fresh_block();
        let clock = self.clock_of_event(event);
        if let Some(init) = init {
            let init_leaves = self.expr_leaves(init);
            let assigns = init_leaves
                .into_iter()
                .map(|(suffix, v)| (SvExpr::Ident(join(&synth, &suffix)), v))
                .collect();
            self.items.push(SvItem::Initial(assigns));
        }
        let d_leaves = self.block_leaves(body);
        let tys = self.expr_leaf_types(m);
        let mut out = Vec::new();
        let mut seq = Vec::new();
        for (k, (suffix, d)) in d_leaves.into_iter().enumerate() {
            let name = join(&synth, &suffix);
            let ty = tys.get(k).cloned().unwrap_or_else(SvType::bit);
            self.items.push(SvItem::Logic(SvLogicDecl {
                ty,
                name: name.clone(),
            }));
            seq.push(SvSeqAssign::new(SvExpr::Ident(name.clone()), d));
            out.push((suffix, SvExpr::Ident(name)));
        }
        self.items.push(SvItem::AlwaysFf(SvAlwaysFf {
            clock,
            reset: None,
            reset_body: Vec::new(),
            clocked_body: seq,
        }));
        out
    }

    /// Statement-form `when` (clocked partial
    /// drives, optional `init`).
    fn lower_when_stmt(&mut self, event: MExprId, body: &MBlock, init: Option<&MBlock>) {
        let clock = self.clock_of_event(event);
        if let Some(init) = init {
            let mut assigns = Vec::new();
            for stmt in &init.stmts {
                if let MStmt::Equation { lhs, rhs } = stmt {
                    let lhs_leaves = self.place_leaves_dir(lhs);
                    let rhs_leaves = self.value_leaves_dir(*rhs);
                    for ((lp, _), (rp, _)) in lhs_leaves.into_iter().zip(rhs_leaves) {
                        assigns.push((lp, rp));
                    }
                }
            }
            if !assigns.is_empty() {
                self.items.push(SvItem::Initial(assigns));
            }
        }
        let mut seq = Vec::new();
        self.when_body_seq(body, None, &mut seq);
        self.items.push(SvItem::AlwaysFf(SvAlwaysFf {
            clock,
            reset: None,
            reset_body: Vec::new(),
            clocked_body: seq,
        }));
    }

    /// Flatten a `when` body into guarded
    /// nonblocking assignments (an `if` narrows the guard).
    fn when_body_seq(&mut self, block: &MBlock, guard: Option<SvExpr>, seq: &mut Vec<SvSeqAssign>) {
        for stmt in &block.stmts {
            match stmt {
                MStmt::Equation { lhs, rhs } => {
                    let lhs_leaves = self.place_leaves_dir(lhs);
                    let rhs_leaves = self.value_leaves_dir(*rhs);
                    for ((lp, _), (rp, _)) in lhs_leaves.into_iter().zip(rhs_leaves) {
                        seq.push(SvSeqAssign {
                            lhs: lp,
                            rhs: rp,
                            guard: guard.clone(),
                        });
                    }
                }
                MStmt::Expr(e) => {
                    if let MExprKind::If {
                        cond,
                        then_branch,
                        else_branch,
                    } = &self.mir.expr(*e).kind
                    {
                        let (cond, then_b, else_b) =
                            (*cond, then_branch.clone(), else_branch.clone());
                        let c = self.expr_value(cond);
                        self.when_body_seq(&then_b, Some(and_guard(&guard, c.clone())), seq);
                        if !else_b.stmts.is_empty() {
                            let not_c = SvExpr::BinOp(
                                SvBinOp::Eq,
                                Box::new(c),
                                Box::new(SvExpr::Lit("1'b0".to_owned())),
                            );
                            self.when_body_seq(&else_b, Some(and_guard(&guard, not_c)), seq);
                        }
                    }
                }
                MStmt::Let { local, value } => self.lower_let(*local, *value),
                _ => {}
            }
        }
    }

    /// The clock signal name for a value's domain (a `Domain::Param` resolves to
    /// the corresponding `dom` generic's name; a bound `Clock` to its local).
    fn clock_of_type(&self, ty: Option<&Type<'db>>) -> String {
        let domain = match ty {
            Some(Type::Value { domain, .. }) | Some(Type::Port { domain, .. }) => *domain,
            _ => return self.first_clock(),
        };
        match domain {
            Domain::Param(i) => self
                .sig
                .generic_params
                .get(i as usize)
                .map(|g| g.name.clone())
                .unwrap_or_else(|| self.first_clock()),
            Domain::Clock(l) => self.local_name(l),
            _ => self.first_clock(),
        }
    }

    /// Fallback clock: the first `dom` generic parameter's name, else `clk`.
    fn first_clock(&self) -> String {
        self.sig
            .generic_params
            .iter()
            .find(|g| matches!(g.kind, TermKind::Domain(_)) && !g.is_lifted_dom())
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "clk".to_owned())
    }

    // (Prelude operators were once recognised here by trait/method name and
    // emitted as an inline `SvBinOp` — `prelude_op`/`prelude_unary`. They are
    // now ordinary `verilog expr` bodies, spliced via `inline_call`/
    // `render_inline` like any other inline fn; see prelude.mrn.)

    // ----- #[inline] body splicing (planning/attributes.md) -----

    /// True if `def`'s body is an inline-expression verilog body
    /// (`= verilog expr { … }`): the whole template is one SV expression,
    /// spliced at the call site in whatever const/net context it sits — never
    /// instantiated. This subsumes the old `prelude_op`/`prelude_unary` special
    /// cases (the prelude operators are now expr-form bodies; see prelude.mrn).
    fn is_inline_expr_body(&self, def: DefId) -> bool {
        body(self.db, self.krate, def)
            .verilog()
            .is_some_and(|t| t.expr_form)
    }

    /// True if `def` should splice inline rather than instantiate: either an
    /// explicit `#[inline]`, or a `verilog expr` body.
    fn splices_inline(&self, def: DefId) -> bool {
        self.map.def_data(def).is_some_and(|d| d.inline) || self.is_inline_expr_body(def)
    }

    /// Splice a verilog inline template given the resolved param→value map and
    /// the call's const substitution. **Id-agnostic** — the SV-building half of
    /// `render_inline`: the caller builds `val_map`/`node_subst`, then calls this.
    fn render_inline_spliced(
        &mut self,
        template: &VerilogTemplate<'db>,
        val_map: &HashMap<LocalId, String>,
        node_subst: &[Option<Term<'db>>],
        result_ty: Option<&Type<'db>>,
    ) -> SvExpr {
        // A statement-form body drives a real `result` net: mint a fresh wire,
        // bind every `${result}` to it, and hand the net back. So the spliced body
        // can name its own result (e.g. `type(${result})'(…)`, the zero-width-safe
        // resize cast), the same way a module's output is a proper net.
        let result_name = if template.expr_form {
            String::new()
        } else {
            self.fresh_block()
        };
        let mut out = String::new();
        for seg in &template.segments {
            match seg {
                VerilogSegment::Text(t) => out.push_str(t),
                VerilogSegment::ResultPort => {
                    out.push_str(if template.expr_form {
                        "result"
                    } else {
                        &result_name
                    });
                }
                VerilogSegment::Param(l) => {
                    out.push_str(val_map.get(l).map(String::as_str).unwrap_or("0"));
                }
                VerilogSegment::Dom(_) => out.push_str(&self.first_clock()),
                VerilogSegment::Const(c) => {
                    // First resolve the call's own const generics (`to` →
                    // `A::bit_size`, in the enclosing def's terms), then the
                    // enclosing def's monomorphisation subst (`A` → `uint(8)`),
                    // so an assoc-const projection onto an outer type param
                    // (`Assoc { self_ty: A }`) grounds rather than reaching
                    // `render_const_sv` as an opaque `Assoc(..)`.
                    let c = subst_const_opt(c, node_subst);
                    let c = subst_const_opt(&c, &self.self_subst);
                    let c = self.subst_promoted_const(&c);
                    let s =
                        match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, &c)
                        {
                            Some(v) => v.to_string(),
                            None => render_const_sv(&c, self.sig),
                        };
                    out.push_str(&s);
                }
            }
        }
        // An expression body IS the SV expression; splice it in place.
        if template.expr_form {
            return SvExpr::Lit(format!("({})", out.trim()));
        }
        // A statement body (`assign ${result} = RHS;`) materializes its result:
        // declare a net of the (grounded) return type, drive it with the body's
        // RHS, and return the net. Ground the callee return type through the call's
        // subst, then the caller's monomorphisation — the same two steps the `Const`
        // segments take above.
        let rt = result_ty.expect("statement-form inline body has a return type");
        let rt = subst_type(&subst_type(rt, node_subst), &self.self_subst);
        let rt = ground_widths(self.db, self.krate, self.def, &rt);
        // A width that grounds to a promoted const body-local renders against its
        // `localparam` name (`uint(w)` → `[w-1:0]`), as local decls do (1725).
        let rt = self.subst_promoted(&rt);
        let ty = flatten_leaves_inner(self.db, self.krate, &rt, true, &self.sig.generic_params)
            .into_iter()
            .next()
            .map(|l| l.ty)
            .unwrap_or_else(SvType::bit);
        self.items.push(SvItem::Logic(SvLogicDecl {
            ty,
            name: result_name.clone(),
        }));
        self.items.push(SvItem::Assign {
            lhs: SvExpr::Ident(result_name.clone()),
            rhs: SvExpr::Lit(extract_assign_rhs(&out)),
        });
        SvExpr::Ident(result_name)
    }

    /// Build the param→value map and
    /// const subst from a MIR `Call` node's parts, then share the splice. `def`
    /// is the resolved callee (after `resolve_trait_instance_with`); `substs` is
    /// the node's recorded subst; `subst_override` is the trait-instance subst
    /// when one applies.
    fn render_inline(
        &mut self,
        def: DefId<'db>,
        substs: &[Term<'db>],
        subst_override: Option<&[Option<Term<'db>>]>,
        receiver: Option<MExprId>,
        args: &[Conn],
        named: &[MNamedArg],
    ) -> SvExpr {
        let csig = sig_of(self.db, self.krate, def);
        let Some(template) = body(self.db, self.krate, def).verilog().cloned() else {
            panic!(
                "#[inline] on a Mirin-bodied fn `{}` is not yet supported (only \
                 verilog-bodied inline splicing is implemented)",
                self.map
                    .def_data(def)
                    .map(|d| d.name.as_str())
                    .unwrap_or("?"),
            );
        };
        // Value params → caller arg expressions: positional zip with
        // `[receiver?] ++ in-args`, named by name. (An inline call carries only
        // in-connections; an out-connection to an inline body is not a thing.)
        let mut positional: Vec<MExprId> = receiver.into_iter().collect();
        positional.extend(args.iter().filter_map(|a| match a {
            Conn::In(e) => Some(*e),
            Conn::Out(_) => None,
        }));
        let mut pos_i = 0;
        let mut val_map: HashMap<LocalId, String> = HashMap::new();
        for p in &csig.params {
            let caller_expr = if p.from_named_section {
                named
                    .iter()
                    .find(|n| n.name == p.name)
                    .and_then(|n| match &n.conn {
                        Conn::In(e) => Some(*e),
                        Conn::Out(_) => None,
                    })
            } else {
                let e = positional.get(pos_i).copied();
                pos_i += 1;
                e
            };
            // TODO(named-args): one scalar string per param — a multi-leaf
            // signal/port param is not handled.
            let rendered = match caller_expr {
                Some(e) => self.expr_value(e).to_string(),
                None => match &p.default {
                    Some(d) => default_value(d).to_string(),
                    None => continue,
                },
            };
            val_map.insert(p.local, rendered);
        }
        let mut node_subst: Vec<Option<Term<'db>>> = match subst_override {
            Some(ov) => ov.to_vec(),
            None => substs.iter().cloned().map(Some).collect(),
        };
        // Explicitly-provided const generics (`{w = …}`) are recorded as NAMED
        // args with their subst slot left deferred — bind them here too (the same
        // fix `splice_inline_body` applies for Mirin bodies), so a verilog inline
        // primitive called with named const generics grounds them.
        node_subst.resize(csig.generic_params.len(), None);
        for n in named {
            if let Conn::In(e) = &n.conn
                && let Some(i) = csig
                    .generic_params
                    .iter()
                    .position(|g| g.kind == TermKind::Const && g.name == n.name)
            {
                node_subst[i] = Some(Term::Const(self.mir_const_arg(*e)));
            }
        }
        self.render_inline_spliced(&template, &val_map, &node_subst, csig.return_type.as_ref())
    }

    /// The leaves of an inline call, dispatching on the callee's body shape: a
    /// verilog template splices its trusted text (one scalar leaf); a Mirin body
    /// sub-lowers (`splice_inline_body`). The seam the two `Call` arms share.
    fn inline_call_leaves(
        &mut self,
        def: DefId<'db>,
        substs: &[Term<'db>],
        subst_override: Option<&[Option<Term<'db>>]>,
        receiver: Option<MExprId>,
        args: &[Conn],
        named: &[MNamedArg],
    ) -> Vec<(String, SvExpr)> {
        if body(self.db, self.krate, def).verilog().is_some() {
            vec![(
                String::new(),
                self.render_inline(def, substs, subst_override, receiver, args, named),
            )]
        } else {
            self.splice_inline_body(def, substs, subst_override, receiver, args, named)
        }
    }

    /// Ground a call's generic argument through the caller's own monomorphisation
    /// (`call_subst ∘ self_subst`): a type/const arg that projects onto an *outer*
    /// type param grounds once the enclosing module is monomorphised — the same
    /// double-substitution `render_inline_spliced`/`emit_instance_core` apply.
    fn compose_term(&self, t: &Option<Term<'db>>) -> Option<Term<'db>> {
        match t {
            Some(Term::Type(ty)) => Some(Term::Type(ground_widths(
                self.db,
                self.krate,
                self.def,
                &subst_type(ty, &self.self_subst),
            ))),
            Some(Term::Const(c)) => Some(Term::Const(self.caller_const(c))),
            other => other.clone(),
        }
    }

    /// Ground a const for hand-off into an inline splice: fold to a literal when
    /// it grounds (so the nested splice's `const if` can still fold); otherwise
    /// **pre-render it in THIS (caller) frame** as a `Symbol`, so a caller generic
    /// prints with the caller's name. The nested splice renders against the
    /// *callee* sig, so a bare caller `Param` would otherwise print the wrong
    /// generic (the cross-frame limit). A `Symbol` is inert to eval, so a symbolic
    /// guard correctly defers to `generate if`. A `Deferred` placeholder (a method
    /// generic bound later via a named arg) passes through untouched.
    fn caller_const(&self, c: &ConstArg<'db>) -> ConstArg<'db> {
        if matches!(c, ConstArg::Deferred) {
            return ConstArg::Deferred;
        }
        let c = self.subst_promoted_const(&subst_const_opt(c, &self.self_subst));
        match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, &c) {
            Some(v) => ConstArg::Lit(v),
            None => ConstArg::Symbol(render_const_sv(&c, self.sig)),
        }
    }

    /// Splice a Mirin-bodied `#[inline]` callee at this call site
    /// (planning/inline_bodies.md): sub-lower the callee's own `(body, inf, mir,
    /// sig)` in a fresh prefix-scoped `SvLower`, with its value params bound to
    /// caller-side wires, merging its items into the caller and returning the
    /// body's value as leaves. v1 scope (combinational, value-returning, no
    /// clocked state / `var` / out-param / `const if` / integer params) is
    /// enforced up front by `inline_check`, so a clean crate only reaches here
    /// with a spliceable body.
    fn splice_inline_body(
        &mut self,
        def: DefId<'db>,
        substs: &[Term<'db>],
        subst_override: Option<&[Option<Term<'db>>]>,
        receiver: Option<MExprId>,
        args: &[Conn],
        named: &[MNamedArg],
    ) -> Vec<(String, SvExpr)> {
        // A generous backstop: `inline_check` already rejects inline *cycles* at
        // the front end, so this only guards an exotic uncaught cycle (e.g. one
        // through method dispatch) — set well above any real inline nesting depth
        // so a legitimate deep-but-finite chain never trips it.
        const MAX_INLINE_DEPTH: u32 = 64;
        assert!(
            self.inline_depth < MAX_INLINE_DEPTH,
            "inline splice depth exceeded — an uncaught recursive `#[inline]` cycle?"
        );

        // The call's generic args, grounded through the caller's mono subst, are
        // the nested lower's `self_subst` (binds the callee's type/const params).
        let node_subst: Vec<Option<Term<'db>>> = match subst_override {
            Some(ov) => ov.to_vec(),
            None => substs.iter().cloned().map(Some).collect(),
        };
        let cbody = body(self.db, self.krate, def);
        let csig = sig_of(self.db, self.krate, def);
        let mut composed: Vec<Option<Term<'db>>> = (0..csig.generic_params.len())
            .map(|i| self.compose_term(&node_subst.get(i).cloned().flatten()))
            .collect();
        // An explicitly-provided const generic (`{k = 0}`) is recorded as a NAMED
        // arg with its subst slot left deferred — bind it into the const subst by
        // matching the named arg to the callee's const generic param. (The value
        // expr is in the *caller*'s MIR, so ground it with our own `mir_const_arg`.)
        for n in named {
            if let Conn::In(e) = &n.conn
                && let Some(i) = csig
                    .generic_params
                    .iter()
                    .position(|g| g.kind == TermKind::Const && g.name == n.name)
            {
                let c = self.mir_const_arg(*e);
                composed[i] = Some(Term::Const(self.caller_const(&c)));
            }
        }
        let site = self.fresh_inline();
        let mut sub = SvLower {
            db: self.db,
            krate: self.krate,
            def,
            map: crate_def_map(self.db, self.krate),
            body: cbody,
            inf: infer(self.db, self.krate, def),
            mir: mir_of(self.db, self.krate, def),
            sig: csig,
            self_subst: composed,
            local_names: unique_local_names(cbody),
            items: Vec::new(),
            synth: 0,
            index_asserts: HashSet::new(),
            instance_counts: HashMap::new(),
            declared: HashSet::new(),
            mono_reqs: Vec::new(),
            promoted: HashMap::new(),
            fns_emitted: HashSet::new(),
            prefix: site,
            inline_depth: self.inline_depth + 1,
        };

        // Bind each value param to a caller-side wire: declare the param's leaves
        // (named exactly as the nested body reads them) and assign each from the
        // caller argument's leaf. A param read inside the callee then resolves to
        // its wire — one wire even if the param is used many times.
        let mut positional: Vec<MExprId> = receiver.into_iter().collect();
        positional.extend(args.iter().filter_map(|a| match a {
            Conn::In(e) => Some(*e),
            Conn::Out(_) => None,
        }));
        let mut pos_i = 0;
        for p in &csig.params {
            let caller_expr = if p.from_named_section {
                named
                    .iter()
                    .find(|n| n.name == p.name)
                    .and_then(|n| match &n.conn {
                        Conn::In(e) => Some(*e),
                        Conn::Out(_) => None,
                    })
            } else {
                let e = positional.get(pos_i).copied();
                pos_i += 1;
                e
            };
            let base = sub.local_name(p.local);
            let param_leaves = sub.local_type_leaves(p.local);
            // Caller-side value per param leaf: the argument's leaves, or — for an
            // omitted param with a default — the default broadcast to each leaf
            // (matching `emit_instance_core`'s scalar-default broadcast). A param
            // with neither is an arity error reported by the front end.
            let caller_vals: Vec<SvExpr> = match caller_expr {
                Some(e) => self.expr_leaves(e).into_iter().map(|(_, v)| v).collect(),
                None => match &p.default {
                    Some(d) => param_leaves.iter().map(|_| default_value(d)).collect(),
                    None => continue,
                },
            };
            for (leaf, cv) in param_leaves.into_iter().zip(caller_vals) {
                let name = join(&base, &leaf.suffix);
                self.items.push(SvItem::Logic(SvLogicDecl {
                    ty: leaf.ty,
                    name: name.clone(),
                }));
                self.push_assign(SvExpr::Ident(name), cv);
            }
        }

        // Lower the callee body for its value: emit intermediate `let`s/blocks,
        // then take the tail (or the desugared whole-result equation / `return`)
        // as the spliced value — never driving a `result` port (there is none).
        let block = sub.mir.block().clone();
        let mut rest: Vec<MStmt> = Vec::new();
        let mut value_src: Option<MExprId> = block.tail;
        for s in &block.stmts {
            match s {
                MStmt::Equation { lhs, rhs }
                    if lhs.projections.is_empty()
                        && sub.mir.local(lhs.base).result_base.is_some() =>
                {
                    value_src = Some(*rhs);
                }
                MStmt::Return { value } => value_src = Some(*value),
                other => rest.push(other.clone()),
            }
        }
        sub.lower_stmts(&rest);
        let leaves = match value_src {
            Some(e) => sub.expr_leaves(e),
            None => vec![(String::new(), SvExpr::Lit("0".to_owned()))],
        };

        self.items.append(&mut sub.items);
        self.mono_reqs.append(&mut sub.mono_reqs);
        leaves
    }

    // ----- instantiation (user calls / methods → submodules) -----

    /// Id-agnostic core of `emit_instance`: build the SV module instance from the
    /// callee def, the recorded/overridden subst, and the resolved [`CallSlot`]s.
    /// Shared core, fed by `emit_instance`.
    fn emit_instance_core(
        &mut self,
        def: DefId<'db>,
        subst_override: Option<Vec<Option<Term<'db>>>>,
        recorded: Vec<Term<'db>>,
        slots: Vec<CallSlot<'db>>,
        result_target: Vec<(String, SvExpr)>,
    ) {
        let csig = sig_of(self.db, self.krate, def);
        let doms: Vec<String> = csig
            .generic_params
            .iter()
            .filter(|g| matches!(g.kind, TermKind::Domain(_)) && !g.is_lifted_dom())
            .map(|g| g.name.clone())
            .collect();

        // The callee's Const-kind generics bind as SV parameters, from the
        // call's recorded instantiation (`#(.n(8))`; a still-symbolic value
        // renders against the caller's own SV parameters; an evaluable local
        // grounds through const_eval).
        let node_subst: Vec<Option<Term<'db>>> = match &subst_override {
            Some(ov) => ov.clone(),
            None => recorded.iter().cloned().map(Some).collect(),
        };
        // A const arg recorded against this generic def is in the def's own
        // term space: ground its type-param projections through the enclosing
        // monomorphisation subst (`Assoc { self_ty: A }` → `uint(8)::bit_size`),
        // then rewrite a promoted body local (`sink(wide)` with `wide: uint(w)`)
        // to its `localparam` name — both in the `#(.W(w))` binding and when
        // flattening the callee's `uint(W)` leaves.
        let node_subst: Vec<Option<Term<'db>>> = node_subst
            .into_iter()
            .map(|t| match t {
                Some(Term::Const(c)) => {
                    let c = subst_const_opt(&c, &self.self_subst);
                    Some(Term::Const(self.subst_promoted_const(&c)))
                }
                other => other,
            })
            .collect();
        let parameters: Vec<(String, SvExpr)> = csig
            .generic_params
            .iter()
            .enumerate()
            .filter(|(_, g)| g.kind == TermKind::Const)
            .filter_map(|(i, g)| {
                let term = node_subst.get(i).and_then(|o| o.as_ref());
                let Some(Term::Const(c)) = term else {
                    return None;
                };
                let c = match c {
                    ConstArg::Local(_)
                    | ConstArg::Field(..)
                    | ConstArg::Op(..)
                    | ConstArg::Assoc { .. } => {
                        match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, c) {
                            Some(v) => ConstArg::Lit(v),
                            None => c.clone(),
                        }
                    }
                    other => other.clone(),
                };
                let rendered = match &c {
                    ConstArg::Lit(v) => SvExpr::Lit(v.to_string()),
                    ConstArg::Param(j) => SvExpr::Ident(
                        self.sig
                            .generic_params
                            .get(*j as usize)
                            .map(|g| g.name.clone())?,
                    ),
                    // A promoted body local → its `localparam` name (`#(.W(w))`).
                    ConstArg::Symbol(s) => SvExpr::Ident(s.clone()),
                    ConstArg::Op(..) => SvExpr::Lit(render_const_sv(&c, self.sig)),
                    _ => return None, // unresolved — leave to the default
                };
                Some((g.name.clone(), rendered))
            })
            .collect();

        // A type-generic callee is monomorphised: bind its Type params from the
        // actual arg types (recorded on each slot), name the copy `Callee__Arg`,
        // and request its emission.
        let mut subst: Vec<Option<Term<'db>>> = if let Some(ov) = &subst_override {
            ov.clone()
        } else if is_type_generic(csig) {
            let mut subst = vec![None; csig.generic_params.len()];
            for slot in &slots {
                if let Some(at) = &slot.actual_ty {
                    match_type(&slot.ty, at, &mut subst);
                }
            }
            subst
        } else {
            Vec::new()
        };
        // Fill any still-unbound TYPE-kind generic from the inferred call subst.
        // A receiver-less type-path call (`Vec(..)::unpack(b)`) carries its Self
        // type in the path, not a value arg, so `match_type` over the args can't
        // see it — but `infer` recorded it (planning/pack_resize.md).
        if is_type_generic(csig) {
            if subst.is_empty() {
                subst = vec![None; csig.generic_params.len()];
            }
            for (i, g) in csig.generic_params.iter().enumerate() {
                if g.kind == TermKind::Type && subst.get(i).is_none_or(Option::is_none) {
                    if let Some(t @ Some(Term::Type(_))) = node_subst.get(i) {
                        subst[i] = t.clone();
                    }
                }
            }
        }
        // Only TYPE-kind bindings force a specialised copy — Const-kind
        // bindings ride the `#(...)` parameters of the one parametric module.
        let needs_mono =
            csig.generic_params.iter().enumerate().any(|(i, g)| {
                g.kind == TermKind::Type && subst.get(i).is_some_and(Option::is_some)
            });
        let module = if needs_mono {
            let name = mono_name(self.map, def, csig, &subst);
            self.mono_reqs.push(MonoReq {
                callee: def,
                subst: subst.clone(),
                name: name.clone(),
            });
            name
        } else {
            module_name(self.map, def)
        };

        // To flatten the callee's types to connection leaves, substitute BOTH
        // its type params (the mono `subst`) AND its const params (`node_subst`,
        // the `#(...)` bindings). A const width like `ff_gen`'s `uint(n)` is a
        // `Param` in the CALLEE's index space; without the const binding it would
        // survive into the caller's generics and render as an out-of-range width.
        // (The leaf `.ty` is only used for suffixes here, but it must still
        // resolve — emission no longer tolerates an unrenderable width.)
        let flatten_subst: Vec<Option<Term<'db>>> = (0..csig.generic_params.len())
            .map(|i| {
                subst
                    .get(i)
                    .cloned()
                    .flatten()
                    .or_else(|| node_subst.get(i).cloned().flatten())
            })
            .collect();

        let mut connections = Vec::new();
        // 1. `dom` generics → the caller's clock.
        let clk = self.first_clock();
        for d in &doms {
            connections.push((d.clone(), SvExpr::Ident(clk.clone())));
        }
        // 2. value/out params (flattened through the mono subst), each connected
        //    to its caller expression's leaves.
        for slot in &slots {
            let pty = subst_type(&slot.ty, &flatten_subst);
            let callee_leaves = flatten_leaves(
                self.db,
                self.krate,
                self.def,
                &pty,
                true,
                &self.sig.generic_params,
            );
            // A supplied arg flattens to its leaves; an omitted param with a
            // default wires that default to each callee leaf.
            // TODO(named-args): the default is BROADCAST as a single scalar to
            // every callee leaf — correct only for a scalar default (`rstn =
            // high`, `reset_val = 0`). A multi-leaf SIGNAL/PORT param with a
            // `= default` (a deliberate feature: named params can be signals/
            // ports, not just generics) would mis-wire — each leaf needs the
            // corresponding leaf of a flattened default, not the whole scalar.
            let caller_leaves: Vec<(String, SvExpr)> = match &slot.caller_leaves {
                Some(ls) => ls.clone(),
                None => match &slot.default {
                    Some(d) => callee_leaves
                        .iter()
                        .map(|_| (String::new(), default_value(d)))
                        .collect(),
                    None => Vec::new(),
                },
            };
            // TODO(named-args): this connects every leaf as `.port(caller)`
            // regardless of `cl.drives`. For an instance that is fine (SV resolves
            // port direction), but a port param with per-field directions is not
            // direction-folded the way returns are (drive_result honours
            // `rl.drives`); revisit if/when named port params with mixed
            // in/out fields are exercised.
            for (cl, (_, cv)) in callee_leaves.into_iter().zip(caller_leaves) {
                connections.push((join(&slot.name, &cl.suffix), cv));
            }
        }
        // 3. return → the result target, by the CALLEE's result places: an
        //    unnamed `return` is `result__…`, named results use their bound
        //    port names (`output__valid`, `sum`). Leaf order matches the return
        //    type's flattening, so the positional zip with `result_target`
        //    holds (planning/return_variable.md).
        let mut callee_result_ports: Vec<String> = Vec::new();
        for place in &csig.result_places {
            let pty = subst_type(&place.ty, &flatten_subst);
            for leaf in flatten_leaves(
                self.db,
                self.krate,
                self.def,
                &pty,
                true,
                &self.sig.generic_params,
            ) {
                callee_result_ports.push(join(&place.sv_base, &leaf.suffix));
            }
        }
        for (port, (_, tv)) in callee_result_ports.into_iter().zip(result_target) {
            connections.push((port, tv));
        }
        // Name after building connections, so a nested call (emitted while
        // resolving an argument) takes the earlier instance number.
        let name = self.instance_name(&module);
        self.items.push(SvItem::Instance(SvInstance {
            module,
            name,
            parameters,
            connections,
        }));
    }

    /// The grounded, substituted type of a MIR node.
    fn actual_type(&self, m: MExprId) -> Option<Type<'db>> {
        if let MExprKind::Local(l) = self.mir.expr(m).kind {
            self.local_ty(l)
        } else {
            Some(self.mir.expr(m).ty.clone())
        }
    }

    /// Resolve each value param's caller arg off the
    /// MIR `Call` node (leaves via `expr_leaves`, type via `actual_type`)
    /// into [`CallSlot`]s, then share `emit_instance_core`.
    fn emit_instance(
        &mut self,
        def: DefId<'db>,
        recorded: &[Term<'db>],
        subst_override: Option<Vec<Option<Term<'db>>>>,
        receiver: Option<MExprId>,
        args: &[Conn],
        named: &[MNamedArg],
        result_target: Vec<(String, SvExpr)>,
    ) {
        let csig = sig_of(self.db, self.krate, def);
        // Positional = `[receiver?] ++ args` as connections (in OR out — an
        // out-arg `=> target` connects the callee's out port to a caller place).
        let mut positional: Vec<Conn> = Vec::new();
        if let Some(r) = receiver {
            positional.push(Conn::In(r));
        }
        positional.extend(args.iter().cloned());
        let mut pos_i = 0;
        let slots: Vec<CallSlot<'db>> = csig
            .params
            .iter()
            .map(|p| {
                let conn = if p.from_named_section {
                    named
                        .iter()
                        .find(|n| n.name == p.name)
                        .map(|n| n.conn.clone())
                } else {
                    let c = positional.get(pos_i).cloned();
                    pos_i += 1;
                    c
                };
                let (caller_leaves, actual_ty) = match conn {
                    Some(Conn::In(e)) => (Some(self.expr_leaves(e)), self.actual_type(e)),
                    Some(Conn::Out(place)) => {
                        // The out-target's leaves; its type (for mono) is known
                        // for a bare-local target (the common `=> target` form).
                        let aty = if place.projections.is_empty() {
                            self.local_ty(place.base)
                        } else {
                            None
                        };
                        (Some(self.projected_leaves(&place)), aty)
                    }
                    None => (None, None),
                };
                CallSlot {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    caller_leaves,
                    actual_ty,
                    default: p.default.clone(),
                }
            })
            .collect();
        self.emit_instance_core(def, subst_override, recorded.to_vec(), slots, result_target);
    }

    /// Emit a module instance for a MIR `Call` node (resolving the trait
    /// instance), driving `result_target` from its return.
    fn emit_instance_from(&mut self, m: MExprId, result_target: Vec<(String, SvExpr)>) {
        let MExprKind::Call {
            callee,
            substs,
            receiver,
            args,
            named,
        } = &self.mir.expr(m).kind
        else {
            unreachable!("emit_instance_from on a non-Call node");
        };
        let (callee, substs) = (*callee, substs.clone());
        let (receiver, args, named) = (*receiver, args.clone(), named.clone());
        let (def, ov) = self.mir_call_target(callee, &substs);
        self.emit_instance(def, &substs, ov, receiver, &args, &named, result_target);
    }

    /// Declare each `=> target` out-arg's
    /// (bare-local, non-param) place as a fresh `logic` before the instance.
    fn declare_out_targets(&mut self, args: &[Conn], named: &[MNamedArg]) {
        let places: Vec<&Place> = named
            .iter()
            .filter_map(|n| match &n.conn {
                Conn::Out(p) => Some(p),
                Conn::In(_) => None,
            })
            .chain(args.iter().filter_map(|a| match a {
                Conn::Out(p) => Some(p),
                Conn::In(_) => None,
            }))
            .collect();
        for place in places {
            if place.projections.is_empty() && self.mir.local(place.base).kind != LocalKind::Param {
                self.declare_local(place.base);
            }
        }
    }

    /// A void user call in statement position
    /// (`f();`, or a unit fn's tail/return) — a (void) instance whose out-args
    /// bind callee out-ports to caller places. An inline / non-call / const-only
    /// statement has no instance to emit.
    fn lower_call_stmt(&mut self, m: MExprId) {
        let MExprKind::Call {
            callee,
            substs,
            args,
            named,
            ..
        } = &self.mir.expr(m).kind
        else {
            return;
        };
        let (callee, substs) = (*callee, substs.clone());
        let (args, named) = (args.clone(), named.clone());
        let (def, _) = self.mir_call_target(callee, &substs);
        if self.splices_inline(def) || is_const_only_fn(sig_of(self.db, self.krate, def)) {
            return;
        }
        self.declare_out_targets(&args, &named);
        self.emit_instance_from(m, Vec::new());
    }

    /// A value-position user call instantiates
    /// into a fresh `__call_N` (declared per result leaf) and returns its leaves.
    /// (The instance path only wires in-connections; a value-position call with
    /// out-args is not handled here.)
    fn call_value_leaves(&mut self, m: MExprId) -> Vec<(String, SvExpr)> {
        let MExprKind::Call {
            callee,
            substs,
            receiver,
            args,
            named,
        } = &self.mir.expr(m).kind
        else {
            unreachable!("call_value_leaves on a non-Call node");
        };
        let (callee, substs) = (*callee, substs.clone());
        let (receiver, args, named) = (*receiver, args.clone(), named.clone());
        let (def, ov) = self.mir_call_target(callee, &substs);
        let Some(rt) = sig_of(self.db, self.krate, def).return_type.clone() else {
            self.emit_instance(def, &substs, ov, receiver, &args, &named, Vec::new());
            return vec![(String::new(), SvExpr::Lit("0".to_owned()))];
        };
        // Substitute the callee-space return type by the recorded instantiation
        // before flattening (mirrors `call_value_leaves`).
        let rt = match &ov {
            Some(o) => ground_widths(self.db, self.krate, self.def, &subst_type(&rt, o)),
            None if substs.is_empty() => rt,
            None => {
                let opts: Vec<Option<Term<'db>>> = substs.iter().cloned().map(Some).collect();
                ground_widths(self.db, self.krate, self.def, &subst_type(&rt, &opts))
            }
        };
        let base = self.fresh_call();
        let target: Vec<(String, SvExpr)> = flatten_leaves(
            self.db,
            self.krate,
            self.def,
            &rt,
            true,
            &self.sig.generic_params,
        )
        .into_iter()
        .map(|l| {
            let name = join(&base, &l.suffix);
            self.items.push(SvItem::Logic(SvLogicDecl {
                ty: l.ty,
                name: name.clone(),
            }));
            (l.suffix, SvExpr::Ident(name))
        })
        .collect();
        self.emit_instance(def, &substs, ov, receiver, &args, &named, target.clone());
        target
    }

    fn fresh_call(&mut self) -> String {
        let n = self.synth;
        self.synth += 1;
        format!("{}__call_{n}", self.prefix)
    }

    /// A unique per-site inline prefix, scoped under this lower's own prefix so
    /// nested splices never collide (`__inl0__` then `__inl0____inl1__`, …).
    fn fresh_inline(&mut self) -> String {
        let n = self.synth;
        self.synth += 1;
        format!("{}__inl{n}__", self.prefix)
    }

    /// A per-callee instance name: the first instance is the bare module name,
    /// later ones get `_1`, `_2`, ….
    fn instance_name(&mut self, module: &str) -> String {
        let n = self.instance_counts.entry(module.to_owned()).or_insert(0);
        let bare = if *n == 0 {
            module.to_owned()
        } else {
            format!("{module}_{n}")
        };
        *n += 1;
        format!("{}{bare}", self.prefix)
    }
}

/// A value param resolved to its caller argument, ready for `emit_instance_core`
/// — the id-agnostic hand-off between the HIR/MIR call paths and instance
/// emission. `caller_leaves` is `None` when the arg was omitted (use `default`).
struct CallSlot<'db> {
    name: String,
    ty: Type<'db>,
    caller_leaves: Option<Vec<(String, SvExpr)>>,
    actual_ty: Option<Type<'db>>,
    default: Option<String>,
}

/// One scalar leaf of a (possibly aggregate) value: its `__`-suffix relative to
/// the binding's base name, its scalar SV type, and whether *this* module drives
/// it (an output, vs. an input it reads).
struct Leaf {
    /// `""` for a scalar; `"valid"`, `"valid__x"` for (nested) struct/port fields.
    suffix: String,
    ty: SvType,
    drives: bool,
}

/// Flatten a type into its scalar leaves. A struct/port erases to one leaf per
/// terminal field (`Packet` → `valid` + `payload`); a port folds each field's
/// direction with `drives` (the binding's own drive: an `out` param / a return
/// drives, an `in` param reads) so a leaf is an output iff this module drives it.
/// A scalar is a single leaf with the given `drives`.
///
/// `generics` is the enclosing def's generic params — used to render a symbolic
/// width (`uint(N)` → `[N-1:0]`). When descending a struct/port, its generic
/// args are substituted into the field types (a parametric `Bus(uint(8))` field
/// `data: A` becomes `uint(8)`), so the leaves are concrete or in terms of the
/// enclosing def's own params.
/// Flatten a type to its SV leaves, **grounding every width first** (a
/// `uint(T::width)` associated const, a `uint(n + 1)` expression → a literal
/// via `const_eval`). Grounding here, at the single entry, guarantees `sv_type`
/// only ever sees a literal or a generic `Param` width — so it can panic on
/// anything else rather than silently emit a 1-bit logic (which masked an
/// ungrounded-width bug). `def` is the body context const_eval resolves in.
fn flatten_leaves<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    ty: &Type<'db>,
    drives: bool,
    generics: &[GenericParam],
) -> Vec<Leaf> {
    let ty = ground_widths(db, krate, def, ty);
    flatten_leaves_inner(db, krate, &ty, drives, generics)
}

/// The recursion of [`flatten_leaves`], over an already-grounded type.
fn flatten_leaves_inner(
    db: &dyn salsa::Database,
    krate: SourceRoot,
    ty: &Type<'_>,
    drives: bool,
    generics: &[GenericParam],
) -> Vec<Leaf> {
    match ty {
        // Struct-of-arrays (planning/vectors.md): one unpacked-array leaf
        // per ELEMENT-TYPE leaf — Vec(3, Packet) → v__valid[0:2] + ….
        Type::Vec { len, elem } => {
            // The unpacked-array dimension. Share `width_expr` so a Vec length
            // and a scalar width render identically: literals verbatim, a bare
            // generic as an SV parameter, and a symbolic COMPOUND length
            // (`Vec(a + b, …)`) as a rendered SV expression — leaning on the
            // elaborator. A bug once made a non-literal length silently `1`
            // (`[0:0]`), a wrong-width SILENT MISCOMPILE; `width_expr` panics on
            // a genuinely unrenderable form instead.
            let dim = width_expr(len, generics);
            flatten_leaves_inner(db, krate, elem, drives, generics)
                .into_iter()
                .map(|mut leaf| {
                    leaf.ty.unpacked.insert(0, dim.clone());
                    leaf
                })
                .collect()
        }
        // A tuple flattens like a struct whose field names are element
        // indices: `x.0.valid` → `x__0__valid` (planning/tuples.md). Port
        // elements fold direction through their own flattening.
        Type::Tuple(elems) => {
            let mut out = Vec::new();
            for (i, ety) in elems.iter().enumerate() {
                for sub in flatten_leaves_inner(db, krate, ety, drives, generics) {
                    out.push(Leaf {
                        suffix: join(&i.to_string(), &sub.suffix),
                        ty: sub.ty,
                        drives: sub.drives,
                    });
                }
            }
            out
        }
        Type::Port { def, args, .. } => {
            let sig = sig_of(db, krate, *def);
            let subst = build_subst(&sig.generic_params, args);
            let mut out = Vec::new();
            for f in &sig.fields {
                // The module drives a port field iff its own drive matches the
                // field's producer direction (`out` field of an `out` port, or
                // `in` field of an `in` port). A struct field has no direction
                // (`None`) — it is positive, flowing with the parent, exactly
                // like an `out` field (structs_as_ports.md).
                let child = drives == (f.direction != Some(Direction::In));
                let fty = subst_type(&f.ty, &subst);
                for sub in flatten_leaves_inner(db, krate, &fty, child, generics) {
                    out.push(Leaf {
                        suffix: join(&f.name, &sub.suffix),
                        ty: sub.ty,
                        drives: sub.drives,
                    });
                }
            }
            out
        }
        _ => vec![Leaf {
            suffix: String::new(),
            ty: sv_type(ty, generics),
            drives,
        }],
    }
}

/// Build a substitution from a def's generic params to a use-site's args.
/// Args appear in declared-param order (named section first, then
/// positional — `sig.rs::lower_args`), but a use site may omit the named
/// section entirely (`Buf(8)` for `Buf{dom clk}(param N)`), so each arg binds
/// the next unfilled param *of its own kind*. Indexed by full generic-param
/// position; unsupplied params stay `None`.
fn build_subst<'db>(
    generic_params: &[GenericParam],
    args: &GenericArgs<'db>,
) -> Vec<Option<Term<'db>>> {
    let mut subst: Vec<Option<Term<'db>>> = vec![None; generic_params.len()];
    let mut start = 0;
    for a in &args.0 {
        let matches_kind = |g: &GenericParam| match a {
            Term::Type(_) => g.kind == TermKind::Type,
            Term::Const(_) => g.kind == TermKind::Const,
            Term::Domain(_) => matches!(g.kind, TermKind::Domain(_)),
        };
        if let Some(i) = (start..generic_params.len())
            .find(|&i| subst[i].is_none() && matches_kind(&generic_params[i]))
        {
            subst[i] = Some(a.clone());
            start = i + 1;
        }
    }
    subst
}

/// Folds a promoted const body local (`ConstArg::Local(l)`) to its `localparam`
/// name (`ConstArg::Symbol`), so widths/splices sized by it render against the
/// emitted localparam instead of panicking on an ungrounded `Local`.
struct PromotedFolder<'a> {
    promoted: &'a HashMap<LocalId, String>,
}

impl<'db> crate::hir::types::Folder<'db> for PromotedFolder<'_> {
    fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
        if let ConstArg::Local(l) = c
            && let Some(name) = self.promoted.get(l)
        {
            return ConstArg::Symbol(name.clone());
        }
        crate::hir::types::super_fold_const(self, c)
    }
}

/// The uniform zero value for an SV leaf type: `'{default: '0}` for an
/// unpacked-array leaf (a `Vec`; works at any length, including the degenerate
/// `[0:-1]` zero-length), else the scalar/packed `'0`. The leaf-level analog of
/// `bits(0)`'s `'0` for the zero-width slice guard (planning/slice_guards.md).
fn zero_value_for(ty: &SvType) -> SvExpr {
    if ty.unpacked.is_empty() {
        SvExpr::Lit("'0".to_owned())
    } else {
        SvExpr::Lit("'{default: '0}".to_owned())
    }
}

/// Substitute a def's generic args into a (field) type: a `Param(i)` type → the
/// i-th type arg, a `uint(Param(i))` width → the i-th const arg; nested
/// struct/port args are substituted recursively. Anything unbound is unchanged.
/// Ground every evaluable width in a type through `const_eval` — `uint(n+5)`
/// flattens to `uint(8)` before SV type rendering. Symbolic widths (free
/// generic params) survive unchanged and render as SV parameters.
fn ground_widths<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    ty: &Type<'db>,
) -> Type<'db> {
    struct G<'db> {
        db: &'db dyn salsa::Database,
        krate: SourceRoot,
        def: DefId<'db>,
    }
    impl<'db> Folder<'db> for G<'db> {
        fn fold_const(&mut self, c: &ConstArg<'db>) -> ConstArg<'db> {
            match c {
                // A symbolic assoc (an input is a `#(...)` parameter) can't
                // evaluate to a literal; expand it to its substituted body so a
                // compound width like `N * A::bit_size` becomes `N * 8` — a
                // renderable `Op` — instead of an unrenderable `Assoc`
                // (planning/pack_resize.md). Then fold that body (grounding its
                // inner, now-concrete assoc).
                ConstArg::Assoc { item, self_ty } => {
                    match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, c) {
                        Some(v) => ConstArg::Lit(v),
                        None => match crate::hir::const_eval::assoc_grounded_body(
                            self.db, self.krate, *item, self_ty,
                        ) {
                            Some(body) => self.fold_const(&body),
                            None => c.clone(),
                        },
                    }
                }
                ConstArg::Local(_) | ConstArg::Op(..) | ConstArg::Field(..) => {
                    match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, c) {
                        Some(v) => ConstArg::Lit(v),
                        // Not a literal (a symbolic operand): fold the children,
                        // so an inner assoc/local still grounds where it can.
                        None => crate::hir::types::super_fold_const(self, c),
                    }
                }
                other => other.clone(),
            }
        }
    }
    G { db, krate, def }.fold_type(ty)
}

/// Render an inline-verilog template: splices become the emitted port /
/// parameter names; const trees render as SV constant expressions.
/// Extract `EXPR` from a rendered `assign <lhs> = EXPR;` body (an `#[inline]`
/// fn's single-assign verilog). The LHS up to the first `=` is dropped; the
/// trailing `;` is stripped. `=` only appears as the assignment operator here
/// (SV `==`/`<=`/`>=` come after it, inside EXPR).
fn extract_assign_rhs(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix("assign").map(str::trim_start).unwrap_or(s);
    let rhs = match s.find('=') {
        Some(i) => &s[i + 1..],
        None => s,
    };
    rhs.trim().trim_end_matches(';').trim().to_owned()
}

fn render_verilog(template: &crate::hir::body::VerilogTemplate, sig: &Signature<'_>) -> String {
    let mut out = String::new();
    for seg in &template.segments {
        match seg {
            VerilogSegment::Text(t) => out.push_str(t),
            VerilogSegment::Param(l) => {
                let name = sig
                    .params
                    .iter()
                    .find(|p| p.local == *l)
                    .map(|p| p.name.as_str())
                    .unwrap_or("/*unknown*/");
                out.push_str(name);
            }
            VerilogSegment::Dom(i) => {
                let name = sig
                    .generic_params
                    .get(*i as usize)
                    .map(|g| g.name.as_str())
                    .unwrap_or("/*unknown*/");
                out.push_str(name);
            }
            VerilogSegment::ResultPort => out.push_str("result"),
            VerilogSegment::Const(c) => out.push_str(&render_const_sv(c, sig)),
        }
    }
    out
}

/// Combine an outer `when` guard with an inner condition (`g && c`), or just the
/// inner condition when there is no outer guard.
fn and_guard(outer: &Option<SvExpr>, inner: SvExpr) -> SvExpr {
    match outer {
        Some(g) => SvExpr::BinOp(SvBinOp::And, Box::new(g.clone()), Box::new(inner)),
        None => inner,
    }
}

/// A const tree as an SV constant expression (symbolic Params render as the
/// module's SV parameter names — legal in any constant context).
fn render_const_sv(c: &ConstArg<'_>, sig: &Signature<'_>) -> String {
    render_const_sv_generics(c, &sig.generic_params)
}

/// Render a const expression as a SystemVerilog constant — `Lit`s verbatim,
/// `Param`s as the emitted module's generic name, `Op`s as a parenthesized SV
/// expression — leaning on the SV elaborator to evaluate it. Used for both
/// widths/lengths and inline-verilog const splices. Only `sig.generic_params`
/// is needed, so this form takes the slice directly (callers in flattening have
/// the generics but not the whole signature).
///
/// Anything that survives to here but cannot be rendered (a `Local`/`Field`/
/// `Assoc` that `const_eval` failed to ground) is an internal invariant
/// violation, not a renderable form — panic rather than emit `/*unknown*/`,
/// which would silently produce malformed SV.
fn render_const_sv_generics(c: &ConstArg<'_>, generics: &[GenericParam]) -> String {
    match c {
        ConstArg::Lit(v) => v.to_string(),
        // A promoted localparam name — emit verbatim.
        ConstArg::Symbol(s) => s.clone(),
        ConstArg::Param(i) => generics
            .get(*i as usize)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| {
                panic!(
                    "const Param({i}) indexes no generic of the emitted module \
                     (it has {} generic params) on a clean crate — a foreign \
                     param leaked into rendering. Substitute it before \
                     flattening; do not emit a placeholder.",
                    generics.len(),
                )
            }),
        ConstArg::Op(op, a, b) => {
            let op = match op {
                ConstOp::Add => "+",
                ConstOp::Sub => "-",
                ConstOp::Mul => "*",
                ConstOp::Div => "/",
                ConstOp::Rem => "%",
            };
            format!(
                "({} {} {})",
                render_const_sv_generics(a, generics),
                op,
                render_const_sv_generics(b, generics)
            )
        }
        // A `Local`/`Field`/`Assoc` reaching here means `const_eval`/`ground_widths`
        // failed to ground a value that should have been concrete on a clean
        // crate. Surfacing it loudly beats emitting `/*unknown*/` into the SV.
        other => panic!(
            "render_const_sv: cannot render `{other:?}` as a SV constant on a \
             clean crate — it should have been grounded to a literal by \
             const_eval before emission. Do not emit a placeholder.",
        ),
    }
}

/// A fn that exists only for compile-time evaluation: it returns `integer`,
/// or every value param (of one or more) is `integer` (an out-param const
/// helper like `widths(n, => narrow, => wide)`).
fn is_const_only_fn(sig: &Signature<'_>) -> bool {
    if sig.return_type.as_ref().is_some_and(is_integer) {
        return true;
    }
    !sig.params.is_empty() && sig.params.iter().all(|p| is_integer(&p.ty))
}

/// Is this (resolved) type the compile-time-only `integer` scalar?
fn is_integer(ty: &Type<'_>) -> bool {
    matches!(
        ty,
        Type::Value {
            kind: ValueKind::Integer,
            ..
        }
    )
}

/// Render a literal in its source base: sized SV when the width is known
/// (`8'hFF`, `3'b101`), bare otherwise (hex keeps `'h` only with a width —
/// unsized SV hex needs one, so unknown-width hex falls back to decimal).
fn render_literal(v: i128, base: crate::hir::body::NumBase, width: Option<(u32, bool)>) -> String {
    use crate::hir::body::NumBase;
    // bits-typed literals default to hex; negatives stay decimal.
    let base = match (base, width) {
        (NumBase::Dec, Some((_, true))) if v >= 0 => NumBase::Hex,
        (b, _) => b,
    };
    match (base, width) {
        (NumBase::Hex, Some((w, _))) if v >= 0 => format!("{w}'h{v:X}"),
        (NumBase::Bin, Some((w, _))) if v >= 0 => format!("{w}'b{v:b}"),
        _ => v.to_string(),
    }
}

/// The def heading a concrete type (infer's `owner_of`, map-only form).
fn type_head_def<'db>(map: &CrateDefMap<'db>, ty: &Type<'db>) -> Option<DefId<'db>> {
    let prelude = |name: &str| map.resolve_local(map.prelude(), name, Namespace::Item);
    match ty {
        Type::Value {
            kind: ValueKind::UInt { .. },
            ..
        } => prelude("uint"),
        Type::Value {
            kind: ValueKind::SInt { .. },
            ..
        } => prelude("sint"),
        Type::Value {
            kind: ValueKind::Bits { .. },
            ..
        } => prelude("bits"),
        Type::Value {
            kind: ValueKind::Bool,
            ..
        } => prelude("bool"),
        Type::Value {
            kind: ValueKind::Integer,
            ..
        } => prelude("integer"),
        Type::Port { def, .. } => Some(*def),
        Type::Vec { .. } => prelude("Vec"),
        Type::Tuple(_) => prelude("Tuple"),
        Type::Clock => prelude("Clock"),
        _ => None,
    }
}

/// Apply a substitution to one Term (each kind through its own channel).
fn subst_term<'db>(t: &Term<'db>, subst: &[Option<Term<'db>>]) -> Term<'db> {
    match t {
        Term::Type(ty) => Term::Type(subst_type(ty, subst)),
        Term::Const(ConstArg::Param(i)) => match subst.get(*i as usize).and_then(|o| o.as_ref()) {
            Some(Term::Const(c)) => Term::Const(c.clone()),
            _ => t.clone(),
        },
        Term::Const(_) => t.clone(),
        Term::Domain(Domain::Param(i)) => match subst.get(*i as usize).and_then(|o| o.as_ref()) {
            Some(Term::Domain(d)) => Term::Domain(*d),
            _ => t.clone(),
        },
        Term::Domain(_) => t.clone(),
    }
}

fn subst_type<'db>(ty: &Type<'db>, subst: &[Option<Term<'db>>]) -> Type<'db> {
    let arg = |i: u32| subst.get(i as usize).and_then(|o| o.as_ref());
    match ty {
        Type::Value {
            kind: ValueKind::Param(i),
            ..
        } => match arg(*i) {
            Some(Term::Type(t)) => t.clone(),
            _ => ty.clone(),
        },
        // Substitute the width through `subst_const_opt`, which recurses into
        // nested `Param`s — not just a bare `width: Param(i)`, but the `Param`
        // inside an associated-const projection (`uint(T::width)` =
        // `uint(Assoc{ self_ty: Param(i) })`) or a const expression
        // (`uint(n + 1)`). Matching only the bare-`Param` form silently dropped
        // those substitutions and left a symbolic width that defaulted to 1 bit.
        Type::Value {
            kind: ValueKind::SInt { width },
            domain,
        } => Type::Value {
            kind: ValueKind::SInt {
                width: subst_const_opt(width, subst),
            },
            domain: *domain,
        },
        Type::Value {
            kind: ValueKind::Bits { width },
            domain,
        } => Type::Value {
            kind: ValueKind::Bits {
                width: subst_const_opt(width, subst),
            },
            domain: *domain,
        },
        Type::Value {
            kind: ValueKind::UInt { width },
            domain,
        } => Type::Value {
            kind: ValueKind::UInt {
                width: subst_const_opt(width, subst),
            },
            domain: *domain,
        },
        Type::Port { def, args, domain } => Type::Port {
            def: *def,
            args: subst_args(args, subst),
            domain: *domain,
        },
        Type::Vec { len, elem } => Type::Vec {
            len: subst_const_opt(len, subst),
            elem: Box::new(subst_type(elem, subst)),
        },
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| subst_type(e, subst)).collect()),
        other => other.clone(),
    }
}

fn subst_args<'db>(args: &GenericArgs<'db>, subst: &[Option<Term<'db>>]) -> GenericArgs<'db> {
    let arg = |i: u32| subst.get(i as usize).and_then(|o| o.as_ref());
    GenericArgs(
        args.0
            .iter()
            .map(|a| match a {
                Term::Type(t) => Term::Type(subst_type(t, subst)),
                Term::Const(ConstArg::Param(i)) => match arg(*i) {
                    Some(c @ Term::Const(_)) => c.clone(),
                    _ => a.clone(),
                },
                other => other.clone(),
            })
            .collect(),
    )
}

/// Join a base name with a field suffix using the `__` separator. An empty
/// suffix (a scalar leaf) leaves the base untouched.
fn join(base: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        base.to_owned()
    } else {
        format!("{base}__{suffix}")
    }
}

/// Render a parameter's `= default` source as an SV value: `high`/`true` →
/// `1'b1`, `low`/`false` → `1'b0`, a number verbatim, else the identifier.
fn default_value(text: &str) -> SvExpr {
    match text {
        "high" | "true" => SvExpr::Lit("1'b1".to_owned()),
        "low" | "false" => SvExpr::Lit("1'b0".to_owned()),
        n if n.parse::<i64>().is_ok() => SvExpr::Lit(n.to_owned()),
        other => SvExpr::Ident(other.to_owned()),
    }
}

/// Project a leaf suffix through a field access: `"payload"` under `.payload`
/// becomes the scalar leaf `""`; `"a__b"` under `.a` becomes `"b"`; a suffix for
/// a different field yields `None`.
fn strip_field(suffix: &str, field: &str) -> Option<String> {
    if suffix == field {
        Some(String::new())
    } else {
        suffix
            .strip_prefix(field)
            .and_then(|r| r.strip_prefix("__"))
            .map(str::to_owned)
    }
}

/// Lower a value type to an SV type. Concrete `uint(W)` → `[W-1:0]`; a symbolic
/// `uint(N)` where `N` is a Const-kind generic → `[N-1:0]` (an SV `parameter`
/// reference, rendered via `generics`); `bool`/`Reset`/`Clock` → 1-bit. A width
/// still unresolved (arithmetic / out-of-range index) falls back to 1-bit.
fn sv_type(ty: &Type, generics: &[GenericParam]) -> SvType {
    match ty {
        Type::Vec { len, elem } => {
            let mut t = sv_type(elem, generics);
            t.unpacked.insert(0, width_expr(len, generics));
            t
        }
        Type::Value {
            kind: ValueKind::Bits { width },
            ..
        } => SvType::uint(width_expr(width, generics)),
        Type::Value {
            kind: ValueKind::SInt { width },
            ..
        } => SvType::sint(width_expr(width, generics)),
        Type::Value {
            kind: ValueKind::UInt { width },
            ..
        } => SvType::uint(width_expr(width, generics)),
        // Genuinely 1-bit signals.
        Type::Value {
            kind: ValueKind::Bool | ValueKind::Reset | ValueKind::Event,
            ..
        } => SvType::bit(),
        // Everything else is unreachable for a WELL-TYPED crate: aggregates are
        // decomposed by `flatten_leaves`, type params are substituted at
        // monomorphisation, and compile-time `integer` is filtered before
        // emission. Emission only runs on a diagnostic-free crate (`sv_file`'s
        // `crate_emittable` gate), so reaching here is an internal invariant
        // violation, not bad input — surface it instead of silently emitting a
        // 1-bit logic (that default once masked a `uint(T::width)` → 1-bit
        // substitution bug).
        other => panic!(
            "sv_type cannot render `{}` on a clean crate; aggregates flatten via \
             flatten_leaves, type params substitute at monomorphisation, and \
             `integer` is compile-time only. This is an internal invariant \
             violation — handle the form explicitly, do not default to 1 bit.",
            describe_type(other),
        ),
    }
}

/// A label for a `Type` in a panic message (`Type` has no `Debug` — it holds
/// `DefId`s that need the db).
fn describe_type(ty: &Type) -> &'static str {
    match ty {
        Type::Value { kind, .. } => match kind {
            ValueKind::UInt { .. } => "uint",
            ValueKind::SInt { .. } => "sint",
            ValueKind::Bits { .. } => "bits",
            ValueKind::Bool => "bool",
            ValueKind::Reset => "reset",
            ValueKind::Event => "event",
            ValueKind::Integer => "integer (compile-time only)",
            ValueKind::Param(_) => "a type param (should be substituted at mono)",
        },
        Type::Vec { .. } => "Vec",
        Type::Tuple(_) => "a tuple (should be flattened)",
        Type::Port { .. } => "a record (should be flattened)",
        Type::Clock => "Clock (a domain witness, not a value)",
        Type::Infer(_) => "an unresolved inference variable",
        Type::Error => "Error (unresolved type)",
    }
}

/// The SV bit-width (or vector-length) expression for a const, or a panic. A
/// width is either a **literal** (concrete — `ground_widths` evaluates const
/// expressions and associated consts to a literal) or a bare generic **`Param`**
/// (a parametric module's own SV parameter, e.g. `[n-1:0]`). Anything else is a
/// bug or an unimplemented case and must NOT silently become a 1-bit type: that
/// default masked an associated-const substitution bug (`uint(T::width)` left
/// ungrounded — planning/pack_resize.md).
fn width_expr(c: &ConstArg, generics: &[GenericParam]) -> SvExpr {
    match c {
        ConstArg::Lit(w) => SvExpr::Lit(w.to_string()),
        // A promoted body local: render as its `localparam` name (`[w-1:0]`).
        ConstArg::Symbol(s) => SvExpr::Ident(s.clone()),
        // A `Param` indexes one of the emitted module's own generics — a
        // symbolic SV parameter (`[n-1:0]`). An OUT-OF-RANGE index means a
        // foreign type's param leaked into this module's rendering (e.g.
        // `emit_instance` flattening a callee type against the caller's
        // generics). `emit_instance` now substitutes the callee's params first,
        // so on a clean crate this cannot happen.
        ConstArg::Param(i) => match generics.get(*i as usize) {
            Some(g) => SvExpr::Ident(g.name.clone()),
            None => panic!(
                "width/length Param({i}) indexes no generic of the emitted module \
                 (it has {} generic params) on a clean crate — a foreign param \
                 leaked into rendering. Substitute it before flattening; do not \
                 default to 1 bit.",
                generics.len(),
            ),
        },
        // A symbolic COMPOUND width/length in a parametric module — `uint(n + 1)`,
        // `Vec(a + b, …)`, or an expanded assoc body (`Vec(N,A)::bit_size` →
        // `N * 8`). `const_eval`/`ground_widths` cannot ground it (its generics
        // are free here), so render it as a SV constant expression and let the
        // SV elaborator evaluate it: `[(n + 1)-1:0]`. NEVER a silent 1-bit/`[0:0]`
        // default. `render_const_sv_generics` panics on a genuinely unrenderable
        // form (an ungrounded `Local`/`Assoc`).
        ConstArg::Op(..) => SvExpr::Lit(render_const_sv_generics(c, generics)),
        // A `Local`/`Field`/`Assoc` width: `ground_widths` grounds these to a
        // literal via const_eval before rendering (that grounding fixed the
        // `uint(T::width)` → 1-bit bug), so one surviving here is an internal
        // invariant violation — surface it loudly, never a 1-bit default.
        other => panic!(
            "width/length `{other:?}` is neither a literal, a generic param, nor \
             a const expression on a clean crate. A concrete width must be \
             grounded to a literal by const_eval before emission. Not a 1-bit \
             default.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;

    fn emit(src: &str) -> String {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src.to_owned());
        let krate = vfs.source_root(&mut db, "t.mrn");
        verilog(&db, krate).clone()
    }

    #[test]
    fn scalar_combinational_fn_emits_matching_verilog() {
        // The `add_constant.mrn` shape — parity with mirin-compiler's output.
        let sv = emit(
            "fn addConstant { dom clk: Clock } ( value: uint(8) @clk ) -> uint(8) @clk {\n  let bumped = value + 3;\n  bumped\n}",
        );
        let expected = "\
module addConstant (
    input  logic clk,
    input  logic [7:0] value,
    output logic [7:0] result
);
    logic [7:0] bumped;
    assign bumped = (value + 3);
    assign result = bumped;
endmodule
";
        assert_eq!(sv, expected, "\n--- got ---\n{sv}");
    }

    #[test]
    fn symbolic_compound_vec_length_renders_not_collapsed() {
        // A Vec whose length is a symbolic COMPOUND const expr (`a + b`, both
        // free generic params) must render the dimension as a SV expression,
        // leaning on the elaborator — NOT silently collapse to `[0:0]` (a
        // wrong-width miscompile). Regression for the silent-coercion bug.
        let sv = emit(
            "fn f (const a: integer, const b: integer, v: Vec(a + b, uint(8))) -> uint(8) {\n  v[0]\n}",
        );
        assert!(sv.contains("logic [7:0] v [0:(a + b)-1]"), "{sv}");
        assert!(
            !sv.contains("v [0:0]"),
            "collapsed to a 1-element array: {sv}"
        );
    }

    #[test]
    fn symbolic_compound_scalar_width_renders_not_panics() {
        // The sibling case: a symbolic compound scalar width (`uint(a + b)`)
        // must render `[(a + b)-1:0]`, not panic (it used to) and not default to
        // 1 bit.
        let sv = emit(
            "fn g (const a: integer, const b: integer, v: uint(a + b)) -> uint(a + b) {\n  v\n}",
        );
        assert!(sv.contains("logic [(a + b)-1:0] v"), "{sv}");
        assert!(sv.contains("logic [(a + b)-1:0] result"), "{sv}");
    }

    #[test]
    fn a_bare_return_drives_result() {
        let sv = emit("fn id (x: uint(4)) -> uint(4) { return x; }");
        assert!(sv.contains("input  logic [3:0] x"), "{sv}");
        assert!(sv.contains("output logic [3:0] result"), "{sv}");
        assert!(sv.contains("assign result = x;"), "{sv}");
    }

    #[test]
    fn a_returned_ports_consumer_field_is_an_input() {
        // A returned PORT is bidirectional: its `out` fields are module
        // outputs, but its `in` field (ready) is a module INPUT — the
        // downstream's backpressure — and the value's own place is driven
        // FROM it (reverse). Regression for the result-emission direction.
        let sv = emit(
            "port S = s { out valid: bool, out data: uint(8), in ready: bool }\n\
             fn tap {dom clk: Clock} (up: S @clk) -> S @clk { up }",
        );
        assert!(sv.contains("output logic result__valid"), "{sv}");
        assert!(sv.contains("output logic [7:0] result__data"), "{sv}");
        assert!(sv.contains("input  logic result__ready"), "{sv}");
        // up's consumer-side ready (a module output) is driven from the
        // returned port's ready input — the reverse direction.
        assert!(sv.contains("assign up__ready = result__ready;"), "{sv}");
        assert!(sv.contains("assign result__valid = up__valid;"), "{sv}");
    }

    #[test]
    fn a_unit_fn_tail_call_emits_its_instance() {
        // A unit-returning fn whose TAIL is a side-effecting call (no
        // semicolon) must still instantiate the callee — otherwise the call,
        // and the drives it carries (`self.ready = true`), silently vanish.
        // Regression: the tail routed to drive_result, which bailed with no
        // return type.
        let sv = emit(
            "port S = s { out valid: bool, in ready: bool }\n\
             impl {dom clk: Clock} S { fn sink(self @clk) { self.ready = true; } }\n\
             fn top {dom clk: Clock} (up: S @clk) { up.sink() }",
        );
        assert!(sv.contains("S__sink"), "callee must be instantiated:\n{sv}");
        assert!(sv.contains(".self__ready(up__ready)"), "{sv}");
    }

    #[test]
    fn when_lowers_to_a_resetless_always_ff() {
        let sv = emit(
            "fn counter { dom clk: Clock } () -> uint(8) @clk {\n  var count: uint(8) @clk;\n  count = when clk.posedge() { count + 1 };\n  count\n}",
        );
        // `count = when …` registers the LOCAL directly — no synthetic, no
        // continuous assign — so `init count = …` would take effect.
        assert!(sv.contains("    logic [7:0] count;"), "{sv}");
        assert!(sv.contains("    always_ff @(posedge clk) begin"), "{sv}");
        assert!(sv.contains("count <= (count + 1);"), "{sv}");
        assert!(sv.contains("assign result = count;"), "{sv}");
        // No reset branch on a `when`.
        assert!(!sv.contains("if (!"), "{sv}");
    }

    #[test]
    fn let_mut_fold_lowers_to_a_procedural_always_comb() {
        // A loop-carried `let mut` accumulator (proposals/compile_mutable.md)
        // becomes a procedural always_comb with a mutable var and a procedural
        // `for` — not a continuous assign + generate-for.
        let sv = emit(
            "fn sum { dom clk: Clock } (v: Vec(4, uint(8)) @clk) -> uint(8) @clk {\n  let mut acc = 0;\n  for x in v {\n    acc = acc + x;\n  }\n  acc\n}",
        );
        assert!(sv.contains("always_comb begin"), "{sv}");
        assert!(sv.contains("acc = 0;"), "{sv}");
        assert!(sv.contains("< 4;") && sv.contains("for (int "), "{sv}");
        assert!(sv.contains("acc = (acc + x);"), "{sv}");
        assert!(sv.contains("assign result = acc;"), "{sv}");
        // Not the structural generate-for path.
        assert!(!sv.contains("genvar"), "{sv}");
    }

    #[test]
    fn let_mut_fold_supports_aggregate_accumulators() {
        // A tuple accumulator folds per leaf — init and carry both flatten to
        // `acc__0`/`acc__1` (no silent scalarisation of the aggregate).
        let sv = emit(
            "fn f { dom clk: Clock } (v: Vec(4, uint(8)) @clk) -> uint(8) @clk {\n  let mut acc = (0, 0);\n  for x in v {\n    acc = (acc.0 + x, acc.1 + x);\n  }\n  acc.0 + acc.1\n}",
        );
        assert!(sv.contains("logic [7:0] acc__0;"), "{sv}");
        assert!(sv.contains("logic [7:0] acc__1;"), "{sv}");
        assert!(
            sv.contains("acc__0 = 0;") && sv.contains("acc__1 = 0;"),
            "{sv}"
        );
        assert!(sv.contains("acc__0 = (acc__0 + x);"), "{sv}");
        // No silent array→0 scalarisation.
        assert!(!sv.contains("acc = 0;"), "{sv}");
    }

    #[test]
    fn statement_when_lowers_to_the_inferred_bram_idiom() {
        // Statement-form `when` binding (proposals/when_binding.md): a guarded
        // index drive becomes `if (we) mem[waddr] <= wdata;` in one always_ff,
        // with `init { … }` as the power-on initial block.
        let sv = emit(
            "fn ram { dom clk: Clock } (waddr: uint(2) @clk, wdata: uint(8) @clk, we: bool @clk, raddr: uint(2) @clk) -> uint(8) @clk {\n  var mem: Vec(4, uint(8)) @clk;\n  init { mem = [0; 4]; }\n  when clk.posedge() {\n    if we { mem[waddr] = wdata; }\n  }\n  mem[raddr]\n}",
        );
        assert!(sv.contains("logic [7:0] mem [0:3];"), "{sv}");
        assert!(sv.contains("initial begin"), "{sv}");
        assert!(sv.contains("always_ff @(posedge clk) begin"), "{sv}");
        assert!(sv.contains("if (we) mem[waddr] <= wdata;"), "{sv}");
        assert!(sv.contains("assign result = mem[raddr];"), "{sv}");
        // A `when` register has no reset branch.
        assert!(!sv.contains("if (!"), "{sv}");
    }

    #[test]
    fn reg_lowers_to_always_ff_with_reset_and_shadowed_lets_uniquify() {
        let sv = emit(
            "fn pipeline { dom clk: Clock, rstn: Reset @clk } ( data: uint(8) @clk ) -> uint(8) @clk {\n  let data = (data + 1).reg(rstn, 0);\n  let data = (data * 2).reg(rstn, 0);\n  return data;\n}",
        );
        assert!(sv.contains("    logic [7:0] data_1;"), "{sv}");
        assert!(sv.contains("    logic [7:0] data_2;"), "{sv}");
        assert!(sv.contains("always_ff @(posedge clk) begin"), "{sv}");
        assert!(sv.contains("if (!rstn) begin"), "{sv}");
        assert!(sv.contains("data_1 <= 0;"), "{sv}");
        assert!(sv.contains("data_1 <= (data + 1);"), "{sv}");
        assert!(sv.contains("data_2 <= (data_1 * 2);"), "{sv}");
        assert!(sv.contains("assign result = data_2;"), "{sv}");
    }

    #[test]
    fn struct_param_reg_and_return_flatten_per_field() {
        // The `packet_struct.mrn` shape: a struct param/return erase to
        // per-field ports; `inp.reg(rstn, packet{…})` is a per-field register
        // (note the `false` init renders `1'b0`); `return held` drives each
        // result field. Byte-parity with mirin-compiler.
        let sv = emit(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn registerPacket { dom clk: Clock, rstn: Reset @clk = high } ( inp: Packet @clk ) -> Packet @clk {\n\
               let held = inp.reg(rstn, packet { valid = false, payload = 0 });\n\
               return held;\n\
             }",
        );
        let expected = "\
module registerPacket (
    input  logic clk,
    input  logic rstn,
    input  logic inp__valid,
    input  logic [7:0] inp__payload,
    output logic result__valid,
    output logic [7:0] result__payload
);
    logic held__valid;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__valid <= 1'b0;
        end else begin
            held__valid <= inp__valid;
        end
    end
    logic [7:0] held__payload;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            held__payload <= 0;
        end else begin
            held__payload <= inp__payload;
        end
    end
    assign result__valid = held__valid;
    assign result__payload = held__payload;
endmodule
";
        assert_eq!(sv, expected, "\n--- got ---\n{sv}");
    }

    #[test]
    fn port_equation_flattens_with_per_field_direction() {
        // The `simple_port.mrn` shape: a port param flattens per field with the
        // module direction folding param + field direction; `downstream =
        // upstream` becomes one connection per field, the `in` field flowing the
        // other way. Byte-parity with mirin-compiler.
        let sv = emit(
            "port Stream8 = stream8 { out valid: bool, out data: uint(8), in ready: bool }\n\
             fn connectStream { dom clk: Clock } ( upstream: Stream8 @clk, out downstream: Stream8 @clk ) {\n\
               downstream = upstream;\n\
             }",
        );
        let expected = "\
module connectStream (
    input  logic clk,
    input  logic upstream__valid,
    input  logic [7:0] upstream__data,
    output logic upstream__ready,
    output logic downstream__valid,
    output logic [7:0] downstream__data,
    input  logic downstream__ready
);
    assign downstream__valid = upstream__valid;
    assign downstream__data = upstream__data;
    assign upstream__ready = downstream__ready;
endmodule
";
        assert_eq!(sv, expected, "\n--- got ---\n{sv}");
    }

    #[test]
    fn field_access_and_record_literal_flatten() {
        // The `reg2` shape (no instantiation): `a.payload` projects to a leaf,
        // the returned `option { … }` record drives each result field.
        let sv = emit(
            "struct Option = option { valid: bool, payload: uint(8) }\n\
             fn reg2 { dom clk: Clock } ( a: Option @clk, rstn: Reset @clk ) -> Option @clk {\n\
               let payloadd = a.payload.reg(rstn, 0);\n\
               return option { valid = a.valid, payload = payloadd };\n\
             }",
        );
        assert!(sv.contains("    logic [7:0] payloadd;"), "{sv}");
        assert!(sv.contains("payloadd <= a__payload;"), "{sv}");
        assert!(sv.contains("assign result__valid = a__valid;"), "{sv}");
        assert!(sv.contains("assign result__payload = payloadd;"), "{sv}");
    }

    #[test]
    fn user_fn_call_becomes_a_submodule_instance() {
        // `let x = add3(x)` instantiates add3, wiring its result to the binding;
        // a nested call value goes through a synthetic `__call_N`.
        let sv = emit(
            "fn add3 (x: uint(8)) -> uint(8) { return x + 3; }\n\
             fn add9 (x: uint(8)) -> uint(8) { let x = add3(x); return add3(x); }",
        );
        // The let binds the second `x` (uniquified to x_1), driven by the instance.
        assert!(sv.contains("    logic [7:0] x_1;"), "{sv}");
        assert!(sv.contains("    add3 add3 ("), "{sv}");
        assert!(sv.contains(".x(x)"), "{sv}");
        assert!(sv.contains(".result(x_1)"), "{sv}");
        // A second instance drives `result` directly from the return.
        assert!(sv.contains("    add3 add3_1 ("), "{sv}");
        assert!(sv.contains(".result(result)"), "{sv}");
    }

    #[test]
    fn out_arg_connection_becomes_an_instance() {
        // A void user call with an out-arg (`downstream => ds`) instantiates the
        // callee, binding its `out` param to the (implicit-`var`) target `ds`.
        let sv = emit(
            "struct Option = option { valid: bool, payload: uint(8) }\n\
             fn snk { dom clk: Clock, out downstream: Option @clk } ( in upstream: Option @clk ) {\n\
               downstream = upstream;\n\
             }\n\
             fn top { dom clk: Clock } ( in upstream: Option @clk, out downstream: Option @clk ) {\n\
               snk{downstream => ds}(upstream);\n\
               snk{downstream => downstream}(ds);\n\
             }",
        );
        // `ds` is a fresh implicit var, declared once before the first instance.
        assert_eq!(sv.matches("logic ds__valid;").count(), 1, "{sv}");
        assert!(sv.contains("    snk snk ("), "{sv}");
        assert!(sv.contains(".downstream__valid(ds__valid)"), "{sv}");
        assert!(sv.contains("    snk snk_1 ("), "{sv}");
        assert!(sv.contains(".upstream__valid(ds__valid)"), "{sv}");
    }

    #[test]
    fn impl_method_call_instantiates_a_qualified_module() {
        // `upstream.reg(rstn)` resolves to `Option::reg` → an `Option__reg`
        // instance with the receiver wired to the `self__…` ports.
        let sv = emit(
            "struct Option = option { valid: bool, payload: uint(8) }\n\
             impl Option {\n\
               fn reg { dom clk: Clock } (self @clk, rstn: Reset @clk) -> Option @clk {\n\
                 let payloadd = self.payload.reg(rstn, 0);\n\
                 option { valid = self.valid, payload = payloadd }\n\
               }\n\
             }\n\
             fn use_it { dom clk: Clock, rstn: Reset @clk } ( in upstream: Option @clk, out downstream: Option @clk ) {\n\
               downstream = upstream.reg(rstn);\n\
             }",
        );
        // The method becomes a module named after its owner.
        assert!(sv.contains("module Option__reg ("), "{sv}");
        assert!(sv.contains("input  logic self__valid,"), "{sv}");
        assert!(sv.contains("    logic [7:0] payloadd;"), "{sv}");
        assert!(sv.contains("payloadd <= self__payload;"), "{sv}");
        // The call site instantiates it, wiring `self` from `upstream`.
        assert!(sv.contains("    Option__reg Option__reg ("), "{sv}");
        assert!(sv.contains(".self__valid(upstream__valid)"), "{sv}");
        assert!(sv.contains(".result__valid(downstream__valid)"), "{sv}");
    }

    #[test]
    fn const_generic_becomes_an_sv_parameter() {
        // `const n: integer` → `#(parameter int n)`, and `uint(n)` → `[n-1:0]`.
        // Byte-parity with mirin-compiler on the `add_n` shape.
        let sv = emit(
            "fn add_n { dom clk: Clock } ( const n: integer, a: uint(n) @clk, b: uint(n) @clk ) -> uint(n) @clk {\n  return a + b;\n}",
        );
        let expected = "\
module add_n #(parameter int n) (
    input  logic clk,
    input  logic [n-1:0] a,
    input  logic [n-1:0] b,
    output logic [n-1:0] result
);
    assign result = (a + b);
endmodule
";
        assert_eq!(sv, expected, "\n--- got ---\n{sv}");
    }

    #[test]
    fn parametric_struct_args_substitute_at_flatten() {
        // `Bus(uint(8))` substitutes `A := uint(8)` into the `data: A` field, so
        // the port flattens to `b__data` of width [7:0] (not 1-bit).
        let sv = emit(
            "struct Bus(type A) = bus { valid: bool, data: A }\n\
             fn pipeline { dom clk: Clock } ( b: Bus(uint(8)) @clk ) -> Bus(uint(8)) @clk {\n  return b;\n}",
        );
        assert!(sv.contains("input  logic b__valid,"), "{sv}");
        assert!(sv.contains("input  logic [7:0] b__data,"), "{sv}");
        assert!(sv.contains("output logic [7:0] result__data"), "{sv}");
        assert!(sv.contains("assign result__data = b__data;"), "{sv}");
        // No SV parameter — the type arg is concrete.
        assert!(!sv.contains("#("), "{sv}");
    }

    #[test]
    fn equal_width_obligation_becomes_an_initial_assert() {
        // `a: uint(n)`, `b: uint(m)`, `a + b` forces `n == m` — an undecidable
        // width equality discharged as an `initial assert`. Byte-parity.
        let sv = emit(
            "fn pair_add { dom clk: Clock } ( const n: integer, const m: integer, a: uint(n) @clk, b: uint(m) @clk ) -> uint(n) @clk {\n  return a + b;\n}",
        );
        let expected = "\
module pair_add #(parameter int n, parameter int m) (
    input  logic clk,
    input  logic [n-1:0] a,
    input  logic [m-1:0] b,
    output logic [n-1:0] result
);
    assign result = (a + b);
    initial begin
        assert ((m == n));
    end
endmodule
";
        assert_eq!(sv, expected, "\n--- got ---\n{sv}");
    }

    #[test]
    fn type_generic_fn_is_monomorphised_per_concrete_type() {
        // A type-generic `fn pass{ type A }(w: Bus(A))` is not emitted
        // directly; a call at `Bus(Write)` emits a specialised `pass__Write`
        // module (struct args substituted) and instantiates it. A defaulted,
        // unsupplied param wires its default at the instance.
        let sv = emit(
            "struct Bus(type A) = bus { valid: bool, data: A }\n\
             struct Write = write { addr: uint(8), data: uint(8) }\n\
             fn pass { dom clk: Clock, rstn: Reset @clk = high, type A } ( w: Bus(A) @clk ) -> Bus(A) @clk { w }\n\
             fn top { dom clk: Clock } ( w: Bus(Write) @clk ) -> Bus(Write) @clk { pass(w) }",
        );
        // The generic `pass` is not emitted; its `Write` specialisation is.
        assert!(!sv.contains("module pass ("), "{sv}");
        assert!(!sv.contains("module pass #("), "{sv}");
        assert!(sv.contains("module pass__Write ("), "{sv}");
        // The specialised module flattens `Bus(Write)` fully.
        assert!(sv.contains("input  logic [7:0] w__data__addr,"), "{sv}");
        // `top` instantiates it, defaulting the omitted `rstn` to `1'b1`.
        assert!(sv.contains("    pass__Write pass__Write ("), "{sv}");
        assert!(sv.contains(".rstn(1'b1)"), "{sv}");
        assert!(sv.contains(".w__data__addr(w__data__addr)"), "{sv}");
    }

    #[test]
    fn parametric_port_width_substitutes_use_site_arg() {
        // `Buf{clk}(8)` binds the port's `param N` to 8, so `data: uint(N)`
        // flattens to width [7:0] — no parameter on the using module.
        let sv = emit(
            "port Buf { dom clk: Clock } ( const N: integer ) = buf { in ready: bool @clk, out data: uint(N) @clk }\n\
             fn pipe { dom clk: Clock } ( upstream: Buf{clk}(8), out downstream: Buf{clk}(8) ) {\n  downstream = upstream;\n}",
        );
        assert!(sv.contains("input  logic [7:0] upstream__data,"), "{sv}");
        assert!(sv.contains("output logic [7:0] downstream__data"), "{sv}");
        assert!(
            sv.contains("assign downstream__data = upstream__data;"),
            "{sv}"
        );
    }

    #[test]
    fn if_expression_lowers_to_always_comb() {
        let sv = emit(
            "fn pickOne (a: uint(8), b: uint(8), cond: bool) -> uint(8) {\n  if cond { a } else { b }\n}",
        );
        assert!(sv.contains("    logic [7:0] __block_0;"), "{sv}");
        assert!(sv.contains("    always_comb begin"), "{sv}");
        assert!(sv.contains("if (cond) begin"), "{sv}");
        assert!(sv.contains("__block_0 = a;"), "{sv}");
        assert!(sv.contains("__block_0 = b;"), "{sv}");
        assert!(sv.contains("assign result = __block_0;"), "{sv}");
    }
}
