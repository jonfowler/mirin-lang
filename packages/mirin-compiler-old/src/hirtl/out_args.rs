//! Pre-flatten pass: rewrite user-function calls into out-argument form.
//!
//! Mirin surface lets a function return a value: `fn add(x, y) -> uint(8)`.
//! That maps badly onto SV module instances, which connect ports — there is
//! no "result of an instance" you can put in an expression. This pass
//! mechanises the conversion:
//!
//! - Each user-fn `(args) -> R` becomes `(args, out result: R)`. The body's
//!   `return e` statements are rewritten to `result = e` equations.
//! - At each call site of such a fn, the caller's binding is converted:
//!     `let x = f(args)`     → `var x: R; f(args, x);`
//!     `lhs = f(args)`       → `f(args, lhs);`
//!     `return f(args)`      → handled via the surrounding fn's return
//!                             rewrite, so this case reduces to the equation
//!                             one above.
//!
//! After this pass, every user-function call sits in `HirStmt::Expr` position
//! with the out-arg appended. `sv_lower` recognises that shape and emits a
//! single `SvItem::Instance`. Prelude calls (`+`, `*`, `.reg`) are not
//! touched — they keep their expression-tree form and are inlined by sv_lower.
//!
//! Calls to user functions appearing inside expressions (e.g. nested:
//! `let z = add(add(x, y), w);`) are lifted into preceding statements
//! through TAC-style normalisation: each inner user-fn call gets a
//! synthesised `var __t: R` and `f(args, __t)`, with the original arg
//! position replaced by `Local(__t)`. After this pass, every user-fn call
//! sits at top level with all arguments atomic (`Const`, `Local`, or
//! `Field` of a Local).

use std::collections::HashMap;
use std::fmt;

use crate::SourceSpan;
use crate::hir::{
    HirArg, HirArgSource, HirBlock, HirCall, HirEquation, HirExpr, HirExprKind, HirFn, HirId,
    HirItem, HirLocalInfo, HirParam, HirSourceFile, HirStmt, HirType, HirVarDecl, LocalId,
    ParamKind, ParamSection,
};
use crate::resolve::{DefId, LocalKind};
use crate::surface::ir::{Direction, NodeId};

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone)]
pub struct OutArgsError {
    pub kind: OutArgsErrorKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutArgsErrorKind {
    /// Reserved for future shape errors. No emitter today.
    #[allow(dead_code)]
    Unsupported { what: &'static str },
}

impl fmt::Display for OutArgsErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { what } => {
                write!(f, "{what} is not supported by the out-args pass")
            }
        }
    }
}

// ============================================================================
// Public entry point
// ============================================================================

/// Rewrite all user-function calls in `file` into out-argument form.
///
/// Returns a new HIR file. Pre-existing nodes are cloned when their shape is
/// unaffected; new statements/locals are inserted where the rewrite needs
/// them. `HirId`s for synthesised `Local` references are allocated from
/// `max_existing_hir_id + 1` upwards so they don't collide with parser-emitted
/// IDs.
pub fn desugar_user_calls(file: &HirSourceFile) -> Result<HirSourceFile, Vec<OutArgsError>> {
    // Build the map of user-fn DefIds that have a return type. These are the
    // ones whose signatures we transform and whose call sites we rewrite.
    // Calls to user fns that already return `()` are left alone. Prelude
    // intrinsics (synthesised `HirFn`s like `reg`) are excluded: their call
    // sites flatten inline as `always_ff`/etc., not as out-arg instance
    // wiring.
    let mut transformed: HashMap<DefId, HirType> = HashMap::new();
    for item in &file.items {
        if let HirItem::Fn(f) = item
            && !f.is_prelude
            && let Some(rt) = &f.return_type
        {
            transformed.insert(f.def_id, rt.clone());
        }
    }

    let mut next_hir_id = max_hir_id(file) + 1;
    let mut errors = Vec::new();
    let mut new_items = Vec::with_capacity(file.items.len());

    for item in &file.items {
        match item {
            HirItem::Fn(f) if f.is_prelude => {
                // Prelude intrinsics carry no body; nothing to rewrite.
                new_items.push(HirItem::Fn(f.clone()));
            }
            HirItem::Fn(f) => match desugar_fn(f, &transformed, &mut next_hir_id) {
                Ok(new_fn) => new_items.push(HirItem::Fn(new_fn)),
                Err(mut errs) => {
                    errors.append(&mut errs);
                    new_items.push(HirItem::Fn(f.clone()));
                }
            },
            other => new_items.push(other.clone()),
        }
    }

    if errors.is_empty() {
        Ok(HirSourceFile {
            items: new_items,
            span: file.span.clone(),
        })
    } else {
        Err(errors)
    }
}

