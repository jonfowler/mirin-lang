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
    SvGenerateFor, SvInstance, SvItem, SvLogicDecl, SvModule, SvPort, SvPortDirection, SvSeqAssign,
    SvType,
};
use crate::base::db::SourceRoot;
use crate::hir::body::{
    Block, Body, ConnArg, ExprId, ExprKind, LocalKind, NamedArg, RecordField, Stmt, VerilogSegment,
    body,
};
use crate::hir::check::{check_drivers, completeness, directions};
use crate::hir::infer::{Inference, infer};
use crate::hir::sig::{Signature, sig_of};
use crate::hir::types::{
    ConstArg, ConstOp, Direction, Domain, Folder, GenericArgs, GenericParam, LocalId, Term,
    TermKind, Type, ValueKind, subst_const_opt,
};
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
    let mut lower = SvLower {
        db,
        krate,
        def,
        map,
        body,
        inf,
        sig,
        self_subst: self_subst.to_vec(),
        local_names: unique_local_names(body),
        items: Vec::new(),
        synth: 0,
        index_asserts: std::collections::HashSet::new(),
        instance_counts: HashMap::new(),
        declared: HashSet::new(),
        mono_reqs: Vec::new(),
    };
    if let Some(template) = body.verilog() {
        lower
            .items
            .push(SvItem::Verbatim(render_verilog(template, sig)));
    } else {
        lower.lower_top_block(body.block());
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
}

