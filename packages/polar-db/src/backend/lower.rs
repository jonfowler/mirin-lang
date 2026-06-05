//! HIR → SV IR lowering + the `verilog` driver (`planning/q5_backend.md`).
//!
//! **Q5b scope:** the combinational scalar case — a `fn` over scalar `uint`/`bool`
//! becomes an `SvModule` whose value params + return are ports and whose
//! `let`/`var`/equation/return bodies become `logic` decls + `assign`s. Registers
//! (`always_ff`), aggregates (flatten), and instances arrive in Q5c/Q5d.

use crate::backend::ir::{
    SvBinOp, SvExpr, SvFile, SvItem, SvLogicDecl, SvModule, SvPort, SvPortDirection, SvType,
};
use crate::base::db::SourceRoot;
use crate::hir::body::{Block, Body, ExprId, ExprKind, Stmt, body};
use crate::hir::infer::{Inference, infer};
use crate::hir::sig::sig_of;
use crate::hir::types::{ConstArg, Direction, GenericParamKind, LocalId, Type, ValueKind};
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
    let lower = SvLower { map, body, inf };
    let mut items = Vec::new();
    lower.block(body.block(), &mut items);

    SvModule {
        name: data.name.clone(),
        parameters: Vec::new(),
        ports,
        items,
    }
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
}

impl<'db> SvLower<'_, 'db> {
    fn block(&self, block: &Block, items: &mut Vec<SvItem>) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { local, value } => {
                    let name = self.body.local(*local).name.clone();
                    items.push(SvItem::Logic(SvLogicDecl {
                        ty: self.local_type(*local),
                        name: name.clone(),
                    }));
                    items.push(SvItem::Assign {
                        lhs: SvExpr::Ident(name),
                        rhs: self.expr(*value),
                    });
                }
                Stmt::VarDecl { local } => items.push(SvItem::Logic(SvLogicDecl {
                    ty: self.local_type(*local),
                    name: self.body.local(*local).name.clone(),
                })),
                Stmt::Equation { lhs, rhs } => items.push(SvItem::Assign {
                    lhs: self.expr(*lhs),
                    rhs: self.expr(*rhs),
                }),
                Stmt::Return { value } => items.push(SvItem::Assign {
                    lhs: SvExpr::Ident("result".to_owned()),
                    rhs: self.expr(*value),
                }),
                // Bare expression statements (instance calls) land in Q5d.
                Stmt::Expr(_) => {}
            }
        }
        if let Some(tail) = block.tail {
            items.push(SvItem::Assign {
                lhs: SvExpr::Ident("result".to_owned()),
                rhs: self.expr(tail),
            });
        }
    }

    /// A local's SV type: its inferred type, falling back to its declared type.
    fn local_type(&self, local: LocalId) -> SvType {
        self.inf
            .local_type(local)
            .or(self.body.local(local).declared_ty.as_ref())
            .map(sv_type)
            .unwrap_or_else(SvType::bit)
    }

    fn expr(&self, expr: ExprId) -> SvExpr {
        match &self.body.expr(expr).kind {
            ExprKind::Number(n) => SvExpr::Lit(n.to_string()),
            ExprKind::Bool(b) => SvExpr::Lit(if *b { "1'b1" } else { "1'b0" }.to_owned()),
            ExprKind::Local(l) => SvExpr::Ident(self.body.local(*l).name.clone()),
            ExprKind::Call { callee, args, .. } => {
                if let Some(op) = self.prelude_op(*callee)
                    && args.len() == 2
                {
                    return SvExpr::BinOp(
                        op,
                        Box::new(self.expr(args[0].expr)),
                        Box::new(self.expr(args[1].expr)),
                    );
                }
                // User-fn calls become module instances (Q5d).
                SvExpr::Lit("0".to_owned())
            }
            // Field access, method calls, records, if/when/block → Q5c/Q5d.
            _ => SvExpr::Lit("0".to_owned()),
        }
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
}