// ============================================================================
// Per-function rewrite
// ============================================================================

fn desugar_fn(
    f: &HirFn,
    transformed: &HashMap<DefId, HirType>,
    next_hir_id: &mut u32,
) -> Result<HirFn, Vec<OutArgsError>> {
    let mut new_params = f.params.clone();
    let mut new_locals = f.locals.clone();

    // 1. Signature transform: add `out result: R` if the fn has a return type.
    let result_local = f.return_type.as_ref().map(|rt| {
        let local = LocalId(new_locals.len() as u32);
        new_locals.push(HirLocalInfo {
            kind: LocalKind::Param {
                owner: f.def_id,
                direction: Some(Direction::Out),
            },
            name: "result".to_owned(),
            span: rt.span.clone(),
            // Synthetic — no surface node. Picked to avoid clashing with
            // parser-emitted ids (which are sequential from 0).
            surface_node: NodeId(u32::MAX),
        });
        new_params.push(HirParam {
            local,
            section: ParamSection::Positional,
            kind: ParamKind::Value,
            direction: Some(Direction::Out),
            ty: rt.clone(),
            default: None,
            span: rt.span.clone(),
        });
        local
    });

    // 2. Body transform.
    let mut ctx = BodyCtx {
        transformed,
        result_local,
        next_hir_id,
        locals: &mut new_locals,
        errors: Vec::new(),
    };
    let new_body = desugar_block(&f.body, &mut ctx);
    if !ctx.errors.is_empty() {
        return Err(ctx.errors);
    }

    Ok(HirFn {
        def_id: f.def_id,
        name: f.name.clone(),
        params: new_params,
        return_type: if result_local.is_some() {
            None
        } else {
            f.return_type.clone()
        },
        locals: new_locals,
        body: new_body,
        is_prelude: f.is_prelude,
        span: f.span.clone(),
    })
}

struct BodyCtx<'a> {
    transformed: &'a HashMap<DefId, HirType>,
    result_local: Option<LocalId>,
    next_hir_id: &'a mut u32,
    locals: &'a mut Vec<HirLocalInfo>,
    errors: Vec<OutArgsError>,
}

impl BodyCtx<'_> {
    fn fresh_hir_id(&mut self) -> HirId {
        let id = HirId(*self.next_hir_id);
        *self.next_hir_id += 1;
        id
    }

    /// Allocate a synthetic local for a lifted user-fn call result.
    fn alloc_temp(&mut self, span: SourceSpan) -> LocalId {
        let local = LocalId(self.locals.len() as u32);
        let name = format!("__call_{}", local.0);
        self.locals.push(HirLocalInfo {
            kind: LocalKind::Let,
            name,
            span,
            surface_node: NodeId(u32::MAX),
        });
        local
    }
}

fn desugar_block(block: &HirBlock, ctx: &mut BodyCtx<'_>) -> HirBlock {
    let mut new_stmts = Vec::with_capacity(block.statements.len());
    for stmt in &block.statements {
        desugar_stmt_into(stmt, ctx, &mut new_stmts);
    }
    HirBlock {
        statements: new_stmts,
        span: block.span.clone(),
    }
}

