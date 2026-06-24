//! `mir_of(def)` — HIR→MIR lowering (`planning/mir.md`).
//!
//! A derived, per-def salsa query: reads `body` + `infer` and bakes the resolved
//! types onto fresh MIR nodes. Because MIR is *derived* (rebuilt, never an
//! input), embedding types costs nothing incrementally — exactly as rustc keeps
//! `TypeckResults` separate from MIR.
//!
//! Lowering is **structural and total over well-typed bodies**, with negative
//! space made explicit: any shape we assert cannot occur (an unresolved method
//! call, a non-`Def` callee) `panic!`s rather than silently degrading. Nothing
//! consumes MIR yet, so a panic here surfaces only via the smoke test.

use crate::base::db::SourceRoot;
use crate::hir::body::{Block, ConnArg, ExprId, ExprKind, NamedArg, Stmt, body};
use crate::hir::infer::{Inference, infer};
use crate::hir::types::{Term, Type};
use crate::mir::ir::*;
use crate::nameres::ids::DefId;

/// QUERY: lower a fn/method body to MIR. Non-fn defs yield an empty MIR (their
/// `body` is empty), mirroring `body`/`infer`.
#[salsa::tracked(returns(ref))]
pub fn mir_of<'db>(db: &'db dyn salsa::Database, krate: SourceRoot, def: DefId<'db>) -> Mir<'db> {
    let body = body(db, krate, def);
    let inf = infer(db, krate, def);

    let mut lower = Lower {
        body,
        inf,
        exprs: Vec::with_capacity(body.exprs().count()),
    };

    // Typed locals: take HIR locals, replace the declared/inferred split with the
    // single resolved type from inference. A local with no recorded type (should
    // not happen for a well-typed body) falls back to `Error`.
    let locals = body
        .locals()
        .iter()
        .enumerate()
        .map(|(i, l)| MLocal {
            name: l.name.clone(),
            kind: l.kind,
            ty: inf
                .local_type(crate::hir::types::LocalId(i as u32))
                .cloned()
                .or_else(|| l.declared_ty.clone())
                .unwrap_or(Type::Error),
            result_base: l.result_base.clone(),
            mutable: l.mutable,
        })
        .collect();

    let block = lower.lower_block(body.block());

    Mir {
        exprs: lower.exprs,
        locals,
        param_count: body.param_count(),
        block,
        verilog: body.verilog().cloned(),
    }
}

/// Map a builtin method name to its closed-set tag. Panics on an unrecognised
/// name — a method call with no `method_resolution` must be one of the builtins
/// inference handles structurally (negative space: the assumption is asserted).
fn builtin_method(name: &str) -> BuiltinMethod {
    match name {
        "reg" => BuiltinMethod::Reg,
        "posedge" => BuiltinMethod::Posedge,
        "replace" => BuiltinMethod::Replace,
        "enumerate" => BuiltinMethod::Enumerate,
        other => panic!(
            "MIR lowering: method `.{other}` has no resolution and is not a \
             known builtin (reg/posedge/replace/enumerate)"
        ),
    }
}

struct Lower<'a, 'db> {
    body: &'a crate::hir::body::Body<'db>,
    inf: &'a Inference<'db>,
    exprs: Vec<MExpr<'db>>,
}

