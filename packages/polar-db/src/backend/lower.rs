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
//! Aggregates (flatten) and instances arrive in Q5d.

use std::collections::HashMap;

use crate::backend::ir::{
    SvAlwaysComb, SvAlwaysFf, SvBinOp, SvCombIf, SvCombStmt, SvExpr, SvFile, SvItem, SvLogicDecl,
    SvModule, SvPort, SvPortDirection, SvSeqAssign, SvType,
};
use crate::base::db::SourceRoot;
use crate::hir::body::{Block, Body, ExprId, ExprKind, RecordField, Stmt, body};
use crate::hir::infer::{Inference, infer};
use crate::hir::sig::{Signature, sig_of};
use crate::hir::types::{ConstArg, Direction, Domain, GenericParamKind, LocalId, Type, ValueKind};
use crate::nameres::def_map::{CrateDefMap, crate_def_map};
use crate::nameres::ids::{DefId, DefKind};

/// QUERY: lower one fn/method to a SystemVerilog module (combinational scalar
/// subset). Non-fn defs yield an empty module.
#[salsa::tracked(returns(ref))]
pub fn sv_module<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> SvModule {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return SvModule::default();
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return SvModule::default();
    }
    let sig = sig_of(db, krate, def);
    let body = body(db, krate, def);

    // Ports: `dom` generics → clock inputs; value params and the return type are
    // flattened per-field (`inp: Packet @clk` → `inp__valid` / `inp__payload`),
    // each field's module direction folding the param/return direction with the
    // port-field direction.
    let mut ports = Vec::new();
    for g in &sig.generic_params {
        if g.kind == GenericParamKind::Domain {
            ports.push(SvPort {
                direction: SvPortDirection::Input,
                ty: SvType::bit(),
                name: g.name.clone(),
            });
        }
    }
    for p in &sig.params {
        let drives = p.direction == Some(Direction::Out);
        for leaf in flatten_leaves(db, krate, &p.ty, drives) {
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
    if let Some(rt) = &sig.return_type {
        for leaf in flatten_leaves(db, krate, rt, true) {
            ports.push(SvPort {
                direction: SvPortDirection::Output,
                ty: leaf.ty,
                name: join("result", &leaf.suffix),
            });
        }
    }

    let inf = infer(db, krate, def);
    let mut lower = SvLower {
        db,
        krate,
        map,
        body,
        inf,
        sig,
        local_names: unique_local_names(body),
        items: Vec::new(),
        synth: 0,
    };
    lower.lower_top_block(body.block());

    SvModule {
        name: data.name.clone(),
        parameters: Vec::new(),
        ports,
        items: lower.items,
    }
}

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

/// QUERY: the crate's SystemVerilog — every top-level `fn` as a module. (Driver:
/// "force `verilog` for each top-level item.") Modules are emitted name-sorted
/// for determinism; an explicit top-entity ordering can refine this later.
#[salsa::tracked(returns(ref))]
pub fn verilog(db: &dyn salsa::Database, krate: SourceRoot) -> String {
    let map = crate_def_map(db, krate);
    // Modules are erased before codegen, so every `fn` in the crate (at the root
    // or nested in a `mod`) becomes a top-level SV module. Name-sorted for a
    // deterministic order (source-order parity with the oracle is a Q5e detail).
    let prelude = map.prelude();
    let mut fns: Vec<(String, DefId)> = map
        .defs()
        .filter_map(|d| map.def_data(d).map(|data| (d, data)))
        .filter(|(_, data)| data.kind == DefKind::Fn && data.module != prelude)
        .map(|(d, data)| (data.name.clone(), d))
        .collect();
    fns.sort_by(|a, b| a.0.cmp(&b.0));
    let modules = fns
        .iter()
        .map(|(_, def)| sv_module(db, krate, *def).clone())
        .collect();
    SvFile { modules }.to_string()
}

struct SvLower<'a, 'db> {
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    map: &'a CrateDefMap<'db>,
    body: &'a Body<'db>,
    inf: &'a Inference<'db>,
    sig: &'a Signature<'db>,
    /// Uniquified SV name per [`LocalId`].
    local_names: Vec<String>,
    items: Vec<SvItem>,
    /// Counter for synthetic `__block_N` result locals (`when`/`if`).
    synth: u32,
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
        for stmt in stmts {
            match stmt {
                Stmt::Let { local, value } => self.lower_let(*local, *value),
                Stmt::VarDecl { local } => self.declare_local(*local),
                Stmt::Equation { lhs, rhs } => self.lower_equation(*lhs, *rhs),
                Stmt::Return { value } => self.drive_result(*value),
                // Bare expression statements (instance calls) land in Q5d-2.
                Stmt::Expr(_) => {}
            }
        }
    }

    /// `let x = value;`. A builtin `.reg` makes the let the register itself
    /// (per field); otherwise the local's leaves are declared and each driven by
    /// the corresponding leaf of the value.
    fn lower_let(&mut self, local: LocalId, value: ExprId) {
        if let Some(reg) = self.as_reg(value) {
            let leaves = self.local_type_leaves(local);
            let base = self.local_name(local);
            let clock = self.clock_of_type(self.inf.local_type(local));
            self.emit_registers(&base, &leaves, reg, clock, true);
            return;
        }
        self.declare_local(local);
        let target = self.local_leaves(local);
        let value_leaves = self.expr_leaves(value);
        for ((_, place), (_, v)) in target.into_iter().zip(value_leaves) {
            self.push_assign(place, v);
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

    /// Drive `result` (the return port) per field from `value`'s leaves.
    fn drive_result(&mut self, value: ExprId) {
        let Some(rt) = self.sig.return_type.clone() else {
            return;
        };
        let result_leaves = flatten_leaves(self.db, self.krate, &rt, true);
        let value_leaves = self.expr_leaves(value);
        for (rl, (_, v)) in result_leaves.into_iter().zip(value_leaves) {
            self.push_assign(SvExpr::Ident(join("result", &rl.suffix)), v);
        }
    }

    /// Declare a `logic` for each of a local's leaves.
    fn declare_local(&mut self, local: LocalId) {
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

    /// A local's SV name (uniquified).
    fn local_name(&self, local: LocalId) -> String {
        self.local_names[local.0 as usize].clone()
    }

    /// A local's type: inferred, falling back to declared.
    fn local_ty(&self, local: LocalId) -> Option<Type<'db>> {
        self.inf
            .local_type(local)
            .cloned()
            .or_else(|| self.body.local(local).declared_ty.clone())
    }

    /// The scalar leaves of a local's type (scalar → one bit-typed leaf).
    fn local_type_leaves(&self, local: LocalId) -> Vec<Leaf> {
        match self.local_ty(local) {
            Some(t) => flatten_leaves(self.db, self.krate, &t, true),
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
            Some(t) => flatten_leaves(self.db, self.krate, &t, drives)
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
            .map(sv_type)
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
                reset_body: vec![SvSeqAssign {
                    lhs: SvExpr::Ident(name.clone()),
                    rhs: init_v,
                }],
                clocked_body: vec![SvSeqAssign {
                    lhs: SvExpr::Ident(name),
                    rhs: d_in,
                }],
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
        match &self.body.expr(expr).kind {
            ExprKind::Number(n) => SvExpr::Lit(n.to_string()),
            ExprKind::Bool(b) => SvExpr::Lit(if *b { "1'b1" } else { "1'b0" }.to_owned()),
            ExprKind::Local(l) => SvExpr::Ident(self.local_name(*l)),
            ExprKind::Call { callee, args, .. } => {
                let callee = *callee;
                if let Some(op) = self.prelude_op(callee)
                    && args.len() == 2
                {
                    let (a, b) = (args[0].expr, args[1].expr);
                    let l = self.expr_value(a);
                    let r = self.expr_value(b);
                    return SvExpr::BinOp(op, Box::new(l), Box::new(r));
                }
                // User-fn calls become module instances (Q5d).
                SvExpr::Lit("0".to_owned())
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
            ExprKind::When { event, body } => {
                let event = *event;
                let body = body.clone();
                self.lower_when(expr, event, &body)
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
            // Field access / records in scalar position: take the single leaf.
            ExprKind::Field { .. } | ExprKind::Record { .. } => self
                .expr_leaves(expr)
                .into_iter()
                .next()
                .map(|(_, e)| e)
                .unwrap_or_else(|| SvExpr::Lit("0".to_owned())),
            // User method calls → Q5d-2.
            _ => SvExpr::Lit("0".to_owned()),
        }
    }

    /// An expression's scalar leaves as `(suffix, value)`, in struct-field order.
    /// Aggregates expand (a struct local → one leaf per field, a field access
    /// projects, a record literal rebuilds); scalars are a single empty-suffix
    /// leaf via [`Self::expr_value`].
    fn expr_leaves(&mut self, expr: ExprId) -> Vec<(String, SvExpr)> {
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
                for (suf, e) in self.expr_leaves(rf.value) {
                    out.push((join(fname, &suf), e));
                }
            }
        }
        out
    }

    /// `when ev { … d }` → a reset-less `always_ff @(posedge <ev-clock>)` whose
    /// single clocked assignment drives a synthetic `__block_N` with the body's
    /// tail value `d`. The expression's value is that held register output.
    fn lower_when(&mut self, when_expr: ExprId, event: ExprId, body: &Block) -> SvExpr {
        let synth = self.fresh_block();
        let ty = self.expr_type(when_expr);
        self.items.push(SvItem::Logic(SvLogicDecl {
            ty,
            name: synth.clone(),
        }));
        let clock = self.clock_of_event(event);
        let d = self.block_value(body);
        self.items.push(SvItem::AlwaysFf(SvAlwaysFf {
            clock,
            reset: None,
            reset_body: Vec::new(),
            clocked_body: vec![SvSeqAssign {
                lhs: SvExpr::Ident(synth.clone()),
                rhs: d,
            }],
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
            .find(|g| g.kind == GenericParamKind::Domain)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "clk".to_owned())
    }

    /// `Some(op)` if the callee expr is the prelude `+` / `*`.
    fn prelude_op(&self, callee: ExprId) -> Option<SvBinOp> {
        let ExprKind::Def(def) = self.body.expr(callee).kind else {
            return None;
        };
        let data = self.map.def_data(def)?;
        if data.module != self.map.prelude() {
            return None;
        }
        match data.name.as_str() {
            "+" => Some(SvBinOp::Add),
            "*" => Some(SvBinOp::Mul),
            _ => None,
        }
    }
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
fn flatten_leaves(
    db: &dyn salsa::Database,
    krate: SourceRoot,
    ty: &Type<'_>,
    drives: bool,
) -> Vec<Leaf> {
    match ty {
        Type::Value {
            kind: ValueKind::Struct { def, .. },
            ..
        } => {
            let sig = sig_of(db, krate, *def);
            let mut out = Vec::new();
            for f in &sig.fields {
                for sub in flatten_leaves(db, krate, &f.ty, drives) {
                    out.push(Leaf {
                        suffix: join(&f.name, &sub.suffix),
                        ty: sub.ty,
                        drives: sub.drives,
                    });
                }
            }
            out
        }
        Type::Port { def, .. } => {
            let sig = sig_of(db, krate, *def);
            let mut out = Vec::new();
            for f in &sig.fields {
                // The module drives a port field iff its own drive matches the
                // field's producer direction (`out` field of an `out` port, or
                // `in` field of an `in` port).
                let child = drives == (f.direction == Some(Direction::Out));
                for sub in flatten_leaves(db, krate, &f.ty, child) {
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
            ty: sv_type(ty),
            drives,
        }],
    }
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

/// Lower a value type to an SV type. Concrete `uint(W)` → `[W-1:0]`; `bool` /
/// `Reset` / `Clock` → 1-bit. Parametric widths (`uint(N)`) become SV
/// `parameter`s in a later slice; here a non-literal width falls back to 1-bit.
fn sv_type(ty: &Type) -> SvType {
    match ty {
        Type::Value {
            kind: ValueKind::UInt {
                width: ConstArg::Lit(w),
            },
            ..
        } => SvType::uint(SvExpr::Lit(w.to_string())),
        _ => SvType::bit(),
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
        vfs.set_file_text(&mut db, "t.plr", src.to_owned());
        let krate = vfs.source_root(&mut db, "t.plr");
        verilog(&db, krate).clone()
    }

    #[test]
    fn scalar_combinational_fn_emits_matching_verilog() {
        // The `add_constant.plr` shape — parity with polar-compiler's output.
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
    fn when_lowers_to_a_resetless_always_ff() {
        let sv = emit(
            "fn counter { dom clk: Clock } () -> uint(8) @clk {\n  var count: uint(8) @clk;\n  count = when clk.posedge() { count + 1 };\n  count\n}",
        );
        // A synthetic register, clocked, reset-less, fed by the body tail.
        assert!(sv.contains("    logic [7:0] count;"), "{sv}");
        assert!(sv.contains("    always_ff @(posedge clk) begin"), "{sv}");
        assert!(sv.contains("__block_0 <= (count + 1);"), "{sv}");
        assert!(sv.contains("assign count = __block_0;"), "{sv}");
        assert!(sv.contains("assign result = count;"), "{sv}");
        // No reset branch on a `when`.
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
        // The `packet_struct.plr` shape: a struct param/return erase to
        // per-field ports; `inp.reg(rstn, packet{…})` is a per-field register
        // (note the `false` init renders `1'b0`); `return held` drives each
        // result field. Byte-parity with polar-compiler.
        let sv = emit(
            "struct Packet = packet { valid: bool, payload: uint(8) }\n\
             fn registerPacket { dom clk: Clock, rstn: Reset @clk = high } ( inp: Packet @clk ) -> Packet @clk {\n\
               let held = inp.reg(rstn, packet { valid: false, payload: 0 });\n\
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
        // The `simple_port.plr` shape: a port param flattens per field with the
        // module direction folding param + field direction; `downstream =
        // upstream` becomes one connection per field, the `in` field flowing the
        // other way. Byte-parity with polar-compiler.
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
               return option { valid: a.valid, payload: payloadd };\n\
             }",
        );
        assert!(sv.contains("    logic [7:0] payloadd;"), "{sv}");
        assert!(sv.contains("payloadd <= a__payload;"), "{sv}");
        assert!(sv.contains("assign result__valid = a__valid;"), "{sv}");
        assert!(sv.contains("assign result__payload = payloadd;"), "{sv}");
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