fn desugar_stmt_into(stmt: &HirStmt, ctx: &mut BodyCtx<'_>, out: &mut Vec<HirStmt>) {
    match stmt {
        HirStmt::Let(l) => desugar_let(l, ctx, out),
        HirStmt::VarDecl(_) => out.push(stmt.clone()),
        HirStmt::Equation(eq) => desugar_equation(eq, ctx, out),
        HirStmt::Return(e) => desugar_return(e, ctx, out),
        HirStmt::Expr(e) => {
            // Lift any nested user-fn calls into preceding statements; emit
            // the lifted expression as the trailing `Expr` statement.
            let lifted = lift_user_calls(e, ctx, out);
            out.push(HirStmt::Expr(lifted));
        }
        HirStmt::If(i) => {
            // Recurse into both branches. The branches contain `Equation`s
            // assigning to the if-result var; if their RHS includes a
            // user-fn call, normal lifting applies. The whole if-statement
            // stays put.
            let then_branch = desugar_block(&i.then_branch, ctx);
            let else_branch = desugar_block(&i.else_branch, ctx);
            out.push(HirStmt::If(crate::hir::HirIfStmt {
                condition: i.condition.clone(),
                then_branch,
                else_branch,
                span: i.span.clone(),
            }));
        }
        HirStmt::AlwaysFf(a) => {
            // `lift_user_calls` can lift any user-fn call inside the
            // D-input expression into preceding statements; the always_ff
            // statement itself is preserved.
            let d_input = lift_user_calls(&a.d_input, ctx, out);
            out.push(HirStmt::AlwaysFf(crate::hir::HirAlwaysFfStmt {
                clock: a.clock,
                dest: a.dest,
                d_input,
                span: a.span.clone(),
            }));
        }
    }
}

fn desugar_let(l: &crate::hir::HirLet, ctx: &mut BodyCtx<'_>, out: &mut Vec<HirStmt>) {
    // Lift nested user-fn calls inside the let's value first. This ensures
    // that by the time we look at the value, any inner user-fn calls have
    // been hoisted to preceding statements with their out-arg bindings.
    let lifted_value = lift_user_calls(&l.value, ctx, out);

    if let HirExprKind::Call(call) = &lifted_value.kind
        && let Some(ret_ty) = ctx.transformed.get(&call.callee).cloned()
    {
        // `let x = user_fn(args)` → `var x: R; user_fn(args, x);`
        out.push(HirStmt::VarDecl(HirVarDecl {
            local: l.local,
            ty: Some(ret_ty),
            span: l.span.clone(),
        }));
        out.push(HirStmt::Expr(append_outarg(
            call,
            l.local,
            l.value.span.clone(),
            ctx,
        )));
        return;
    }
    // Not a user-fn call (or void fn) — emit a let with the lifted value.
    out.push(HirStmt::Let(crate::hir::HirLet {
        local: l.local,
        value: lifted_value,
        span: l.span.clone(),
    }));
}

fn desugar_equation(eq: &HirEquation, ctx: &mut BodyCtx<'_>, out: &mut Vec<HirStmt>) {
    let lifted_rhs = lift_user_calls(&eq.rhs, ctx, out);
    if let HirExprKind::Call(call) = &lifted_rhs.kind
        && ctx.transformed.contains_key(&call.callee)
    {
        // `lhs = user_fn(args)` → `user_fn(args, lhs);`
        out.push(HirStmt::Expr(append_outarg(
            call,
            eq.lhs,
            eq.rhs.span.clone(),
            ctx,
        )));
        return;
    }
    out.push(HirStmt::Equation(HirEquation {
        lhs: eq.lhs,
        rhs: lifted_rhs,
        span: eq.span.clone(),
    }));
}

fn desugar_return(e: &HirExpr, ctx: &mut BodyCtx<'_>, out: &mut Vec<HirStmt>) {
    let Some(result_local) = ctx.result_local else {
        // Function has no return type — `return` shouldn't appear, but pass
        // it through if it does (after lifting nested calls).
        let lifted = lift_user_calls(e, ctx, out);
        out.push(HirStmt::Return(lifted));
        return;
    };
    // Convert `return e` to an equation `result = e`, then run the equation
    // rewrite so a `return user_fn(args)` becomes `user_fn(args, result);`.
    let synth_eq = HirEquation {
        lhs: result_local,
        rhs: e.clone(),
        span: e.span.clone(),
    };
    desugar_equation(&synth_eq, ctx, out);
}

// ============================================================================
// TAC lifting: hoist nested user-fn calls into preceding statements
// ============================================================================