impl<'a, 'db> Lower<'a, 'db> {
    /// The baked type of a HIR expression. Missing only for exprs inference does
    /// not type (e.g. the callee sub-expression of a call) — `Error` there is
    /// fine because those nodes are consumed structurally, not by type.
    fn ty_of(&self, id: ExprId) -> Type<'db> {
        self.inf.expr_type(id).cloned().unwrap_or(Type::Error)
    }

    fn push(&mut self, kind: MExprKind<'db>, id: ExprId) -> MExprId {
        let mexpr = MExpr {
            kind,
            ty: self.ty_of(id),
            span: self.body.expr_span(id),
        };
        let mid = MExprId(self.exprs.len() as u32);
        self.exprs.push(mexpr);
        mid
    }

    fn lower_block(&mut self, block: &Block) -> MBlock {
        let stmts = block.stmts.iter().map(|s| self.lower_stmt(s)).collect();
        let tail = block.tail.map(|e| self.lower_expr(e));
        MBlock { stmts, tail }
    }

    fn lower_stmt(&mut self, stmt: &Stmt) -> MStmt {
        match stmt {
            Stmt::Let { local, value } => MStmt::Let {
                local: *local,
                value: self.lower_expr(*value),
            },
            Stmt::VarDecl { local } => MStmt::VarDecl { local: *local },
            Stmt::Equation { lhs, rhs } => MStmt::Equation {
                lhs: self.lower_expr(*lhs),
                rhs: self.lower_expr(*rhs),
            },
            Stmt::Return { value } => MStmt::Return {
                value: self.lower_expr(*value),
            },
            Stmt::Expr(e) => MStmt::Expr(self.lower_expr(*e)),
            Stmt::When { event, body, init } => MStmt::When {
                event: self.lower_expr(*event),
                body: self.lower_block(body),
                init: init.as_ref().map(|b| self.lower_block(b)),
            },
            Stmt::For {
                index,
                elem,
                iter,
                body,
            } => MStmt::For {
                index: *index,
                elem: *elem,
                iter: self.lower_expr(*iter),
                body: self.lower_block(body),
            },
        }
    }

    fn lower_expr(&mut self, id: ExprId) -> MExprId {
        let kind = self.lower_kind(id, &self.body.expr(id).kind.clone());
        self.push(kind, id)
    }

    /// Lower one HIR `ExprKind` to a MIR `MExprKind`, resolving calls and folding
    /// typed literals. `id` is the HIR expr id (for subst/method lookups, which
    /// are keyed on the call expression itself).
    fn lower_kind(&mut self, id: ExprId, kind: &ExprKind<'db>) -> MExprKind<'db> {
        match kind {
            ExprKind::Missing => MExprKind::Missing,
            ExprKind::Number(v, base) => MExprKind::Number(*v, *base),
            // A typed literal is just a number; its type rides on the MExpr.
            ExprKind::TypedLiteral { value, base, .. } => MExprKind::Number(*value, *base),
            ExprKind::Bool(b) => MExprKind::Bool(*b),
            ExprKind::Local(l) => MExprKind::Local(*l),
            ExprKind::ConstParam(i) => MExprKind::ConstParam(*i),
            ExprKind::ConstAssoc { item, self_ty } => MExprKind::ConstAssoc {
                item: *item,
                self_ty: self_ty.clone(),
            },
            ExprKind::Def(d) => MExprKind::Def(*d),
            ExprKind::VecLit(es) => {
                MExprKind::VecLit(es.iter().map(|e| self.lower_expr(*e)).collect())
            }
            ExprKind::TupleLit(es) => {
                MExprKind::TupleLit(es.iter().map(|e| self.lower_expr(*e)).collect())
            }
            ExprKind::VecRepeat { elem, len } => MExprKind::VecRepeat {
                elem: self.lower_expr(*elem),
                len: len.clone(),
            },
            ExprKind::Index { base, index } => MExprKind::Index {
                base: self.lower_expr(*base),
                index: self.lower_expr(*index),
            },
            ExprKind::Field { receiver, field } => MExprKind::Field {
                receiver: self.lower_expr(*receiver),
                field: field.clone(),
            },
            // The four call shapes collapse to one resolved Call.
            ExprKind::Call {
                callee,
                args,
                named,
            } => {
                let callee_def = match &self.body.expr(*callee).kind {
                    ExprKind::Def(d) => *d,
                    other => panic!(
                        "MIR lowering: plain-call callee is not a Def ({:?}-shaped); \
                         dispatch resolution gap",
                        std::mem::discriminant(other)
                    ),
                };
                MExprKind::Call {
                    callee: callee_def,
                    substs: self.substs_of(id),
                    receiver: None,
                    args: self.lower_args(args),
                    named: self.lower_named(named),
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                // A resolved dispatch records a callee def; a builtin does not.
                // The presence of a `method_resolution` is exactly that split.
                match self.inf.method_resolution(id) {
                    Some(callee) => MExprKind::Call {
                        callee,
                        substs: self.substs_of(id),
                        receiver: Some(self.lower_expr(*receiver)),
                        args: self.lower_args(args),
                        named: Vec::new(),
                    },
                    None => MExprKind::Builtin {
                        method: builtin_method(method),
                        receiver: self.lower_expr(*receiver),
                        args: self.lower_args(args),
                    },
                }
            }
            ExprKind::TypePathCall {
                self_ty: _,
                method,
                args,
            } => {
                let callee = self.inf.method_resolution(id).unwrap_or_else(|| {
                    panic!("MIR lowering: unresolved type-path call `::{method}`")
                });
                MExprKind::Call {
                    callee,
                    substs: self.substs_of(id),
                    receiver: None,
                    args: self.lower_args(args),
                    named: Vec::new(),
                }
            }
            ExprKind::Record { ctor, fields } => MExprKind::Record {
                ctor: *ctor,
                fields: fields
                    .iter()
                    .map(|f| MRecordField {
                        name: f.name.clone(),
                        out: f.out,
                        value: self.lower_expr(f.value),
                    })
                    .collect(),
            },
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => MExprKind::If {
                cond: self.lower_expr(*cond),
                then_branch: self.lower_block(then_branch),
                else_branch: self.lower_block(else_branch),
            },
            ExprKind::ConstIf {
                cond,
                then_branch,
                else_branch,
            } => MExprKind::ConstIf {
                cond: self.lower_expr(*cond),
                then_branch: self.lower_block(then_branch),
                else_branch: self.lower_block(else_branch),
            },
            ExprKind::Slice {
                base,
                lo,
                hi,
                width,
            } => MExprKind::Slice {
                base: self.lower_expr(*base),
                lo: lo.map(|e| self.lower_expr(e)),
                hi: hi.map(|e| self.lower_expr(e)),
                width: width.map(|e| self.lower_expr(e)),
            },
            ExprKind::When { event, body, init } => MExprKind::When {
                event: self.lower_expr(*event),
                body: self.lower_block(body),
                init: init.map(|e| self.lower_expr(e)),
            },
            ExprKind::Block(b) => MExprKind::Block(self.lower_block(b)),
        }
    }

    /// The deep-resolved generic substitution for a call site (callee-param
    /// order). Empty when the callee is non-generic or none was recorded.
    fn substs_of(&self, id: ExprId) -> Vec<Term<'db>> {
        self.inf
            .call_subst(id)
            .map(|ts| ts.to_vec())
            .unwrap_or_default()
    }

    fn lower_args(&mut self, args: &[ConnArg]) -> Vec<MArg> {
        args.iter()
            .map(|a| MArg {
                out: a.out,
                expr: self.lower_expr(a.expr),
            })
            .collect()
    }

    fn lower_named(&mut self, named: &[NamedArg]) -> Vec<MNamedArg> {
        named
            .iter()
            .map(|n| MNamedArg {
                name: n.name.clone(),
                out: n.out,
                expr: self.lower_expr(n.expr),
            })
            .collect()
    }
}
