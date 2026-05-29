//! Late lowering pass that flattens `HirExprKind::Block` and `HirExprKind::If`
//! out of HIR.
//!
//! Runs after `typeck` (which type-checks the tree-shaped form) and before
//! `method_lower`. Mirrors rustc's THIR→MIR layering: keep blocks and
//! `if`-expressions tree-shaped through type-checking, then convert them into
//! a flat sequence with a single result local that both branches assign to.
//!
//! - `{ stmt; …; tail }` as a value: hoist `stmt; …` into the enclosing
//!   statement list; the expression's value becomes `tail`'s lowered form.
//!   No synthetic local is needed.
//! - `if cond { tail_a } else { tail_b }` as a value: allocate a synthetic
//!   `var __block_N` of the if-expression's type, emit `if (cond) begin
//!   __block_N = tail_a; end else begin __block_N = tail_b; end`, and
//!   substitute `Local(__block_N)` at the expression site.
//!
//! Deeply nested cases compose: an if-expression inside an arg of a call,
//! itself containing another if-expression in one branch, all get lifted to
//! sit in front of the call. Each lifted prelude reorders only the
//! synthesised vars/ifs — user code never gets implicitly reordered, since
//! every original statement still appears in source order.

use std::collections::HashMap;

use super::{
    HirArg, HirBlock, HirBlockExpr, HirCall, HirEquation, HirExpr, HirExprKind, HirFieldAccess,
    HirFn, HirId, HirIfExpr, HirIfStmt, HirItem, HirLet, HirLocalInfo, HirMethodCall,
    HirSourceFile, HirStmt, HirType, HirVarDecl, LocalId,
};
use crate::SourceSpan;

/// Result of the pass: the rewritten HIR plus extended `local_types`. The
/// new locals introduced for if-expression results are added so downstream
/// passes (flatten, sv_lower) can size their declarations correctly.
pub struct BlockExprLowering {
    pub file: HirSourceFile,
    pub local_types: HashMap<LocalId, HirType>,
}

pub fn lower_block_expressions(
    file: &HirSourceFile,
    expr_types: &HashMap<HirId, HirType>,
    local_types: &HashMap<LocalId, HirType>,
) -> BlockExprLowering {
    let mut new_items = Vec::with_capacity(file.items.len());
    let mut all_local_types = local_types.clone();

    for item in &file.items {
        match item {
            HirItem::Fn(func) => {
                let mut ctx = FnCtx::new(func, expr_types, &mut all_local_types);
                let new_body = ctx.lower_block(&func.body);
                let new_locals = std::mem::take(&mut ctx.locals);
                let next_hir_id = ctx.next_hir_id;
                new_items.push(HirItem::Fn(HirFn {
                    locals: new_locals,
                    body: new_body,
                    ..func.clone()
                }));
                let _ = next_hir_id;
            }
            other => new_items.push(other.clone()),
        }
    }

    BlockExprLowering {
        file: HirSourceFile {
            items: new_items,
            span: file.span.clone(),
        },
        local_types: all_local_types,
    }
}

/// Per-function context: owns the (growing) locals vector, tracks fresh
/// HirId allocation, and remembers each expression's type for sizing the
/// synthetic locals.
struct FnCtx<'a> {
    locals: Vec<HirLocalInfo>,
    next_hir_id: u32,
    expr_types: &'a HashMap<HirId, HirType>,
    local_types: &'a mut HashMap<LocalId, HirType>,
}

impl<'a> FnCtx<'a> {
    fn new(
        func: &HirFn,
        expr_types: &'a HashMap<HirId, HirType>,
        local_types: &'a mut HashMap<LocalId, HirType>,
    ) -> Self {
        // Start HirId allocation past the highest id in the input HIR.
        let mut max_id = 0u32;
        scan_max_hir_id_block(&func.body, &mut max_id);
        Self {
            locals: func.locals.clone(),
            next_hir_id: max_id + 1,
            expr_types,
            local_types,
        }
    }

    fn fresh_hir_id(&mut self) -> HirId {
        let id = HirId(self.next_hir_id);
        self.next_hir_id += 1;
        id
    }

    /// Allocate a synthetic `var __block_N` local for an if-expression's
    /// result. Adds it to the fn's locals and records its type so flatten
    /// and sv_lower can read it back.
    fn alloc_result_var(&mut self, ty: HirType, span: SourceSpan) -> LocalId {
        let local = LocalId(self.locals.len() as u32);
        let name = format!("__block_{}", local.0);
        self.locals.push(HirLocalInfo {
            kind: crate::resolve::LocalKind::Var,
            name,
            span,
            // Synthetic local — no surface-level node introduced it.
            // Use a sentinel NodeId; diagnostics won't ever point here.
            surface_node: crate::surface_ir::NodeId(u32::MAX),
        });
        self.local_types.insert(local, ty);
        local
    }