/// Walk `expr` and lift every user-fn call beneath the root into a preceding
/// `var __t; f(args, __t);` pair, replacing the call site with `Local(__t)`.
/// The expression at the root (if it's itself a user-fn call) is left in
/// place — the caller decides whether to rewrite it to out-arg form (let /
/// equation / return) or keep it as a void-returning expression statement.
fn lift_user_calls(expr: &HirExpr, ctx: &mut BodyCtx<'_>, out: &mut Vec<HirStmt>) -> HirExpr {
    match &expr.kind {
        HirExprKind::Call(call) => {
            // Recursively lift each arg expression; if the resulting arg is
            // itself a user-fn call, hoist it.
            let mut new_args: Vec<HirArg> = Vec::with_capacity(call.args.len());
            for arg in &call.args {
                match arg {
                    HirArg::Inferable => new_args.push(HirArg::Inferable),
                    HirArg::Provided {
                        expr: arg_expr,
                        source,
                    } => {
                        let lifted = lift_user_calls(arg_expr, ctx, out);
                        // If the lifted arg is now a user-fn call, hoist it
                        // into a `var __t; f(args, __t);` pair and replace
                        // with `Local(__t)`.
                        if let HirExprKind::Call(inner) = &lifted.kind
                            && let Some(ret_ty) = ctx.transformed.get(&inner.callee).cloned()
                        {
                            let temp = ctx.alloc_temp(arg_expr.span.clone());
                            out.push(HirStmt::VarDecl(HirVarDecl {
                                local: temp,
                                ty: Some(ret_ty),
                                span: arg_expr.span.clone(),
                            }));
                            out.push(HirStmt::Expr(append_outarg(
                                inner,
                                temp,
                                lifted.span.clone(),
                                ctx,
                            )));
                            let local_expr = HirExpr {
                                kind: HirExprKind::Local(temp),
                                ty: None,
                                span: arg_expr.span.clone(),
                                id: ctx.fresh_hir_id(),
                            };
                            new_args.push(HirArg::Provided {
                                expr: local_expr,
                                source: *source,
                            });
                        } else {
                            new_args.push(HirArg::Provided {
                                expr: lifted,
                                source: *source,
                            });
                        }
                    }
                }
            }
            HirExpr {
                kind: HirExprKind::Call(HirCall {
                    callee: call.callee,
                    args: new_args,
                    span: call.span.clone(),
                }),
                ty: expr.ty.clone(),
                span: expr.span.clone(),
                id: expr.id,
            }
        }
        HirExprKind::Const(_)
        | HirExprKind::Local(_)
        | HirExprKind::Param(_)
        | HirExprKind::ConstVar(_)
        | HirExprKind::Field(_) => expr.clone(),
        HirExprKind::MethodCall(_) => unreachable!(
            "MethodCall should be lowered to Call by `hir::method_lower` before out_args"
        ),
        HirExprKind::Block(_) | HirExprKind::If(_) | HirExprKind::When(_) => {
            unreachable!(
                "Block/If/When should be flattened by lower_block_expressions before out_args"
            )
        }
    }
}

/// Build a new call expression equal to `call` but with `Local(out_local)`
/// appended as a trailing `Provided` arg — the out-arg shape for transformed
/// user-fn calls.
fn append_outarg(
    call: &HirCall,
    out_local: LocalId,
    span: SourceSpan,
    ctx: &mut BodyCtx<'_>,
) -> HirExpr {
    let mut new_args = call.args.clone();
    let out_arg_expr = HirExpr {
        kind: HirExprKind::Local(out_local),
        ty: None,
        span: span.clone(),
        id: ctx.fresh_hir_id(),
    };
    new_args.push(HirArg::Provided {
        expr: out_arg_expr,
        source: HirArgSource::Given,
    });
    HirExpr {
        kind: HirExprKind::Call(HirCall {
            callee: call.callee,
            args: new_args,
            span: call.span.clone(),
        }),
        ty: None,
        span,
        id: ctx.fresh_hir_id(),
    }
}

// ============================================================================
// HirId allocation
// ============================================================================

fn max_hir_id(file: &HirSourceFile) -> u32 {
    let mut max = 0u32;
    for item in &file.items {
        if let HirItem::Fn(f) = item {
            walk_block_max(&f.body, &mut max);
        }
    }
    max
}

fn walk_block_max(block: &HirBlock, max: &mut u32) {
    for stmt in &block.statements {
        match stmt {
            HirStmt::Let(l) => walk_expr_max(&l.value, max),
            HirStmt::VarDecl(_) => {}
            HirStmt::Equation(eq) => walk_expr_max(&eq.rhs, max),
            HirStmt::Return(e) => walk_expr_max(e, max),
            HirStmt::Expr(e) => walk_expr_max(e, max),
            HirStmt::If(i) => {
                walk_expr_max(&i.condition, max);
                walk_block_max(&i.then_branch, max);
                walk_block_max(&i.else_branch, max);
            }
            HirStmt::AlwaysFf(a) => walk_expr_max(&a.d_input, max),
        }
    }
}

