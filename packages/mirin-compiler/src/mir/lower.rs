//! `mir_of(def)` — HIR→MIR lowering (`planning/mir.md`).
//!
//! A derived, per-def salsa query: reads `body` + `infer` and bakes the resolved
//! types onto fresh MIR nodes. Because MIR is *derived* (rebuilt, never an
//! input), embedding types costs nothing incrementally — exactly as rustc keeps
//! `TypeckResults` separate from MIR.
//!
//! Lowering is **structural and total**, with negative space made explicit. On a
//! *well-formed* body (no body/infer diagnostics) any shape we assert cannot
//! occur — an unresolved method call, a non-`Def` callee, a non-place equation
//! LHS — `panic!`s rather than silently degrading. On a malformed body those
//! same shapes can legitimately appear (parse recovery, type errors), so there
//! they degrade to `Missing`/degenerate places instead of crashing: a consumer
//! running ahead of the diagnostics gate (the LSP, a future pass) must not be
//! taken down by imperfect input. The `well_typed` flag gates the two regimes.

use crate::base::db::SourceRoot;
use crate::hir::body::{Block, ConnArg, ExprId, ExprKind, NamedArg, Stmt, body};
use crate::hir::infer::{Inference, infer};
use crate::hir::sig::sig_of;
use crate::hir::types::{ConstArg, LocalId, Term, Type, ValueKind};
use crate::mir::ir::*;
use crate::nameres::ids::DefId;

/// QUERY: lower a fn/method body to MIR. Non-fn defs yield an empty MIR (their
/// `body` is empty), mirroring `body`/`infer`.
#[salsa::tracked(returns(ref))]
pub fn mir_of<'db>(db: &'db dyn salsa::Database, krate: SourceRoot, def: DefId<'db>) -> Mir<'db> {
    let body = body(db, krate, def);
    let inf = infer(db, krate, def);

    let mut lower = Lower {
        db,
        krate,
        def,
        body,
        inf,
        // A malformed body can carry shapes the lowering/inference left
        // unresolved: a `Missing` node (parse recovery — a BODY diagnostic, not
        // an infer one), a non-`Def` callee, an unresolved method. On such a
        // body the negative-space panics degrade to `Missing`/degenerate places
        // — the panics assert the *well-formed-but-unhandled* invariant only, so
        // the gate must see both body and infer diagnostics.
        well_typed: body.diagnostics().is_empty() && inf.diagnostics().is_empty(),
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
/// The base length/width of a slice base (`bits(W)` → `W`, `Vec(N, _)` → `N`) —
/// the impl generic the desugared `Slice` call carries in its subst.
fn slice_base_width<'db>(ty: &Type<'db>) -> Option<ConstArg<'db>> {
    match ty {
        Type::Value {
            kind: ValueKind::Bits { width },
            ..
        } => Some(width.clone()),
        Type::Vec { len, .. } => Some(len.clone()),
        _ => None,
    }
}

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
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
    body: &'a crate::hir::body::Body<'db>,
    inf: &'a Inference<'db>,
    /// Whether the body type-checked (no infer diagnostics). Gates the
    /// negative-space panics: only a well-typed body asserts the invariants.
    well_typed: bool,
    exprs: Vec<MExpr<'db>>,
}