    fn lower_block(&mut self, block: &HirBlock) -> HirBlock {
        let mut out = Vec::new();
        for stmt in &block.statements {
            self.lower_stmt(stmt, &mut out);
        }
        HirBlock {
            statements: out,
            span: block.span.clone(),
        }
    }

    fn lower_stmt(&mut self, stmt: &HirStmt, out: &mut Vec<HirStmt>) {
        match stmt {
            HirStmt::Let(l) => {
                let (pre, value) = self.lower_expr(&l.value);
                out.extend(pre);
                out.push(HirStmt::Let(HirLet {
                    local: l.local,
                    value,
                    span: l.span.clone(),
                }));
            }
            HirStmt::VarDecl(v) => {
                out.push(HirStmt::VarDecl(v.clone()));
            }
            HirStmt::Equation(eq) => {
                let (pre, rhs) = self.lower_expr(&eq.rhs);
                out.extend(pre);
                out.push(HirStmt::Equation(HirEquation {
                    lhs: eq.lhs,
                    rhs,
                    span: eq.span.clone(),
                }));
            }
            HirStmt::Return(e) => {
                let (pre, value) = self.lower_expr(e);
                out.extend(pre);
                out.push(HirStmt::Return(value));
            }
            HirStmt::Expr(e) => {
                let (pre, value) = self.lower_expr(e);
                out.extend(pre);
                out.push(HirStmt::Expr(value));
            }
            HirStmt::If(_) => {
                // `HirStmt::If` is only produced by this pass itself. Reaching
                // it on input means we ran twice; treat as a clone.
                out.push(stmt.clone());
            }
        }
    }

    /// Lower an expression, returning any statements that must precede it.
    /// Pure expressions (no nested block/if) return an empty prelude.
    fn lower_expr(&mut self, expr: &HirExpr) -> (Vec<HirStmt>, HirExpr) {
        match &expr.kind {
            HirExprKind::Const(_) | HirExprKind::Local(_) => (Vec::new(), expr.clone()),
            HirExprKind::Call(call) => {
                let mut pre = Vec::new();
                let mut new_args = Vec::with_capacity(call.args.len());
                for arg in &call.args {
                    match arg {
                        HirArg::Inferable => new_args.push(HirArg::Inferable),
                        HirArg::Provided {
                            expr: arg_expr,
                            source,
                        } => {
                            let (arg_pre, lowered) = self.lower_expr(arg_expr);
                            pre.extend(arg_pre);
                            new_args.push(HirArg::Provided {
                                expr: lowered,
                                source: *source,
                            });
                        }
                    }
                }
                (
                    pre,
                    HirExpr {
                        kind: HirExprKind::Call(HirCall {
                            callee: call.callee,
                            args: new_args,
                            span: call.span.clone(),
                        }),
                        ty: expr.ty.clone(),
                        span: expr.span.clone(),
                        id: expr.id,
                    },
                )
            }
            HirExprKind::Field(field) => {
                let (recv_pre, recv) = self.lower_expr(&field.receiver);
                (
                    recv_pre,
                    HirExpr {
                        kind: HirExprKind::Field(HirFieldAccess {
                            receiver: Box::new(recv),
                            name: field.name.clone(),
                            name_span: field.name_span.clone(),
                        }),
                        ty: expr.ty.clone(),
                        span: expr.span.clone(),
                        id: expr.id,
                    },
                )
            }
            HirExprKind::MethodCall(mc) => {
                let (recv_pre, recv) = self.lower_expr(&mc.receiver);
                let mut pre = recv_pre;
                let mut new_args = Vec::with_capacity(mc.args.len());
                for arg in &mc.args {
                    match arg {
                        HirArg::Inferable => new_args.push(HirArg::Inferable),
                        HirArg::Provided {
                            expr: arg_expr,
                            source,
                        } => {
                            let (arg_pre, lowered) = self.lower_expr(arg_expr);
                            pre.extend(arg_pre);
                            new_args.push(HirArg::Provided {
                                expr: lowered,
                                source: *source,
                            });
                        }
                    }
                }
                (
                    pre,
                    HirExpr {
                        kind: HirExprKind::MethodCall(HirMethodCall {
                            receiver: Box::new(recv),
                            name: mc.name.clone(),
                            name_span: mc.name_span.clone(),
                            args: new_args,
                        }),
                        ty: expr.ty.clone(),
                        span: expr.span.clone(),
                        id: expr.id,
                    },
                )
            }
            HirExprKind::Block(b) => self.lower_block_expr(b, &expr.span),
            HirExprKind::If(if_expr) => self.lower_if_expr(if_expr, expr.id, &expr.span),
        }
    }