fn walk_expr_max(e: &HirExpr, max: &mut u32) {
    if e.id.0 != u32::MAX {
        *max = (*max).max(e.id.0);
    }
    if let HirExprKind::Call(c) = &e.kind {
        for arg in &c.args {
            if let HirArg::Provided { expr, .. } = arg {
                walk_expr_max(expr, max);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{lower_to_hir, *};
    use crate::resolve::resolve_file;
    use crate::surface::ir::parse_surface_source;

    fn desugar(source: &str) -> HirSourceFile {
        let file = parse_surface_source(source).expect("parse");
        let resolve = resolve_file(&file);
        assert!(resolve.errors.is_empty(), "resolve: {:?}", resolve.errors);
        let hir = lower_to_hir(&file, &resolve).expect("hir lower");
        desugar_user_calls(&hir).expect("desugar")
    }

    fn nth_fn(file: &HirSourceFile, n: usize) -> &HirFn {
        file.items
            .iter()
            .filter_map(|item| match item {
                HirItem::Fn(f) => Some(f),
                _ => None,
            })
            .nth(n)
            .expect("fn")
    }

    #[test]
    fn scalar_return_becomes_out_param() {
        let file = desugar("fn id(x: uint(8)) -> uint(8) { return x; }");
        let f = nth_fn(&file, 0);
        // Original param + synthesised `result` out-param.
        assert_eq!(f.params.len(), 2);
        let result_param = &f.params[1];
        assert!(matches!(result_param.direction, Some(Direction::Out)));
        assert_eq!(f.locals[result_param.local.0 as usize].name, "result");
        // Return type cleared.
        assert!(f.return_type.is_none());
        // Body's `return x` rewritten to `result = x` (an Equation).
        assert!(matches!(f.body.statements[0], HirStmt::Equation(_)));
    }

    #[test]
    fn user_call_in_let_becomes_var_plus_expr_call() {
        let file = desugar(
            "fn id(x: uint(8)) -> uint(8) { return x; }\n\
             fn caller(y: uint(8)) -> uint(8) { let z = id(y); return z; }",
        );
        let caller = nth_fn(&file, 1);
        // Expect: VarDecl(z), Expr(Call(id, [y, z])), Equation(result = z)
        assert!(matches!(caller.body.statements[0], HirStmt::VarDecl(_)));
        let HirStmt::Expr(call_expr) = &caller.body.statements[1] else {
            panic!("expected Expr(Call), got {:?}", caller.body.statements[1]);
        };
        let HirExprKind::Call(call) = &call_expr.kind else {
            panic!("expected Call");
        };
        // Original call had 1 positional arg (y); now has 2 (y, z).
        assert_eq!(call.args.len(), 2);
    }

    #[test]
    fn user_call_in_equation_rhs_becomes_expr_call_with_lhs_outarg() {
        let file = desugar(
            "fn id(x: uint(8)) -> uint(8) { return x; }\n\
             fn caller(y: uint(8), out r: uint(8)) { r = id(y); }",
        );
        let caller = nth_fn(&file, 1);
        // Expect: Expr(Call(id, [y, r]))
        let HirStmt::Expr(call_expr) = &caller.body.statements[0] else {
            panic!("expected Expr, got {:?}", caller.body.statements[0]);
        };
        let HirExprKind::Call(call) = &call_expr.kind else {
            panic!("expected Call");
        };
        assert_eq!(call.args.len(), 2);
    }

    #[test]
    fn void_fn_is_unchanged() {
        let file = desugar("fn noop(x: uint(8)) { let r = x; }");
        let f = nth_fn(&file, 0);
        assert!(f.return_type.is_none());
        // No synthetic `result` param.
        assert_eq!(f.params.len(), 1);
    }

    #[test]
    fn primitives_are_not_rewritten() {
        // `+` is a prelude call; should not be turned into out-arg form.
        let file = desugar("fn add_one(x: uint(8)) -> uint(8) { return x + 1; }");
        let f = nth_fn(&file, 0);
        // Body should still contain the Equation with a Call(+,...) rhs.
        let HirStmt::Equation(eq) = &f.body.statements[0] else {
            panic!("expected Equation");
        };
        assert!(matches!(eq.rhs.kind, HirExprKind::Call(_)));
    }
}