impl<'a, 'db> Lower<'a, 'db> {
    /// The baked type of a HIR expression. Every expr that is *lowered* (pushed
    /// to the arena) is one inference visited and typed; callee sub-expressions —
    /// the only untyped exprs in a well-typed body — are consumed structurally
    /// (their `DefId` extracted), never lowered. So on a well-typed body a
    /// missing type is a hole, not a normal case: assert it. On an ill-typed
    /// body `Error` is the honest fallback.
    fn ty_of(&self, id: ExprId) -> Type<'db> {
        debug_assert!(
            !self.well_typed || self.inf.expr_type(id).is_some(),
            "MIR lowering: lowered expr has no inferred type on a clean body"
        );
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
                lhs: self.lower_place(*lhs),
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
                    // A non-`Def` callee only arises from infer's Def-or-Error
                    // fallback on an ill-typed body. Degrade there; assert only
                    // on a well-typed body (a genuine dispatch-resolution gap).
                    other if self.well_typed => panic!(
                        "MIR lowering: plain-call callee is not a Def ({:?}-shaped); \
                         dispatch resolution gap",
                        std::mem::discriminant(other)
                    ),
                    _ => return MExprKind::Missing,
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
                // On an ill-typed body a `None` may instead be an unresolved
                // method (infer emitted `UnresolvedMethod`) — degrade to Missing
                // rather than mis-tag it as a builtin.
                match self.inf.method_resolution(id) {
                    Some(callee) => MExprKind::Call {
                        callee,
                        substs: self.substs_of(id),
                        receiver: Some(self.lower_expr(*receiver)),
                        args: self.lower_args(args),
                        named: Vec::new(),
                    },
                    None if self.well_typed => MExprKind::Builtin {
                        method: builtin_method(method),
                        receiver: self.lower_expr(*receiver),
                        args: self.lower_args(args),
                    },
                    None => MExprKind::Missing,
                }
            }
            ExprKind::TypePathCall {
                self_ty: _,
                method,
                args,
            } => {
                // A well-typed type-path call always has a resolution; a `None`
                // means infer rejected it (`UnresolvedMethod`) — degrade.
                let callee = match self.inf.method_resolution(id) {
                    Some(c) => c,
                    None if self.well_typed => {
                        panic!(
                            "MIR lowering: unresolved type-path call `::{method}` on a clean body"
                        )
                    }
                    None => return MExprKind::Missing,
                };
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
                        conn: self.lower_conn(f.out, f.value),
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
            // `const if` folds at lowering: evaluate the (HIR) condition and keep
            // only the taken branch as a block — the discarded arm's (possibly
            // invalid) SV is never produced (planning/comptime_if.md). A
            // still-symbolic condition (the `generate if` case, not yet built)
            // keeps the structural `ConstIf` node; emission rejects it as today.
            ExprKind::ConstIf {
                cond,
                then_branch,
                else_branch,
            } => match crate::hir::const_eval::eval_cond(self.db, self.krate, self.def, *cond) {
                Some(true) => MExprKind::Block(self.lower_block(then_branch)),
                Some(false) => MExprKind::Block(self.lower_block(else_branch)),
                None => MExprKind::ConstIf {
                    cond: self.lower_expr(*cond),
                    then_branch: self.lower_block(then_branch),
                    else_branch: self.lower_block(else_branch),
                },
            },
            // Slicing desugars to a call of the prelude `Slice` method that infer
            // resolved (planning/slice_guards.md): the impl generic (base width)
            // rides in `substs`, the const endpoints as named args (`{lo, hi}` /
            // `{w}`) that the inline splice binds, the base as the receiver, and a
            // runtime offset base as the value arg. The const-if zero-width guard
            // then lives in the spliced method body. Shapes not yet routed (vec,
            // elision, unresolved) fall back to the structural `Slice` node.
            ExprKind::Slice {
                base,
                lo,
                hi,
                width,
            } => {
                let resolved = self.inf.method_resolution(id);
                let base_w = slice_base_width(&self.ty_of(*base));
                let base_ground = base_w.as_ref().is_some_and(|w| {
                    crate::hir::const_eval::eval_const(self.db, self.krate, self.def, w).is_some()
                });
                match (resolved, base_w, *width, *lo, *hi) {
                    // two-endpoint, both present, FULLY GROUND base + endpoints:
                    // `base.slice{lo, hi}()`, routed through the prelude guard. The
                    // base width binds the impl generic in `substs` (by name); the
                    // const endpoints ride as named args the inline splice binds.
                    // A symbolic base/endpoint stays on the structural node — the
                    // inline splice can't yet render a *caller* generic in the
                    // callee's frame (the cross-frame limit; planning/slice_guards.md).
                    (Some(callee), Some(w), None, Some(lo), Some(hi))
                        if base_ground
                            && self.const_param_free(lo)
                            && self.const_param_free(hi) =>
                    {
                        MExprKind::Call {
                            callee,
                            substs: self.slice_impl_substs(callee, &w),
                            receiver: Some(self.lower_expr(*base)),
                            args: Vec::new(),
                            named: vec![
                                MNamedArg {
                                    name: "lo".to_owned(),
                                    conn: Conn::In(self.lower_expr(lo)),
                                },
                                MNamedArg {
                                    name: "hi".to_owned(),
                                    conn: Conn::In(self.lower_expr(hi)),
                                },
                            ],
                        }
                    }
                    // Offset / elision / vec / symbolic / unresolved: keep the
                    // structural node (still emitted by the backend).
                    _ => MExprKind::Slice {
                        base: self.lower_expr(*base),
                        lo: lo.map(|e| self.lower_expr(e)),
                        hi: hi.map(|e| self.lower_expr(e)),
                        width: width.map(|e| self.lower_expr(e)),
                    },
                }
            }
            ExprKind::When { event, body, init } => MExprKind::When {
                event: self.lower_expr(*event),
                body: self.lower_block(body),
                init: init.map(|e| self.lower_expr(e)),
            },
            ExprKind::Block(b) => MExprKind::Block(self.lower_block(b)),
        }
    }

    /// Lower an equation LHS to a [`Place`]: walk the `Local`/`Field`/`Index`
    /// chain to its root local. The chain is collected leaf→base, then reversed
    /// so projections read base→leaf.
    fn lower_place(&mut self, id: ExprId) -> Place {
        let mut projections = Vec::new();
        match self.collect_place(id, &mut projections) {
            Some(base) => {
                projections.reverse();
                Place { base, projections }
            }
            // Only reachable on an ill-typed body (the panics below fire on a
            // well-typed one). Degrade to a degenerate place — this MIR is never
            // emitted (the diagnostics gate blocks it).
            None => {
                debug_assert!(!self.well_typed);
                Place {
                    base: LocalId(0),
                    projections: Vec::new(),
                }
            }
        }
    }

    /// Walk a `Local`/`Field`/`Index` chain to its root local, pushing
    /// projections leaf→base. Returns `None` only on an ill-typed body (a
    /// non-place LHS); on a well-typed body a non-place LHS is a lowering
    /// invariant violation and panics.
    fn collect_place(&mut self, id: ExprId, projs: &mut Vec<Projection>) -> Option<LocalId> {
        match self.body.expr(id).kind.clone() {
            ExprKind::Local(l) => Some(l),
            ExprKind::Field { receiver, field } => {
                projs.push(Projection::Field(field));
                self.collect_place(receiver, projs)
            }
            ExprKind::Index { base, index } => {
                let mi = self.lower_expr(index);
                projs.push(Projection::Index(mi));
                self.collect_place(base, projs)
            }
            // Slice-set (`x[a..b] = y`): a `BitRange` projection (the lvalue dual
            // of a slice read).
            ExprKind::Slice {
                base,
                lo,
                hi,
                width,
            } => {
                let lo = lo.map(|e| self.lower_expr(e));
                let hi = hi.map(|e| self.lower_expr(e));
                let width = width.map(|e| self.lower_expr(e));
                projs.push(Projection::BitRange { lo, hi, width });
                self.collect_place(base, projs)
            }
            other if self.well_typed => panic!(
                "MIR lowering: equation LHS is not a place ({:?}-shaped)",
                std::mem::discriminant(&other)
            ),
            _ => None,
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

    /// Is a slice endpoint free of `ConstParam` (a caller const generic)? Such an
    /// endpoint is symbolic in the caller's frame, which the inline splice can't
    /// render in the callee's frame (the cross-frame limit), so those slices stay
    /// on the structural node. Cheap structural check — must NOT call `mir_of`-
    /// based const-eval here (that would re-enter the in-flight `mir_of` query).
    fn const_param_free(&self, e: ExprId) -> bool {
        match &self.body.expr(e).kind {
            ExprKind::ConstParam(_) => false,
            ExprKind::Number(..)
            | ExprKind::TypedLiteral { .. }
            | ExprKind::Bool(_)
            | ExprKind::Local(_)
            | ExprKind::ConstAssoc { .. }
            | ExprKind::Def(_)
            | ExprKind::Missing => true,
            ExprKind::MethodCall { receiver, args, .. } => {
                self.const_param_free(*receiver) && args.iter().all(|a| self.const_param_free(a.expr))
            }
            ExprKind::Field { receiver, .. } => self.const_param_free(*receiver),
            // Conservative: any other shape stays on the structural node.
            _ => false,
        }
    }

    /// The subst for a desugared slice call: the base width binds the `Slice`
    /// impl's generic (placed by name — the method's own `lo`/`hi`/`w`/`L`
    /// generics precede it and are bound via named args / the value arg, so their
    /// slots are left `Deferred` placeholders the splice overrides).
    fn slice_impl_substs(&self, callee: DefId<'db>, base_w: &ConstArg<'db>) -> Vec<Term<'db>> {
        sig_of(self.db, self.krate, callee)
            .generic_params
            .iter()
            .map(|g| match g.name.as_str() {
                "lo" | "hi" | "w" | "L" => Term::Const(ConstArg::Deferred),
                _ => Term::Const(base_w.clone()),
            })
            .collect()
    }

    /// Lower a connection: an out-connection (`=> target`) is a drive target
    /// (a [`Place`]), an in-connection is a value. This is the one place the
    /// in/out split becomes the place/value split.
    fn lower_conn(&mut self, out: bool, expr: ExprId) -> Conn {
        if out {
            Conn::Out(self.lower_place(expr))
        } else {
            Conn::In(self.lower_expr(expr))
        }
    }

    fn lower_args(&mut self, args: &[ConnArg]) -> Vec<Conn> {
        args.iter()
            .map(|a| self.lower_conn(a.out, a.expr))
            .collect()
    }

    fn lower_named(&mut self, named: &[NamedArg]) -> Vec<MNamedArg> {
        named
            .iter()
            .map(|n| MNamedArg {
                name: n.name.clone(),
                conn: self.lower_conn(n.out, n.expr),
            })
            .collect()
    }
}