    /// `{ stmt; …; tail }` in value position: hoist `stmt; …` into the
    /// caller's pending list and return `tail`'s lowered form as the
    /// expression's value. No synthetic local needed.
    fn lower_block_expr(&mut self, b: &HirBlockExpr, span: &SourceSpan) -> (Vec<HirStmt>, HirExpr) {
        let mut pre = Vec::new();
        for stmt in &b.block.statements {
            self.lower_stmt(stmt, &mut pre);
        }
        match &b.tail {
            Some(tail) => {
                let (tail_pre, tail_val) = self.lower_expr(tail);
                pre.extend(tail_pre);
                (pre, tail_val)
            }
            None => {
                // A block-expression with no tail has no value. Typeck
                // should have flagged the use site; produce a 0-const so
                // downstream passes don't panic.
                (
                    pre,
                    HirExpr {
                        kind: HirExprKind::Const(super::ConstValue::Integer(0)),
                        ty: None,
                        span: span.clone(),
                        id: self.fresh_hir_id(),
                    },
                )
            }
        }
    }

    /// `if cond { tail_a } else { tail_b }` in value position: synthesise
    /// a result var, emit `var __block_N; if (cond) { … __block_N = a; }
    /// else { … __block_N = b; }`, and substitute `Local(__block_N)` at
    /// the expression site.
    fn lower_if_expr(
        &mut self,
        if_expr: &HirIfExpr,
        if_hir_id: HirId,
        span: &SourceSpan,
    ) -> (Vec<HirStmt>, HirExpr) {
        // Look up the if-expression's type so the synthetic var is sized
        // correctly. Fall back to None — flatten and sv_lower will treat
        // it as 1-bit, which keeps the pipeline total.
        let result_ty = self
            .expr_types
            .get(&if_hir_id)
            .cloned()
            .unwrap_or_else(|| HirType {
                kind: super::HirTypeKind::Value(super::ValueType {
                    kind: super::ValueKind::Bool,
                    domain: super::Domain::Unspecified,
                }),
                span: span.clone(),
            });

        let result_local = self.alloc_result_var(result_ty.clone(), span.clone());

        let (cond_pre, cond) = self.lower_expr(&if_expr.condition);

        let then_block = self.lower_branch_to_block(&if_expr.then_branch, result_local);
        let else_block = self.lower_branch_to_block(&if_expr.else_branch, result_local);

        let mut pre = cond_pre;
        pre.push(HirStmt::VarDecl(HirVarDecl {
            local: result_local,
            ty: Some(result_ty),
            span: span.clone(),
        }));
        pre.push(HirStmt::If(HirIfStmt {
            condition: cond,
            then_branch: then_block,
            else_branch: else_block,
            span: span.clone(),
        }));

        (
            pre,
            HirExpr {
                kind: HirExprKind::Local(result_local),
                ty: None,
                span: span.clone(),
                id: self.fresh_hir_id(),
            },
        )
    }

    /// Lower one branch of an if-expression into a `HirBlock` whose last
    /// statement is an `Equation` assigning the branch's tail value to the
    /// shared result local.
    fn lower_branch_to_block(&mut self, branch: &HirBlockExpr, dest: LocalId) -> HirBlock {
        let mut stmts = Vec::new();
        for stmt in &branch.block.statements {
            self.lower_stmt(stmt, &mut stmts);
        }
        let tail_expr = match &branch.tail {
            Some(t) => {
                let (tail_pre, val) = self.lower_expr(t);
                stmts.extend(tail_pre);
                val
            }
            None => HirExpr {
                kind: HirExprKind::Const(super::ConstValue::Integer(0)),
                ty: None,
                span: branch.block.span.clone(),
                id: self.fresh_hir_id(),
            },
        };
        stmts.push(HirStmt::Equation(HirEquation {
            lhs: dest,
            rhs: tail_expr,
            span: branch.block.span.clone(),
        }));
        HirBlock {
            statements: stmts,
            span: branch.block.span.clone(),
        }
    }
}

/// Walk a HirBlock recursively to find the maximum HirId so the pass can
/// allocate fresh ids without collisions.
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
    }
}

fn scan_max_hir_id_expr(expr: &HirExpr, max: &mut u32) {
    if expr.id.0 > *max {
        *max = expr.id.0;
    }
    match &expr.kind {
        HirExprKind::Const(_) | HirExprKind::Local(_) => {}
        HirExprKind::Call(call) => {
            for arg in &call.args {
                if let HirArg::Provided { expr, .. } = arg {
                    scan_max_hir_id_expr(expr, max);
                }
            }
        }
        HirExprKind::Field(field) => scan_max_hir_id_expr(&field.receiver, max),
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
        HirExprKind::If(if_expr) => {
            scan_max_hir_id_expr(&if_expr.condition, max);
            scan_max_hir_id_block(&if_expr.then_branch.block, max);
            if let Some(t) = &if_expr.then_branch.tail {
                scan_max_hir_id_expr(t, max);
            }
            scan_max_hir_id_block(&if_expr.else_branch.block, max);
            if let Some(t) = &if_expr.else_branch.tail {
                scan_max_hir_id_expr(t, max);
            }
        }
    }
}
