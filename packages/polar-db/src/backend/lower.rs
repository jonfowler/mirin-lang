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
use crate::hir::body::{Block, Body, ExprId, ExprKind, Stmt, body};
use crate::hir::infer::{Inference, infer};
use crate::hir::sig::{Signature, sig_of};
use crate::hir::types::{ConstArg, Direction, Domain, GenericParamKind, LocalId, Type, ValueKind};
use crate::nameres::def_map::{CrateDefMap, crate_def_map};
use crate::nameres::ids::{DefId, DefKind, Namespace};

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

    // Ports: `dom` generics → clock inputs; value params → in/out by direction;
    // the return type → an `output` named `result`.
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
        ports.push(SvPort {
            direction: if p.direction == Some(Direction::Out) {
                SvPortDirection::Output
            } else {
                SvPortDirection::Input
            },
            ty: sv_type(&p.ty),
            name: p.name.clone(),
        });
    }
    if let Some(rt) = &sig.return_type {
        ports.push(SvPort {
            direction: SvPortDirection::Output,
            ty: sv_type(rt),
            name: "result".to_owned(),
        });
    }

    let inf = infer(db, krate, def);
    let mut lower = SvLower {
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
    let root = map.root();
    let mut fns: Vec<(String, DefId)> = map
        .module(root)
        .items()
        .filter(|((_, ns), b)| {
            *ns == Namespace::Item && map.def_data(b.def).map(|d| d.kind) == Some(DefKind::Fn)
        })
        .map(|((name, _), b)| (name.clone(), b.def))
        .collect();
    fns.sort_by(|a, b| a.0.cmp(&b.0));
    let modules = fns
        .iter()
        .map(|(_, def)| sv_module(db, krate, *def).clone())
        .collect();
    SvFile { modules }.to_string()
}

struct SvLower<'a, 'db> {
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
            let rhs = self.expr_value(tail);
            self.push_assign(SvExpr::Ident("result".to_owned()), rhs);
        }
    }

    fn lower_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::Let { local, value } => {
                    let name = self.local_name(*local);
                    self.items.push(SvItem::Logic(SvLogicDecl {
                        ty: self.local_type(*local),
                        name: name.clone(),
                    }));
                    // `let x = e.reg(rst, init)` — the let *is* the register.
                    if let Some(reg) = self.as_reg(*value) {
                        let clock = self.clock_of_type(self.inf.local_type(*local));
                        self.emit_reg(name, clock, reg);
                    } else {
                        let rhs = self.expr_value(*value);
                        self.push_assign(SvExpr::Ident(name), rhs);
                    }
                }
                Stmt::VarDecl { local } => self.items.push(SvItem::Logic(SvLogicDecl {
                    ty: self.local_type(*local),
                    name: self.local_name(*local),
                })),
                Stmt::Equation { lhs, rhs } => {
                    // `place = e.reg(rst, init)` — `place` is the register.
                    if let (ExprKind::Local(l), Some(reg)) =
                        (&self.body.expr(*lhs).kind, self.as_reg(*rhs))
                    {
                        let name = self.local_name(*l);
                        let clock = self.clock_of_type(self.inf.local_type(*l));
                        self.emit_reg(name, clock, reg);
                    } else {
                        let lhs = self.expr_value(*lhs);
                        let rhs = self.expr_value(*rhs);
                        self.push_assign(lhs, rhs);
                    }
                }
                Stmt::Return { value } => {
                    let rhs = self.expr_value(*value);
                    self.push_assign(SvExpr::Ident("result".to_owned()), rhs);
                }
                // Bare expression statements (instance calls) land in Q5d.
                Stmt::Expr(_) => {}
            }
        }
    }

    fn push_assign(&mut self, lhs: SvExpr, rhs: SvExpr) {
        self.items.push(SvItem::Assign { lhs, rhs });
    }

    /// A local's SV name (uniquified).
    fn local_name(&self, local: LocalId) -> String {
        self.local_names[local.0 as usize].clone()
    }

    /// A local's SV type: its inferred type, falling back to its declared type.
    fn local_type(&self, local: LocalId) -> SvType {
        self.inf
            .local_type(local)
            .or(self.body.local(local).declared_ty.as_ref())
            .map(sv_type)
            .unwrap_or_else(SvType::bit)
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

    /// Emit `always_ff @(posedge clock) … <target> <= …` for a `.reg`. The
    /// `logic` declaration for `target` is the caller's responsibility.
    fn emit_reg(&mut self, target: String, clock: String, reg: RegCall) {
        let d = self.expr_value(reg.d_input);
        let init = self.expr_value(reg.init);
        let reset = match self.expr_value(reg.reset) {
            SvExpr::Ident(s) => s,
            other => other.to_string(),
        };
        self.items.push(SvItem::AlwaysFf(SvAlwaysFf {
            clock,
            reset: Some(reset),
            reset_body: vec![SvSeqAssign {
                lhs: SvExpr::Ident(target.clone()),
                rhs: init,
            }],
            clocked_body: vec![SvSeqAssign {
                lhs: SvExpr::Ident(target),
                rhs: d,
            }],
        }));
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
            // Field access, records, user method calls → Q5d.
            _ => SvExpr::Lit("0".to_owned()),
        }
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
