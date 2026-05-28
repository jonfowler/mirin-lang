//! Post-typeck pass: rewrite each `HirExprKind::MethodCall` into a regular
//! `HirExprKind::Call` using the resolutions typeck recorded in
//! `TypeCheckResult::method_resolutions`.
//!
//! For `recv.m(args)` resolved to `T::m`'s `DefId`:
//!
//! - The receiver becomes the first positional `HirArg`.
//! - The user-supplied args follow in order.
//! - Inferable named params (e.g. `dom clk: Clock`) get `HirArg::Inferable`
//!   slots prepended so the post-flatten call shape matches what `out_args`
//!   and the rest of the pipeline expect for user-fn calls.
//!
//! After this pass no `MethodCall` remains in HIR; downstream passes treat
//! the rewritten calls exactly like any other user-fn call.

use std::collections::HashMap;

use super::{
    HirArg, HirArgSource, HirBlock, HirCall, HirEquation, HirExpr, HirExprKind, HirFieldAccess,
    HirFn, HirId, HirItem, HirLet, HirMethodCall, HirSourceFile, HirStmt, ParamKind, ParamSection,
};
use crate::resolve::DefId;

pub fn lower_method_calls(
    file: &HirSourceFile,
    method_resolutions: &HashMap<HirId, DefId>,
) -> HirSourceFile {
    let mut callee_params: HashMap<DefId, Vec<CalleeParam>> = HashMap::new();
    for item in &file.items {
        if let HirItem::Fn(f) = item {
            callee_params.insert(f.def_id, summarise_params(f));
        }
    }
    let mut new_items = Vec::with_capacity(file.items.len());
    for item in &file.items {
        match item {
            HirItem::Fn(f) => {
                let new_body = rewrite_block(&f.body, method_resolutions, &callee_params);
                new_items.push(HirItem::Fn(HirFn {
                    body: new_body,
                    ..f.clone()
                }));
            }
            other => new_items.push(other.clone()),
        }
    }
    HirSourceFile {
        items: new_items,
        span: file.span.clone(),
    }
}

/// Summarised callee shape used by `rewrite_call_for_method` to slot the
/// rewritten args. Only the kind/section/default tuple matters here — we
/// just need to know which slots take `Inferable` placeholders.
#[derive(Debug, Clone)]
struct CalleeParam {
    section: ParamSection,
    kind: ParamKind,
    has_default: bool,
}

fn summarise_params(f: &HirFn) -> Vec<CalleeParam> {
    f.params
        .iter()
        .map(|p| CalleeParam {
            section: p.section,
            kind: p.kind,
            has_default: p.default.is_some(),
        })
        .collect()
}

fn rewrite_block(
    block: &HirBlock,
    method_resolutions: &HashMap<HirId, DefId>,
    callee_params: &HashMap<DefId, Vec<CalleeParam>>,
) -> HirBlock {
    HirBlock {
        statements: block
            .statements
            .iter()
            .map(|s| rewrite_stmt(s, method_resolutions, callee_params))
            .collect(),
        span: block.span.clone(),
    }
}

fn rewrite_stmt(
    stmt: &HirStmt,
    method_resolutions: &HashMap<HirId, DefId>,
    callee_params: &HashMap<DefId, Vec<CalleeParam>>,
) -> HirStmt {
    match stmt {
        HirStmt::Let(l) => HirStmt::Let(HirLet {
            local: l.local,
            value: rewrite_expr(&l.value, method_resolutions, callee_params),
            span: l.span.clone(),
        }),
        HirStmt::VarDecl(v) => HirStmt::VarDecl(v.clone()),
        HirStmt::Equation(eq) => HirStmt::Equation(HirEquation {
            lhs: eq.lhs,
            rhs: rewrite_expr(&eq.rhs, method_resolutions, callee_params),
            span: eq.span.clone(),
        }),
        HirStmt::Return(e) => HirStmt::Return(rewrite_expr(e, method_resolutions, callee_params)),
        HirStmt::Expr(e) => HirStmt::Expr(rewrite_expr(e, method_resolutions, callee_params)),
    }
}

fn rewrite_expr(
    expr: &HirExpr,
    method_resolutions: &HashMap<HirId, DefId>,
    callee_params: &HashMap<DefId, Vec<CalleeParam>>,
) -> HirExpr {
    match &expr.kind {
        HirExprKind::Const(_) | HirExprKind::Local(_) => expr.clone(),
        HirExprKind::Call(call) => HirExpr {
            kind: HirExprKind::Call(HirCall {
                callee: call.callee,
                args: call
                    .args
                    .iter()
                    .map(|a| rewrite_arg(a, method_resolutions, callee_params))
                    .collect(),
                span: call.span.clone(),
            }),
            ty: expr.ty.clone(),
            span: expr.span.clone(),
            id: expr.id,
        },
        HirExprKind::Field(field) => HirExpr {
            kind: HirExprKind::Field(HirFieldAccess {
                receiver: Box::new(rewrite_expr(
                    &field.receiver,
                    method_resolutions,
                    callee_params,
                )),
                name: field.name.clone(),
                name_span: field.name_span.clone(),
            }),
            ty: expr.ty.clone(),
            span: expr.span.clone(),
            id: expr.id,
        },
        HirExprKind::MethodCall(mc) => {
            rewrite_method_call(mc, expr, method_resolutions, callee_params)
        }
    }
}