impl<'db> SvLower<'_, 'db> {
    /// The function body: statements, then the tail expression drives `result`.
    fn lower_top_block(&mut self, block: &Block) {
        self.lower_stmts(&block.stmts);
        if let Some(tail) = block.tail {
            self.drive_result(tail);
        }
    }

    fn lower_stmts(&mut self, stmts: &[Stmt]) {
        let mut i = 0;
        while i < stmts.len() {
            // A `let mut acc = init;` immediately followed by `for` loop(s) that
            // reassign it is a loop-carried fold → one procedural `always_comb`
            // (proposals/compile_mutable.md), not a continuous assign + a
            // structural generate-for. Consume the run together.
            if let Stmt::Let { local, value } = &stmts[i]
                && self.body.local(*local).mutable
            {
                let (acc, init) = (*local, *value);
                // The contiguous run of statements that reassign `acc` — a
                // straight-line `acc = …` or a carrying `for`. (A statement that
                // only reads `acc`, or anything else, ends the run; the mid-read
                // fold is a later refinement.)
                let mut steps: Vec<Stmt> = Vec::new();
                let mut j = i + 1;
                while let Some(stmt) = stmts.get(j) {
                    let carries = match stmt {
                        Stmt::Equation { lhs, .. } => {
                            backend_root_local(self.body, *lhs) == Some(acc)
                        }
                        Stmt::For { body, .. } => self.for_carries(body, acc),
                        _ => false,
                    };
                    if !carries {
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

    fn lower_one_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { local, value } => self.lower_let(*local, *value),
            Stmt::VarDecl { local } => self.declare_local(*local),
            Stmt::Equation { lhs, rhs } => {
                if let ExprKind::Local(l) = self.body.expr(*lhs).kind
                    && self.is_integer_local(l)
                {
                    return;
                }
                self.lower_equation(*lhs, *rhs)
            }
            Stmt::Return { value } => self.drive_result(*value),
            Stmt::For {
                index,
                elem,
                iter,
                body,
            } => {
                let (index, elem, iter) = (*index, *elem, *iter);
                let body = body.clone();
                self.lower_for(index, elem, iter, &body);
            }
            // A bare call statement: a (void) submodule instantiation whose
            // out-arg connections bind callee `out` params to caller places.
            Stmt::Expr(e) => self.lower_call_stmt(*e),
            Stmt::When { event, body, init } => {
                let (event, body) = (*event, body.clone());
                let init = init.clone();
                self.lower_when_stmt(event, &body, init.as_ref());
            }
        }
    }

    /// Does this `for` body reassign `acc` (a loop-carried fold)?
    fn for_carries(&self, body: &Block, acc: LocalId) -> bool {
        body.stmts.iter().any(|s| {
            matches!(s, Stmt::Equation { lhs, .. } if backend_root_local(self.body, *lhs) == Some(acc))
        })
    }

    /// Lower `let mut acc = init;` + carrying `for`(s) to one procedural
    /// `always_comb`: `acc = init;` then a procedural `for` per loop whose body
    /// reassigns `acc` with blocking assignments (the synthesiser unrolls; the
    /// recurrence rides procedural execution order, like LLVM after mem2reg).
    /// First cut: scalar accumulator and scalar element.
    fn lower_mut_fold(&mut self, acc: LocalId, init: ExprId, steps: &[Stmt]) {
        self.declare_local(acc);
        let mut comb: Vec<SvCombStmt> = Vec::new();
        // The init `acc = …;` per leaf — handles scalars, a Vec (`'{…}`), and
        // multi-leaf tuples/structs (`acc__0 = …; acc__1 = …;`), consistent
        // with the carry's per-leaf assigns.
        let acc_leaves = self.local_leaves(acc);
        let init_leaves = self.expr_leaves(init);
        for ((_, lp), (_, rv)) in acc_leaves.into_iter().zip(init_leaves) {
            comb.push(SvCombStmt::Assign { lhs: lp, rhs: rv });
        }
        for step in steps {
            match step {
                // A straight-line reassignment `acc = …;` → a blocking assign.
                Stmt::Equation { lhs, rhs } => {
                    for a in self.blocking_assigns(*lhs, *rhs) {
                        comb.push(a);
                    }
                }
                // A carrying `for` → a procedural `for` with blocking body.
                Stmt::For {
                    index,
                    elem,
                    iter,
                    body,
                } => {
                    let Some((bound, var)) = self.loop_bound_var(*index, *elem, *iter) else {
                        continue;
                    };
                    let mut inner: Vec<SvCombStmt> = Vec::new();
                    // Element binding `x = v[i];` unless the element IS the
                    // genvar (a `range(n)` loop).
                    let elem_is_genvar = matches!(
                        self.body.local(*elem).kind,
                        crate::hir::body::LocalKind::ForBound
                    );
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
                    for stmt in &body.stmts {
                        if let Stmt::Equation { lhs, rhs } = stmt {
                            for a in self.blocking_assigns(*lhs, *rhs) {
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

    /// Per-leaf blocking assignments for one `lhs = rhs` equation (used inside a
    /// procedural fold `always_comb`).
    fn blocking_assigns(&mut self, lhs: ExprId, rhs: ExprId) -> Vec<SvCombStmt> {
        let lhs_leaves = self.place_leaves_dir(lhs);
        let rhs_leaves = self.value_leaves_dir(rhs);
        lhs_leaves
            .into_iter()
            .zip(rhs_leaves)
            .map(|((lp, _), (rp, _))| SvCombStmt::Assign { lhs: lp, rhs: rp })
            .collect()
    }

    /// The (bound, genvar-name) for a loop's iterable, mirroring `lower_for`.
    fn loop_bound_var(
        &mut self,
        index: Option<LocalId>,
        elem: LocalId,
        iter: ExprId,
    ) -> Option<(SvExpr, String)> {
        let it = self
            .inf
            .expr_type(iter)
            .cloned()
            .map(|t| {
                ground_widths(
                    self.db,
                    self.krate,
                    self.def,
                    &subst_type(&t, &self.self_subst),
                )
            })
            .unwrap_or(Type::Error);
        let len = match &it {
            Type::Vec { len, .. } => len.clone(),
            Type::Value {
                kind: ValueKind::Bits { width },
                ..
            } => width.clone(),
            _ => return None,
        };
        let bound = match &len {
            ConstArg::Lit(n) => SvExpr::Lit(n.to_string()),
            ConstArg::Param(i) => match self.sig.generic_params.get(*i as usize) {
                Some(g) => SvExpr::Ident(g.name.clone()),
                None => SvExpr::Lit("0".to_owned()),
            },
            other => SvExpr::Lit(render_const_sv(other, self.sig)),
        };
        let elem_is_genvar = matches!(
            self.body.local(elem).kind,
            crate::hir::body::LocalKind::ForBound
        );
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

    /// Statement-form `when` (proposals/when_binding.md): the body's drives
    /// become nonblocking assignments in one `always_ff` — `if`-guarded drives
    /// render as `if (g) leaf <= d;` (unwritten/false → the register holds, no
    /// else). An optional `init` block becomes an SV `initial` (power-on, not
    /// reset). The register IS the driven `var`'s own leaves.
    fn lower_when_stmt(&mut self, event: ExprId, body: &Block, init: Option<&Block>) {
        let clock = self.clock_of_event(event);
        if let Some(init) = init {
            let mut assigns = Vec::new();
            for stmt in &init.stmts {
                if let Stmt::Equation { lhs, rhs } = stmt {
                    let lhs_leaves = self.place_leaves_dir(*lhs);
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

    /// Flatten a `when` body into guarded nonblocking assignments. Each `if`
    /// narrows the guard (`g && cond` on the then-branch, `g && !cond` on the
    /// else-branch); unwritten leaves hold by virtue of being a register.
    fn when_body_seq(&mut self, block: &Block, guard: Option<SvExpr>, seq: &mut Vec<SvSeqAssign>) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Equation { lhs, rhs } => {
                    let lhs_leaves = self.place_leaves_dir(*lhs);
                    let rhs_leaves = self.value_leaves_dir(*rhs);
                    for ((lp, _), (rp, _)) in lhs_leaves.into_iter().zip(rhs_leaves) {
                        seq.push(SvSeqAssign {
                            lhs: lp,
                            rhs: rp,
                            guard: guard.clone(),
                        });
                    }
                }
                Stmt::Expr(e) => {
                    if let ExprKind::If {
                        cond,
                        then_branch,
                        else_branch,
                    } = &self.body.expr(*e).kind
                    {
                        let (cond, then_b, else_b) =
                            (*cond, then_branch.clone(), else_branch.clone());
                        let c = self.expr_value(cond);
                        self.when_body_seq(&then_b, Some(and_guard(&guard, c.clone())), seq);
                        if !else_b.stmts.is_empty() {
                            // `!c` as `c == 1'b0` (SvExpr has no logical not).
                            let not_c = SvExpr::BinOp(
                                SvBinOp::Eq,
                                Box::new(c),
                                Box::new(SvExpr::Lit("1'b0".to_owned())),
                            );
                            self.when_body_seq(&else_b, Some(and_guard(&guard, not_c)), seq);
                        }
                    }
                }
                Stmt::Let { local, value } => self.lower_let(*local, *value),
                _ => {}
            }
        }
    }

    /// `for x in v { … }` → a NAMED generate-for (planning/for_loops.md):
    /// the genvar is the (elided, integer-typed) index local; the elem local
    /// is an ordinary per-iteration binding `assign x = v[i];` inside the
    /// block, so hierarchy is recoverable as `label[i].name`.
    fn lower_for(
        &mut self,
        index: Option<LocalId>,
        elem: LocalId,
        iter: ExprId,
        body: &crate::hir::body::Block,
    ) {
        // The loop bound, from the iterable's type.
        let it = self
            .inf
            .expr_type(iter)
            .cloned()
            .map(|t| {
                ground_widths(
                    self.db,
                    self.krate,
                    self.def,
                    &subst_type(&t, &self.self_subst),
                )
            })
            .unwrap_or(Type::Error);
        let (len, is_bits) = match &it {
            Type::Vec { len, .. } => (len.clone(), false),
            Type::Value {
                kind: ValueKind::Bits { width },
                ..
            } => (width.clone(), true),
            _ => return,
        };
        let bound = match &len {
            ConstArg::Lit(n) => SvExpr::Lit(n.to_string()),
            ConstArg::Param(i) => match self.sig.generic_params.get(*i as usize) {
                Some(g) => SvExpr::Ident(g.name.clone()),
                None => SvExpr::Lit("0".to_owned()),
            },
            other => SvExpr::Lit(render_const_sv(other, self.sig)),
        };
        // A `range(n)` iterable never materialises: the ELEM local is the
        // genvar itself and no binding is emitted. Keyed off the SYNTACTIC
        // range call (lowering marks its elem ForBound) — the type alone
        // (Vec(N, integer)) is not enough: `[3, 1, 2]` has the same type
        // but its values are not 0..N-1 (infer rejects those for now).
        let elem_is_genvar = matches!(
            self.body.local(elem).kind,
            crate::hir::body::LocalKind::ForBound
        );
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

        // Body items collect into the generate block.
        let saved = std::mem::take(&mut self.items);
        // The elem binding: `assign x = v[i];` per leaf (or the bit).
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

    /// `let x = value;`. A builtin `.reg` makes the let the register itself
    /// (per field); otherwise the local's leaves are declared and each driven by
    /// the corresponding leaf of the value.
    fn lower_let(&mut self, local: LocalId, value: ExprId) {
        if self.is_integer_local(local) {
            return;
        }
        if let Some(reg) = self.as_reg(value) {
            // A register is typed by its D-input (also covers a target whose own
            // type inference left unknown).
            let leaves = self.expr_type_leaves(reg.d_input);
            let base = self.local_name(local);
            let clock = self.clock_of_type(self.inf.local_type(local));
            self.emit_registers(&base, &leaves, reg, clock, true);
            return;
        }
        // `let x = f(args)` — `x` is the callee's (flattened) result.
        if let Some(uc) = self.as_user_call(value) {
            self.declare_local(local);
            let target = self.local_leaves(local);
            self.emit_instance(uc, target);
            return;
        }
        self.declare_local(local);
        let target = self.local_leaves(local);
        let value_leaves = self.expr_leaves(value);
        // Match leaves by suffix, not position: a record's `=>` fields are
        // absent from its forward leaves, which would shift a positional zip.
        for (suf, place) in target {
            if let Some((_, v)) = value_leaves.iter().find(|(s, _)| *s == suf) {
                self.push_assign(place, v.clone());
            }
        }
        let base = self.local_name(local);
        for (suf, target) in self.record_out_conns(value) {
            self.push_assign(target, SvExpr::Ident(join(&base, &suf)));
        }
    }

    /// A driving equation `lhs = rhs`, per field. A builtin `.reg` on the RHS
    /// makes the (already-declared) LHS local the register; otherwise each field
    /// is a connection whose sink is chosen by the leaves' module direction.
    fn lower_equation(&mut self, lhs: ExprId, rhs: ExprId) {
        if let (ExprKind::Local(l), Some(reg)) = (&self.body.expr(lhs).kind, self.as_reg(rhs)) {
            let l = *l;
            let leaves = self.local_type_leaves(l);
            let base = self.local_name(l);
            let clock = self.clock_of_type(self.inf.local_type(l));
            self.emit_registers(&base, &leaves, reg, clock, false);
            return;
        }
        // `mem = when E { tail };` — the local IS the register: always_ff
        // directly on its leaves (no synthetic, no continuous assign), so
        // `init mem = …` (an SV initial block) actually takes effect and
        // the RAM idiom stays one array (planning/when_ram.md).
        if let ExprKind::Local(l) = self.body.expr(lhs).kind
            && let ExprKind::When { event, body, init } = &self.body.expr(rhs).kind
        {
            let (event, b, init) = (*event, body.clone(), *init);
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
        // `place = f(args)` — the callee's result drives `place`.
        if let ExprKind::Local(l) = self.body.expr(lhs).kind
            && let Some(uc) = self.as_user_call(rhs)
        {
            let target = self.local_leaves(l);
            self.emit_instance(uc, target);
            return;
        }
        if let ExprKind::Local(l) = self.body.expr(lhs).kind {
            let base = self.local_name(l);
            for (suf, target) in self.record_out_conns(rhs) {
                self.push_assign(target, SvExpr::Ident(join(&base, &suf)));
            }
            // A record RHS assigns suffix-matched (its `=>` fields are absent
            // from the forward leaves — a positional zip would shift).
            if matches!(self.body.expr(rhs).kind, ExprKind::Record { .. }) {
                let target = self.local_leaves(l);
                let value_leaves = self.expr_leaves(rhs);
                for (suf, place) in target {
                    if let Some((_, v)) = value_leaves.iter().find(|(s, _)| *s == suf) {
                        self.push_assign(place, v.clone());
                    }
                }
                return;
            }
        }
        let lhs_leaves = self.place_leaves_dir(lhs);
        let rhs_leaves = self.value_leaves_dir(rhs);
        for ((lp, ld), (rp, rd)) in lhs_leaves.into_iter().zip(rhs_leaves) {
            // The leaf the body drives is the sink (LHS of the `assign`).
            let (sink, src) = match (ld, rd) {
                (true, _) => (lp, rp),
                (false, true) => (rp, lp),
                (false, false) => (lp, rp),
            };
            self.push_assign(sink, src);
        }
    }

    /// A side-effecting call in statement position (`f();`, or the tail/`return`
    /// of a unit-returning fn): a (void) submodule instantiation whose out-arg
    /// connections bind callee `out` params to caller places. A non-call
    /// statement expression has no effect and is dropped.
    fn lower_call_stmt(&mut self, e: ExprId) {
        if let Some(uc) = self.as_user_call(e) {
            if self.is_const_only_call(&uc) {
                return; // results reached via const_eval
            }
            self.declare_out_targets(&uc);
            self.emit_instance(uc, Vec::new());
        }
    }

    /// Drive `result` (the return port) per field from `value`'s leaves.
    fn drive_result(&mut self, value: ExprId) {
        let Some(rt) = self.sig.return_type.clone() else {
            // No result port: a unit-returning fn whose tail (or `return`) is a
            // side-effecting call still needs its instance emitted. Without
            // this the call — and the drives it carries (`self.ready = …`) —
            // silently vanishes.
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
        if let Some(uc) = self.as_user_call(value) {
            let target = result_leaves
                .into_iter()
                .map(|l| (l.suffix.clone(), SvExpr::Ident(join("result", &l.suffix))))
                .collect();
            self.emit_instance(uc, target);
            return;
        }
        let value_leaves = self.expr_leaves(value);
        for rl in result_leaves {
            if let Some((_, v)) = value_leaves.iter().find(|(s, _)| *s == rl.suffix) {
                let result_leaf = SvExpr::Ident(join("result", &rl.suffix));
                if rl.drives {
                    // A produced (`out`) field: the result drives the value.
                    self.push_assign(result_leaf, v.clone());
                } else {
                    // A consumer-side (`in`) field of a returned port: the
                    // module RECEIVES `result__<field>`, and the value's own
                    // place (e.g. `up__ready`) is driven from it — the reverse
                    // direction, like a record `field => target` binding.
                    self.push_assign(v.clone(), result_leaf);
                }
            }
        }
        for (suf, target) in self.record_out_conns(value) {
            self.push_assign(target, SvExpr::Ident(join("result", &suf)));
        }
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
        self.local_names[local.0 as usize].clone()
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
            ground_widths(self.db, self.krate, self.def, &t)
        })
    }

    /// A literal expr's GROUND uint width, if its inferred type has one —
    /// drives sized SV literal forms (`8'hFF`).
    /// A DYNAMIC (uint-typed) index gets a simulation-time bounds assert
    /// (`always_comb assert (sel < 3);`) unless the width provably cannot
    /// express an out-of-range value (2^w ≤ N — note a non-power-of-two
    /// length always leaves a gap). Synthesis ignores it; simulation fires
    /// at the access. planning/vectors.md.
    fn index_bounds_assert(&mut self, base: ExprId, index: ExprId, idx_sv: &SvExpr) {
        let Some(it) = self.inf.expr_type(index).cloned() else {
            return;
        };
        let it = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(&it, &self.self_subst),
        );
        let Type::Value {
            kind: ValueKind::UInt { width: iw },
            ..
        } = it
        else {
            return; // static (integer/literal) indexes are checked in infer
        };
        let Some(bt) = self.inf.expr_type(base).cloned() else {
            return;
        };
        let bt = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(&bt, &self.self_subst),
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
        let len_sv = match &len {
            ConstArg::Lit(n) => n.to_string(),
            ConstArg::Param(i) => self
                .sig
                .generic_params
                .get(*i as usize)
                .map(|g| g.name.clone())
                .unwrap_or_else(|| "0".to_owned()),
            other => render_const_sv(other, self.sig),
        };
        let cond = format!("{idx_sv} < {len_sv}");
        if self.index_asserts.insert(cond.clone()) {
            self.items.push(SvItem::CombAssert(SvCombAssert { cond }));
        }
    }

    /// The bool is "prefer hex": bits-typed literals print hex by default
    /// (planning/bits.md).
    fn expr_type_width(&mut self, expr: ExprId) -> Option<(u32, bool)> {
        let t = self.inf.expr_type(expr)?.clone();
        let t = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(&t, &self.self_subst),
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

    /// rustc's `Instance::resolve`, minimal form: a call recorded against a
    /// trait method DECL is re-selected to the unique matching impl once the
    /// self type is concrete (after applying this module's own mono subst).
    /// Returns the impl method plus its substitution: the header binding for
    /// the impl's binder prefix, then the decl's own generic args.
    fn resolve_trait_instance(
        &self,
        expr: ExprId,
        decl: DefId<'db>,
    ) -> Option<(DefId<'db>, Vec<Option<Term<'db>>>)> {
        if !self.map.is_trait_method_decl(decl) {
            return None;
        }
        let trait_def = self.map.def_data(decl)?.owner?;
        let decl_subst = self.inf.call_subst(expr)?;
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

    /// A statement-position call is const-only when the callee's every value
    /// param and return are `integer` — nothing of it is hardware; its outputs
    /// are reached by `const_eval` through the width trees.
    fn is_const_only_call(&self, uc: &UserCall<'db>) -> bool {
        is_const_only_fn(sig_of(self.db, self.krate, uc.def))
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

    /// The scalar leaves (suffix + type) of an expression's type. Mirrors
    /// [`Self::expr_leaves`] but yields field types — used to type a register
    /// from its D-input (a register is the same type as what feeds it), which
    /// also covers `self.field` where `self` is untyped in inference.
    fn expr_type_leaves(&self, expr: ExprId) -> Vec<Leaf> {
        match &self.body.expr(expr).kind {
            ExprKind::Local(l) => self.local_type_leaves(*l),
            ExprKind::Field { receiver, field } => self
                .expr_type_leaves(*receiver)
                .into_iter()
                .filter_map(|leaf| {
                    strip_field(&leaf.suffix, field).map(|rest| Leaf {
                        suffix: rest,
                        ..leaf
                    })
                })
                .collect(),
            ExprKind::Record { ctor, .. } => {
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
                ty: self.expr_type(expr),
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

    /// The leaves of an equation place (`lhs`/`rhs`): a local carries its
    /// direction; anything else is a single driven scalar.
    fn place_leaves_dir(&mut self, e: ExprId) -> Vec<(SvExpr, bool)> {
        match self.body.expr(e).kind {
            ExprKind::Local(l) => self.local_leaves_dir(l),
            // An indexed place fans out per element-type leaf
            // (`whole[i] = x` → whole__valid[i], whole__val[i], …).
            ExprKind::Index { .. } => self
                .expr_leaves(e)
                .into_iter()
                .map(|(_, v)| (v, true))
                .collect(),
            _ => vec![(self.expr_value(e), true)],
        }
    }

    /// The leaves of an equation's RHS value: a local carries its direction;
    /// anything else (field access, record) is flattened as a driven source.
    fn value_leaves_dir(&mut self, e: ExprId) -> Vec<(SvExpr, bool)> {
        match self.body.expr(e).kind {
            ExprKind::Local(l) => self.local_leaves_dir(l),
            _ => self
                .expr_leaves(e)
                .into_iter()
                .map(|(_, v)| (v, true))
                .collect(),
        }
    }

    /// An expression's SV type, falling back to 1-bit.
    fn expr_type(&self, expr: ExprId) -> SvType {
        self.inf
            .expr_type(expr)
            .map(|t| {
                let t = ground_widths(self.db, self.krate, self.def, t);
                sv_type(&t, &self.sig.generic_params)
            })
            .unwrap_or_else(SvType::bit)
    }

    /// `Some(reg)` if `expr` is a `e.reg(rst, init)` method call.
    fn as_reg(&self, expr: ExprId) -> Option<RegCall> {
        match &self.body.expr(expr).kind {
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } if method == "reg" && args.len() == 2 => Some(RegCall {
                d_input: *receiver,
                reset: args[0].expr,
                init: args[1].expr,
            }),
            _ => None,
        }
    }

    /// Emit one `always_ff @(posedge clock)` per leaf of a `.reg` target,
    /// synchronous active-low reset. The D-input and init are flattened in the
    /// same field order as the target's leaves. `declare` emits each leaf's
    /// `logic` immediately before its block (the let form); an equation form
    /// leaves declaration to the preceding `var`.
    fn emit_registers(
        &mut self,
        base: &str,
        leaves: &[Leaf],
        reg: RegCall,
        clock: String,
        declare: bool,
    ) {
        let reset = self.reset_name(reg.reset);
        let d = self.expr_leaves(reg.d_input);
        let init = self.expr_leaves(reg.init);
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

    /// The reset signal's name (a bare ident, else its rendered form).
    fn reset_name(&mut self, reset: ExprId) -> String {
        match self.expr_value(reset) {
            SvExpr::Ident(s) => s,
            other => other.to_string(),
        }
    }

    /// Emit a single scalar register into an already-declared `target`.
    fn emit_reg(&mut self, target: String, clock: String, reg: RegCall) {
        let leaf = Leaf {
            suffix: String::new(),
            ty: SvType::bit(),
            drives: true,
        };
        self.emit_registers(&target, std::slice::from_ref(&leaf), reg, clock, false);
    }

    fn fresh_block(&mut self) -> String {
        let n = self.synth;
        self.synth += 1;
        format!("__block_{n}")
    }

    /// Lower an expression to its SV value, emitting any items its evaluation
    /// requires (registers / combinational blocks for `reg`/`when`/`if`).
    fn expr_value(&mut self, expr: ExprId) -> SvExpr {
        // An `#[inline]` call splices its body in place (planning/attributes.md).
        if let Some(uc) = self.inline_call(expr) {
            return self.render_inline(uc);
        }
        // A user call in scalar value position: instantiate, take the one leaf.
        if let Some(uc) = self.as_user_call(expr) {
            return self
                .call_value_leaves(uc)
                .into_iter()
                .next()
                .map(|(_, e)| e)
                .unwrap_or_else(|| SvExpr::Lit("0".to_owned()));
        }
        match &self.body.expr(expr).kind {
            // A literal emits in its source base; when its type carries a
            // ground width, sized SV form (`8'hFF`) — planning/numeric_literals.md L6.
            ExprKind::Number(n, base) => {
                SvExpr::Lit(render_literal(*n, *base, self.expr_type_width(expr)))
            }
            ExprKind::TypedLiteral { value, base, .. } => {
                SvExpr::Lit(render_literal(*value, *base, self.expr_type_width(expr)))
            }
            ExprKind::Bool(b) => SvExpr::Lit(if *b { "1'b1" } else { "1'b0" }.to_owned()),
            ExprKind::Local(l) => SvExpr::Ident(self.local_name(*l)),
            // A const generic used as a value renders as the SV `#(…)` parameter
            // name — legal in widths, bounds, and ordinary expressions alike.
            ExprKind::ConstParam(i) => match self.sig.generic_params.get(*i as usize) {
                Some(g) => SvExpr::Ident(g.name.clone()),
                None => SvExpr::Lit("0".to_owned()),
            },
            ExprKind::Call { .. } => {
                // User-fn calls become module instances (Q5d).
                SvExpr::Lit("0".to_owned())
            }
            // `v[i]` in scalar position.
            ExprKind::Index { base, index } => {
                let (base, index) = (*base, *index);
                let b = self.expr_value(base);
                let i = self.expr_value(index);
                self.index_bounds_assert(base, index, &i);
                SvExpr::Lit(format!("{b}[{i}]"))
            }
            // An operator desugar (`a + b` → `a.add(b)`) that selected a
            // prelude impl: inline SV operator.
            ExprKind::MethodCall { receiver, args, .. }
                if self.prelude_op(expr).is_some() && args.len() == 1 =>
            {
                let op = self.prelude_op(expr).unwrap();
                let l = self.expr_value(*receiver);
                let r = self.expr_value(args[0].expr);
                SvExpr::BinOp(op, Box::new(l), Box::new(r))
            }
            // `-x` (Neg on sint) / `!x` (Not on bool): inline unary operator.
            ExprKind::MethodCall { receiver, args, .. }
                if args.is_empty() && self.prelude_unary(expr).is_some() =>
            {
                let op = self.prelude_unary(expr).unwrap();
                let x = self.expr_value(*receiver);
                SvExpr::Lit(format!("({op}{x})"))
            }
            // `e.reg(rst, init)` in value position: a register into a fresh local.
            ExprKind::MethodCall { .. } if self.as_reg(expr).is_some() => {
                let reg = self.as_reg(expr).unwrap();
                let synth = self.fresh_block();
                let ty = self.expr_type(expr);
                self.items.push(SvItem::Logic(SvLogicDecl {
                    ty,
                    name: synth.clone(),
                }));
                let clock = self.clock_of_type(self.inf.expr_type(reg.d_input));
                self.emit_reg(synth.clone(), clock, reg);
                SvExpr::Ident(synth)
            }
            ExprKind::When { event, body, init } => {
                let (event, init) = (*event, *init);
                let body = body.clone();
                self.lower_when(expr, event, &body, init)
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond = *cond;
                let then_branch = then_branch.clone();
                let else_branch = else_branch.clone();
                self.lower_if(expr, cond, &then_branch, &else_branch)
            }
            ExprKind::Block(b) => {
                let b = b.clone();
                self.block_value(&b)
            }
            // An aggregate or field/record in scalar value position: reduce to
            // its single SV leaf if it has one (a scalar, or a Vec → `'{…}`).
            // A value that flattens to SEVERAL signals (a tuple, a multi-field
            // struct, a Vec-of-struct) has no scalar form — `one_leaf` emits a
            // marker that fails downstream rather than silently dropping leaves
            // or scalarising an array to `0`.
            ExprKind::VecLit(_)
            | ExprKind::TupleLit(_)
            | ExprKind::VecRepeat { .. }
            | ExprKind::Field { .. }
            | ExprKind::Record { .. } => self.one_leaf(expr),
            // Defensive only: every shape that lands here (user method calls
            // pending Q5d-2, malformed exprs) is diagnosed by `infer`, and the
            // CLI refuses to emit SV when any diagnostic exists — so this
            // placeholder never reaches written output.
            _ => SvExpr::Lit("0".to_owned()),
        }
    }

    /// Reduce an expression to a single scalar SV value. A value that flattens
    /// to one SV signal (a scalar, or a Vec → `'{…}` assignment pattern) is
    /// returned directly; one that flattens to several (a tuple, a multi-field
    /// struct/port) has no scalar representation, so emit a marker that fails
    /// downstream (verilator) — never a silent `0`, which would scalarise an
    /// aggregate. (`expr_leaves` resolves these constructors itself, so this
    /// does not recurse back through `expr_value`'s fallback.)
    fn one_leaf(&mut self, expr: ExprId) -> SvExpr {
        let mut leaves = self.expr_leaves(expr);
        if leaves.len() == 1 {
            leaves.pop().unwrap().1
        } else {
            SvExpr::Lit(format!(
                "/* mirin: non-scalar value ({} leaves) in scalar position */",
                leaves.len()
            ))
        }
    }

    /// An expression's scalar leaves as `(suffix, value)`, in struct-field order.
    /// Aggregates expand (a struct local → one leaf per field, a field access
    /// projects, a record literal rebuilds); scalars are a single empty-suffix
    /// leaf via [`Self::expr_value`].
    fn expr_leaves(&mut self, expr: ExprId) -> Vec<(String, SvExpr)> {
        // An `#[inline]` call splices its (scalar) body in place. Aggregate
        // inline results are future work (planning/attributes.md).
        if let Some(uc) = self.inline_call(expr) {
            return vec![(String::new(), self.render_inline(uc))];
        }
        // A user call in value position instantiates into a fresh `__call_N`.
        if let Some(uc) = self.as_user_call(expr) {
            return self.call_value_leaves(uc);
        }
        match &self.body.expr(expr).kind {
            ExprKind::Local(l) => self.local_leaves(*l),
            ExprKind::Field { receiver, field } => {
                let receiver = *receiver;
                let field = field.clone();
                self.expr_leaves(receiver)
                    .into_iter()
                    .filter_map(|(suf, e)| strip_field(&suf, &field).map(|rest| (rest, e)))
                    .collect()
            }
            // `[a, b, c]` per element-type leaf: an SV assignment pattern
            // over the elements' corresponding leaves.
            ExprKind::VecLit(elems) => {
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
            // `(a, b)`: element leaves prefixed with their index — the
            // anonymous-struct shape (planning/tuples.md).
            ExprKind::TupleLit(elems) => {
                let elems = elems.clone();
                let mut out = Vec::new();
                for (i, e) in elems.iter().enumerate() {
                    for (suf, v) in self.expr_leaves(*e) {
                        out.push((join(&i.to_string(), &suf), v));
                    }
                }
                out
            }
            // `[e; N]`: SV replication pattern per leaf.
            ExprKind::VecRepeat { elem, len } => {
                let len = len.clone();
                let n =
                    match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, &len) {
                        Some(v) => v.to_string(),
                        None => render_const_sv(&len, self.sig),
                    };
                self.expr_leaves(*elem)
                    .into_iter()
                    .map(|(suffix, e)| (suffix, SvExpr::Lit(format!("'{{{n}{{{e}}}}}"))))
                    .collect()
            }
            // `v[i]`: each base leaf, selected.
            ExprKind::Index { base, index } => {
                let (base, index) = (*base, *index);
                let idx = self.expr_value(index);
                self.index_bounds_assert(base, index, &idx);
                self.expr_leaves(base)
                    .into_iter()
                    .map(|(suffix, e)| (suffix, SvExpr::Lit(format!("{e}[{idx}]"))))
                    .collect()
            }
            // An aggregate-valued `if`: per-leaf mux into a synthetic.
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let (cond, tb, eb) = (*cond, then_branch.clone(), else_branch.clone());
                let synth = self.fresh_block();
                let c = self.expr_value(cond);
                let then_leaves = self.block_leaves(&tb);
                let else_leaves = self.block_leaves(&eb);
                let tys = self.expr_leaf_types(expr);
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
            // An aggregate-valued `when`: per-leaf register of the tail.
            ExprKind::When { event, body, init } => {
                let (event, b, init) = (*event, body.clone(), *init);
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
                let d_leaves = self.block_leaves(&b);
                let tys = self.expr_leaf_types(expr);
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
            // `v.replace(i, x)`: a combinational copy with one element
            // swapped — `__repl = v; __repl[i] = x;` per leaf.
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } if method == "replace" && args.len() == 2 => {
                let (receiver, i_e, x_e) = (*receiver, args[0].expr, args[1].expr);
                let synth = self.fresh_block();
                let idx = self.expr_value(i_e);
                self.index_bounds_assert(receiver, i_e, &idx);
                let recv_leaves = self.expr_leaves(receiver);
                let x_leaves = self.expr_leaves(x_e);
                let tys = self.expr_leaf_types(receiver);
                let mut out = Vec::new();
                let mut body = Vec::new();
                for (k, (suffix, rv)) in recv_leaves.into_iter().enumerate() {
                    let name = join(&synth, &suffix);
                    let ty = tys.get(k).cloned().unwrap_or_else(SvType::bit);
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
            ExprKind::Record { ctor, fields } => {
                let ctor = *ctor;
                let fields = fields.clone();
                self.record_leaves(ctor, &fields)
            }
            _ => vec![(String::new(), self.expr_value(expr))],
        }
    }

    /// The leaves of a record literal, in the constructor's declared field order
    /// (each field's value flattened and prefixed with the field name).
    fn record_leaves(
        &mut self,
        ctor: Option<DefId<'db>>,
        fields: &[RecordField],
    ) -> Vec<(String, SvExpr)> {
        // The constructor's fields live on the struct/port it is owned by.
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
            if let Some(rf) = fields.iter().find(|rf| &rf.name == fname) {
                if rf.out {
                    continue; // `field => target` flows the other way
                }
                for (suf, e) in self.expr_leaves(rf.value) {
                    out.push((join(fname, &suf), e));
                }
            }
        }
        out
    }

    /// A record constructor's `field => target` connections as
    /// `(field_suffix, target_place)` pairs — the constructed value's field
    /// drives the target (`assign target = <base>__field`).
    fn record_out_conns(&mut self, expr: ExprId) -> Vec<(String, SvExpr)> {
        let ExprKind::Record { fields, .. } = &self.body.expr(expr).kind else {
            return Vec::new();
        };
        let fields = fields.clone();
        let mut out = Vec::new();
        for rf in &fields {
            if !rf.out {
                continue;
            }
            for (tsuf, target) in self.expr_leaves(rf.value) {
                out.push((join(&rf.name, &tsuf), target));
            }
        }
        out
    }

    /// `when ev { … d }` → a reset-less `always_ff @(posedge <ev-clock>)` whose
    /// single clocked assignment drives a synthetic `__block_N` with the body's
    /// tail value `d`. The expression's value is that held register output.
    fn lower_when(
        &mut self,
        when_expr: ExprId,
        event: ExprId,
        body: &Block,
        init: Option<ExprId>,
    ) -> SvExpr {
        let synth = self.fresh_block();
        let ty = self.expr_type(when_expr);
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

    /// `if c { a } else { b }` → an `always_comb` mux driving a synthetic
    /// `__block_N`; the expression's value is that local.
    fn lower_if(
        &mut self,
        if_expr: ExprId,
        cond: ExprId,
        then_branch: &Block,
        else_branch: &Block,
    ) -> SvExpr {
        let synth = self.fresh_block();
        let ty = self.expr_type(if_expr);
        self.items.push(SvItem::Logic(SvLogicDecl {
            ty,
            name: synth.clone(),
        }));
        let cond = self.expr_value(cond);
        let then_v = self.block_value(then_branch);
        let else_v = self.block_value(else_branch);
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

    /// Lower a block's statements (combinationally) and return its tail value.
    /// A block's value as leaves (statements lower; the tail fans out).
    fn block_leaves(&mut self, block: &Block) -> Vec<(String, SvExpr)> {
        self.lower_stmts(&block.stmts);
        match block.tail {
            Some(tail) => self.expr_leaves(tail),
            None => Vec::new(),
        }
    }

    /// The SV types of an expression's leaves, in leaf order.
    fn expr_leaf_types(&mut self, expr: ExprId) -> Vec<SvType> {
        let Some(t) = self.inf.expr_type(expr).cloned() else {
            return Vec::new();
        };
        let t = ground_widths(
            self.db,
            self.krate,
            self.def,
            &subst_type(&t, &self.self_subst),
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

    fn block_value(&mut self, block: &Block) -> SvExpr {
        self.lower_stmts(&block.stmts);
        match block.tail {
            Some(tail) => self.expr_value(tail),
            None => SvExpr::Lit("0".to_owned()),
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

    /// The clock of a `when` event `clk.posedge()` — the receiver local's name.
    fn clock_of_event(&self, event: ExprId) -> String {
        if let ExprKind::MethodCall { receiver, .. } = &self.body.expr(event).kind
            && let ExprKind::Local(l) = &self.body.expr(*receiver).kind
        {
            return self.local_name(*l);
        }
        self.first_clock()
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

    /// `Some(prefix)` if this method call selected a prelude UNARY operator
    /// impl (`Neg` → `-`, `Not` → `!`) — emitted inline as `(-x)` / `(!x)`,
    /// never as an instance (the unary twin of `prelude_op`).
    fn prelude_unary(&self, expr: ExprId) -> Option<&'static str> {
        let def = self.inf.method_resolution(expr)?;
        let t = self.map.trait_of_method(def)?;
        let tdata = self.map.def_data(t)?;
        if tdata.module != self.map.prelude() {
            return None;
        }
        match tdata.name.as_str() {
            "Neg" => Some("-"),
            "Not" => Some("!"),
            "BitNot" => Some("~"),
            _ => None,
        }
    }

    /// `Some(op)` if a method call resolved to a PRELUDE operator-trait impl
    /// method — codegen's one operator special case (rustc's enforce_builtin_
    /// binop shape): the selection is real trait dispatch, but the emission
    /// is the inline SV operator, never an instance.
    fn prelude_op(&self, expr: ExprId) -> Option<SvBinOp> {
        let def = self.inf.method_resolution(expr)?;
        let t = self.map.trait_of_method(def)?;
        let tdata = self.map.def_data(t)?;
        if tdata.module != self.map.prelude() {
            return None;
        }
        // Key on the resolved METHOD name, not the trait: one trait (`Ord`)
        // backs four ordering operators, `Eq` backs `eq`/`ne`.
        match self.map.def_data(def)?.name.as_str() {
            "add" => Some(SvBinOp::Add),
            "sub" => Some(SvBinOp::Sub),
            "mul" => Some(SvBinOp::Mul),
            "div" => Some(SvBinOp::Div),
            "rem" => Some(SvBinOp::Rem),
            "eq" => Some(SvBinOp::Eq),
            "ne" => Some(SvBinOp::Ne),
            "lt" => Some(SvBinOp::Lt),
            "le" => Some(SvBinOp::Le),
            "gt" => Some(SvBinOp::Gt),
            "ge" => Some(SvBinOp::Ge),
            "and" => Some(SvBinOp::And),
            "or" => Some(SvBinOp::Or),
            "bitand" => Some(SvBinOp::BitAnd),
            "bitor" => Some(SvBinOp::BitOr),
            "bitxor" => Some(SvBinOp::BitXor),
            "shl" => Some(SvBinOp::Shl),
            // `>>` is arithmetic (sign-extending) on a sint receiver, logical
            // on uint/bits (planning/operators.md O3).
            "shr" => Some(if self.receiver_is_signed(expr) {
                SvBinOp::AShr
            } else {
                SvBinOp::Shr
            }),
            _ => None,
        }
    }

    /// True if a method call's receiver has a signed integer type.
    fn receiver_is_signed(&self, expr: ExprId) -> bool {
        let ExprKind::MethodCall { receiver, .. } = &self.body.expr(expr).kind else {
            return false;
        };
        matches!(
            self.inf.expr_type(*receiver),
            Some(Type::Value {
                kind: ValueKind::SInt { .. },
                ..
            })
        )
    }

    // ----- #[inline] body splicing (planning/attributes.md) -----

    /// `Some(call)` if `expr` is a call to an `#[inline]` fn/method (prelude or
    /// user) — the body is spliced at the call site instead of instantiating a
    /// module. Operators and `.reg` are handled by their own inline paths.
    fn inline_call(&self, expr: ExprId) -> Option<UserCall<'db>> {
        match &self.body.expr(expr).kind {
            ExprKind::Call {
                callee,
                args,
                named,
            } => {
                let ExprKind::Def(def) = self.body.expr(*callee).kind else {
                    return None;
                };
                if !self.map.def_data(def)?.inline {
                    return None;
                }
                Some(UserCall {
                    def,
                    expr,
                    subst_override: None,
                    receiver: None,
                    args: args.clone(),
                    named: named.clone(),
                })
            }
            ExprKind::MethodCall { receiver, args, .. }
                if self.prelude_op(expr).is_none()
                    && self.prelude_unary(expr).is_none()
                    && self.as_reg(expr).is_none() =>
            {
                let decl = self.inf.method_resolution(expr)?;
                let (def, subst_override) = match self.resolve_trait_instance(expr, decl) {
                    Some((m, ov)) => (m, Some(ov)),
                    None => (decl, None),
                };
                if !self.map.def_data(def)?.inline {
                    return None;
                }
                Some(UserCall {
                    def,
                    expr,
                    subst_override,
                    receiver: Some(*receiver),
                    args: args.clone(),
                    named: Vec::new(),
                })
            }
            // `uint(8)::unpack(b)` — a receiver-less associated call. Same as a
            // method call minus the receiver (planning/pack_resize.md).
            ExprKind::TypePathCall { args, .. } => {
                let decl = self.inf.method_resolution(expr)?;
                let (def, subst_override) = match self.resolve_trait_instance(expr, decl) {
                    Some((m, ov)) => (m, Some(ov)),
                    None => (decl, None),
                };
                if !self.map.def_data(def)?.inline {
                    return None;
                }
                Some(UserCall {
                    def,
                    expr,
                    subst_override,
                    receiver: None,
                    args: args.clone(),
                    named: Vec::new(),
                })
            }
            _ => None,
        }
    }

    /// Splice an `#[inline]` call's body as an SV expression. v1 supports a
    /// verilog body of the single-assign shape `assign ${result} = EXPR;`: the
    /// callee's value params resolve to the call's argument expressions, its
    /// const generics to the call's instantiation, and `EXPR` is returned
    /// (parenthesised) for the caller to use in place.
    fn render_inline(&mut self, uc: UserCall<'db>) -> SvExpr {
        let csig = sig_of(self.db, self.krate, uc.def);
        let Some(template) = body(self.db, self.krate, uc.def).verilog().cloned() else {
            // Non-verilog #[inline] bodies are not supported in v1.
            return SvExpr::Lit("0".to_owned());
        };

        // Value params → the caller's argument expressions (positional zip with
        // `[receiver?] ++ args`, named by name), keyed by the param's local.
        let mut positional: Vec<ExprId> = uc.receiver.into_iter().collect();
        positional.extend(uc.args.iter().map(|a| a.expr));
        let mut pos_i = 0;
        let mut val_map: HashMap<LocalId, String> = HashMap::new();
        for p in &csig.params {
            let caller_expr = if p.from_named_section {
                uc.named.iter().find(|n| n.name == p.name).map(|n| n.expr)
            } else {
                let e = positional.get(pos_i).copied();
                pos_i += 1;
                e
            };
            let rendered = match caller_expr {
                Some(e) => self.expr_value(e).to_string(),
                None => match &p.default {
                    Some(d) => default_value(d).to_string(),
                    None => continue,
                },
            };
            val_map.insert(p.local, rendered);
        }

        // Const generics bind from the call's recorded instantiation, rendered
        // in the caller's terms (a ground value evaluates, a symbolic one
        // renders against the caller's SV parameters) — as `emit_instance` does.
        let node_subst: Vec<Option<Term<'db>>> = match &uc.subst_override {
            Some(ov) => ov.clone(),
            None => self
                .inf
                .call_subst(uc.expr)
                .map(|ts| ts.iter().cloned().map(Some).collect())
                .unwrap_or_default(),
        };

        let mut out = String::new();
        for seg in &template.segments {
            match seg {
                VerilogSegment::Text(t) => out.push_str(t),
                // The LHS `${result}` is dropped when we extract the RHS.
                VerilogSegment::ResultPort => out.push_str("result"),
                VerilogSegment::Param(l) => {
                    out.push_str(val_map.get(l).map(String::as_str).unwrap_or("0"));
                }
                VerilogSegment::Dom(_) => out.push_str(&self.first_clock()),
                VerilogSegment::Const(c) => {
                    let c = subst_const_opt(c, &node_subst);
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
        SvExpr::Lit(format!("({})", extract_assign_rhs(&out)))
    }

    // ----- instantiation (user calls / methods → submodules) -----

    /// `Some(call)` if `expr` is a user `fn` call or a resolved user method call
    /// (not a prelude operator and not the builtin `.reg`).
    fn as_user_call(&self, expr: ExprId) -> Option<UserCall<'db>> {
        match &self.body.expr(expr).kind {
            ExprKind::Call {
                callee,
                args,
                named,
            } => {
                let ExprKind::Def(def) = self.body.expr(*callee).kind else {
                    return None;
                };
                let data = self.map.def_data(def)?;
                if data.module == self.map.prelude()
                    || data.inline
                    || !matches!(data.kind, DefKind::Fn | DefKind::Method)
                {
                    return None;
                }
                Some(UserCall {
                    def,
                    expr,
                    subst_override: None,
                    receiver: None,
                    args: args.clone(),
                    named: named.clone(),
                })
            }
            // A prelude operator selection is NOT a user call — it lowers
            // inline (`(a + b)`, `(-x)`), never as an instance.
            ExprKind::MethodCall { .. }
                if self.prelude_op(expr).is_some() || self.prelude_unary(expr).is_some() =>
            {
                None
            }
            ExprKind::MethodCall { receiver, args, .. } if self.as_reg(expr).is_none() => {
                {
                    let decl = self.inf.method_resolution(expr)?;
                    // A trait-method DECL (picked through a `T: Trait` bound)
                    // re-selects to the matching impl with the now-concrete
                    // self type — rustc's Instance::resolve at mono time.
                    let (def, subst_override) = match self.resolve_trait_instance(expr, decl) {
                        Some((m, ov)) => (m, Some(ov)),
                        None => (decl, None),
                    };
                    // `#[inline]` methods splice their body; never an instance.
                    if self.map.def_data(def)?.inline {
                        return None;
                    }
                    Some(UserCall {
                        def,
                        expr,
                        subst_override,
                        receiver: Some(*receiver),
                        args: args.clone(),
                        named: Vec::new(),
                    })
                }
            }
            // A receiver-less associated call (`uint(8)::unpack(b)`) that is not
            // `#[inline]` becomes an instance, like a method call.
            ExprKind::TypePathCall { args, .. } => {
                let decl = self.inf.method_resolution(expr)?;
                let (def, subst_override) = match self.resolve_trait_instance(expr, decl) {
                    Some((m, ov)) => (m, Some(ov)),
                    None => (decl, None),
                };
                if self.map.def_data(def)?.inline {
                    return None;
                }
                Some(UserCall {
                    def,
                    expr,
                    subst_override,
                    receiver: None,
                    args: args.clone(),
                    named: Vec::new(),
                })
            }
            _ => None,
        }
    }

    /// Emit a submodule instance for a user call, connecting `result_target` to
    /// the callee's (flattened) return. Connection order is the callee's module
    /// order: `dom` clocks, then params (named-then-positional, each flattened),
    /// then the return. Positional params zip with `[receiver?] ++ args`; named
    /// params match the call's named section by name.
    fn emit_instance(&mut self, uc: UserCall<'db>, result_target: Vec<(String, SvExpr)>) {
        let csig = sig_of(self.db, self.krate, uc.def);
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
        let node_subst: Vec<Option<Term<'db>>> = match &uc.subst_override {
            Some(ov) => ov.clone(),
            None => self
                .inf
                .call_subst(uc.expr)
                .map(|ts| ts.iter().cloned().map(Some).collect())
                .unwrap_or_default(),
        };
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
                    ConstArg::Local(_) | ConstArg::Field(..) | ConstArg::Op(..) => {
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
                    ConstArg::Op(..) => SvExpr::Lit(render_const_sv(&c, self.sig)),
                    _ => return None, // unresolved — leave to the default
                };
                Some((g.name.clone(), rendered))
            })
            .collect();

        // Resolve each value param to its caller expression (positional zip with
        // `[receiver?] ++ args`; named by name) — `(pname, pty, caller_expr)`.
        let mut positional: Vec<ExprId> = uc.receiver.into_iter().collect();
        positional.extend(uc.args.iter().map(|a| a.expr));
        let mut pos_i = 0;
        let slots: Vec<(String, Type<'db>, Option<ExprId>, Option<String>)> = csig
            .params
            .iter()
            .map(|p| {
                let ty = p.ty.clone();
                let caller_expr = if p.from_named_section {
                    uc.named.iter().find(|n| n.name == p.name).map(|n| n.expr)
                } else {
                    let e = positional.get(pos_i).copied();
                    pos_i += 1;
                    e
                };
                (p.name.clone(), ty, caller_expr, p.default.clone())
            })
            .collect();

        // A type-generic callee is monomorphised: bind its Type params from the
        // actual arg types, name the copy `Callee__Arg`, and request its emission.
        let subst: Vec<Option<Term<'db>>> = if let Some(ov) = &uc.subst_override {
            ov.clone()
        } else if is_type_generic(csig) {
            let mut subst = vec![None; csig.generic_params.len()];
            for (_, pty, caller_expr, _) in &slots {
                if let Some(e) = caller_expr
                    && let Some(at) = self.actual_type(*e)
                {
                    match_type(pty, &at, &mut subst);
                }
            }
            subst
        } else {
            Vec::new()
        };
        // Only TYPE-kind bindings force a specialised copy — Const-kind
        // bindings ride the `#(...)` parameters of the one parametric module.
        let needs_mono =
            csig.generic_params.iter().enumerate().any(|(i, g)| {
                g.kind == TermKind::Type && subst.get(i).is_some_and(Option::is_some)
            });
        let module = if needs_mono {
            let name = mono_name(self.map, uc.def, csig, &subst);
            self.mono_reqs.push(MonoReq {
                callee: uc.def,
                subst: subst.clone(),
                name: name.clone(),
            });
            name
        } else {
            module_name(self.map, uc.def)
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
        for (pname, pty, caller_expr, default) in &slots {
            let pty = subst_type(pty, &flatten_subst);
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
            let caller_leaves: Vec<(String, SvExpr)> = match caller_expr {
                Some(e) => self.expr_leaves(*e),
                None => match default {
                    Some(d) => callee_leaves
                        .iter()
                        .map(|_| (String::new(), default_value(d)))
                        .collect(),
                    None => Vec::new(),
                },
            };
            for (cl, (_, cv)) in callee_leaves.into_iter().zip(caller_leaves) {
                connections.push((join(pname, &cl.suffix), cv));
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

    /// The concrete type of a call argument (for binding a type-generic callee):
    /// a local's resolved type, else the expression's inferred type.
    fn actual_type(&self, e: ExprId) -> Option<Type<'db>> {
        match self.body.expr(e).kind {
            ExprKind::Local(l) => self.local_ty(l),
            _ => self.inf.expr_type(e).cloned(),
        }
    }

    /// A user call in value position: instantiate into a fresh `__call_N`
    /// (declared per field) and return its leaves. A void callee yields a single
    /// placeholder leaf.
    fn call_value_leaves(&mut self, uc: UserCall<'db>) -> Vec<(String, SvExpr)> {
        let Some(rt) = sig_of(self.db, self.krate, uc.def).return_type.clone() else {
            self.emit_instance(uc, Vec::new());
            return vec![(String::new(), SvExpr::Lit("0".to_owned()))];
        };
        // The return type is written in the CALLEE's generic-param space —
        // substitute the call's recorded instantiation before flattening
        // against the caller's generics (a parametric callee's `uint(n)`
        // otherwise renders against the wrong index space).
        let rt = match &uc.subst_override {
            Some(ov) => ground_widths(self.db, self.krate, self.def, &subst_type(&rt, ov)),
            None => match self.inf.call_subst(uc.expr) {
                Some(ts) => {
                    let opts: Vec<Option<Term<'db>>> = ts.iter().cloned().map(Some).collect();
                    ground_widths(self.db, self.krate, self.def, &subst_type(&rt, &opts))
                }
                None => rt,
            },
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
        self.emit_instance(uc, target.clone());
        target
    }

    /// Declare each out-arg target that is a fresh implicit `var` (not a port),
    /// so a `=> ds` binding gets its `logic` before the instance that drives it.
    fn declare_out_targets(&mut self, uc: &UserCall<'db>) {
        let targets: Vec<ExprId> = uc
            .named
            .iter()
            .filter(|n| n.out)
            .map(|n| n.expr)
            .chain(uc.args.iter().filter(|a| a.out).map(|a| a.expr))
            .collect();
        for e in targets {
            if let ExprKind::Local(l) = self.body.expr(e).kind
                && self.body.local(l).kind != LocalKind::Param
            {
                self.declare_local(l);
            }
        }
    }

    fn fresh_call(&mut self) -> String {
        let n = self.synth;
        self.synth += 1;
        format!("__call_{n}")
    }

    /// A per-callee instance name: the first instance is the bare module name,
    /// later ones get `_1`, `_2`, ….
    fn instance_name(&mut self, module: &str) -> String {
        let n = self.instance_counts.entry(module.to_owned()).or_insert(0);
        let name = if *n == 0 {
            module.to_owned()
        } else {
            format!("{module}_{n}")
        };
        *n += 1;
        name
    }
}

/// A user `fn`/method call decomposed for instantiation.
struct UserCall<'db> {
    def: DefId<'db>,
    /// The call expression itself — keys `Inference::call_subst`.
    expr: ExprId,
    /// Present when `def` was re-selected from a trait-method DECL to an
    /// impl method (Instance::resolve): the impl method's substitution
    /// (header binding ++ the decl's own generic tail), already in the
    /// CALLER's value space. Overrides both the mono subst and the
    /// `#(...)` parameter source.
    subst_override: Option<Vec<Option<Term<'db>>>>,
    receiver: Option<ExprId>,
    args: Vec<ConnArg>,
    named: Vec<NamedArg>,
}

/// The decomposed parts of a `receiver.reg(reset, init)` method call.
struct RegCall {
    d_input: ExprId,
    reset: ExprId,
    init: ExprId,
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
            let dim = match len {
                ConstArg::Lit(n) => SvExpr::Lit(n.to_string()),
                ConstArg::Param(i) => match generics.get(*i as usize) {
                    Some(g) => SvExpr::Ident(g.name.clone()),
                    None => SvExpr::Lit("1".to_owned()),
                },
                _ => SvExpr::Lit("1".to_owned()),
            };
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
                ConstArg::Local(_)
                | ConstArg::Op(..)
                | ConstArg::Field(..)
                | ConstArg::Assoc { .. } => {
                    match crate::hir::const_eval::eval_const(self.db, self.krate, self.def, c) {
                        Some(v) => ConstArg::Lit(v),
                        None => c.clone(),
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

/// The base local of a place expression (`acc`, `acc.f`, `acc[i]` all root at
/// the same local).
fn backend_root_local(body: &Body, expr: ExprId) -> Option<LocalId> {
    match &body.expr(expr).kind {
        ExprKind::Local(l) => Some(*l),
        ExprKind::Field { receiver, .. } => backend_root_local(body, *receiver),
        ExprKind::Index { base, .. } => backend_root_local(body, *base),
        _ => None,
    }
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
    match c {
        ConstArg::Lit(v) => v.to_string(),
        ConstArg::Param(i) => sig
            .generic_params
            .get(*i as usize)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "/*unknown*/".to_owned()),
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
                render_const_sv(a, sig),
                op,
                render_const_sv(b, sig)
            )
        }
        _ => "/*unknown*/".to_owned(),
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
        // A non-literal, non-`Param` width (associated const, const expression).
        // `flatten_leaves` grounds these to a literal via const_eval before
        // rendering (that grounding fixed the `uint(T::width)` → 1-bit bug), so on
        // a clean crate one surviving here is an internal invariant violation.
        // The exception worth building: a symbolic COMPOUND width in a parametric
        // module (`uint(n + 1)`) — TODO: render those via `render_const_sv`
        // (`[(n+1)-1:0]`) rather than this hard error.
        other => panic!(
            "width/length `{other:?}` is neither a literal nor a generic param on \
             a clean crate. A concrete width must be grounded to a literal by \
             const_eval before emission; symbolic compound widths are not \
             rendered yet (TODO: render via render_const_sv). Not a 1-bit default.",
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