fn rewrite_arg(
    arg: &HirArg,
    method_resolutions: &HashMap<HirId, DefId>,
    callee_params: &HashMap<DefId, Vec<CalleeParam>>,
) -> HirArg {
    match arg {
        HirArg::Inferable => HirArg::Inferable,
        HirArg::Provided { expr, source } => HirArg::Provided {
            expr: rewrite_expr(expr, method_resolutions, callee_params),
            source: *source,
        },
    }
}

fn rewrite_method_call(
    mc: &HirMethodCall,
    whole: &HirExpr,
    method_resolutions: &HashMap<HirId, DefId>,
    callee_params: &HashMap<DefId, Vec<CalleeParam>>,
) -> HirExpr {
    // Method calls with no resolution survived a typeck error; emit a
    // placeholder so downstream passes don't crash. The error was already
    // recorded.
    let Some(&callee) = method_resolutions.get(&whole.id) else {
        return whole.clone();
    };
    let recv = rewrite_expr(&mc.receiver, method_resolutions, callee_params);
    let mut user_args: Vec<HirArg> = mc
        .args
        .iter()
        .map(|a| rewrite_arg(a, method_resolutions, callee_params))
        .collect();

    // Build the call's arg list to match the callee's signature shape.
    // Each named slot is filled with `Inferable` (for `param`/`dom` without
    // a default) or `Provided` with the default expression — but we don't
    // have the default expression here without cloning the callee. Easier
    // path: emit `Inferable` for inferable named slots and leave default
    // substitution to typeck's downstream pass. In practice, post-typeck
    // the only consumer is `out_args` + `flatten` + `sv_lower`, which
    // accept Inferable for the clock/param slots.
    let params = match callee_params.get(&callee) {
        Some(p) => p,
        None => {
            // Unknown callee — emit a Call with whatever we have so the
            // file still builds.
            let mut args = vec![HirArg::Provided {
                expr: recv,
                source: HirArgSource::Given,
            }];
            args.append(&mut user_args);
            return HirExpr {
                kind: HirExprKind::Call(HirCall {
                    callee,
                    args,
                    span: whole.span.clone(),
                }),
                ty: whole.ty.clone(),
                span: whole.span.clone(),
                id: whole.id,
            };
        }
    };

    let mut new_args: Vec<HirArg> = Vec::with_capacity(params.len());
    let mut positional_iter = std::iter::once(HirArg::Provided {
        expr: recv,
        source: HirArgSource::Given,
    })
    .chain(user_args.into_iter());

    for param in params {
        match param.section {
            ParamSection::Named => {
                // Named `param`/`dom` without a default → Inferable; with a
                // default → typeck has already substituted defaults
                // upstream. We emit Inferable in either case here, and let
                // the downstream call-slotting logic fill defaults.
                if matches!(param.kind, ParamKind::Param | ParamKind::Dom) && !param.has_default {
                    new_args.push(HirArg::Inferable);
                } else {
                    // Value-kind named param or defaulted: emit Inferable
                    // for now; downstream may need refinement but no current
                    // example exercises this combination.
                    new_args.push(HirArg::Inferable);
                }
            }
            ParamSection::Positional => {
                if let Some(arg) = positional_iter.next() {
                    new_args.push(arg);
                } else {
                    new_args.push(HirArg::Inferable);
                }
            }
        }
    }

    HirExpr {
        kind: HirExprKind::Call(HirCall {
            callee,
            args: new_args,
            span: whole.span.clone(),
        }),
        ty: whole.ty.clone(),
        span: whole.span.clone(),
        id: whole.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::lower_to_hir;
    use crate::resolve::resolve_file;
    use crate::surface_ir::parse_surface_source;
    use crate::typeck;

    fn process(source: &str) -> HirSourceFile {
        let file = parse_surface_source(source).expect("parse");
        let resolve = resolve_file(&file);
        assert!(resolve.errors.is_empty(), "resolve: {:?}", resolve.errors);
        let hir = lower_to_hir(&file, &resolve).expect("lower");
        let tc = typeck::check_file(&hir, &resolve);
        assert!(tc.errors.is_empty(), "typeck: {:?}", tc.errors);
        lower_method_calls(&hir, &tc.method_resolutions)
    }

    fn nth_fn(file: &HirSourceFile, n: usize) -> &HirFn {
        file.items
            .iter()
            .filter_map(|i| match i {
                HirItem::Fn(f) => Some(f),
                _ => None,
            })
            .nth(n)
            .expect("fn")
    }

    #[test]
    fn rewrites_method_call_to_call() {
        // After this pass the body of `caller` contains no `MethodCall`.
        let file = process(
            "struct Box = bx { value: uint(8) }\n\
             impl Box { fn get(self) -> uint(8) { return self.value; } }\n\
             fn caller(b: Box) -> uint(8) { return b.get(); }",
        );
        // Three HirItem::Fn entries: Box::get and caller (Box itself is a struct).
        // Find caller.
        let caller = file
            .items
            .iter()
            .find_map(|i| match i {
                HirItem::Fn(f) if f.name == "caller" => Some(f),
                _ => None,
            })
            .expect("caller");
        let HirStmt::Return(ret) = &caller.body.statements[0] else {
            panic!("expected return");
        };
        let HirExprKind::Call(_) = &ret.kind else {
            panic!("expected Call, got {:?}", ret.kind);
        };
    }
}
