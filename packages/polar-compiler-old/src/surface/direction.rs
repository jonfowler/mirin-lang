use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::resolve::{DefId, DefKind, Res, ResolveResult};
use crate::surface::ir::{
    Block, ConnectionDirection, Expression, FunctionDefinition, Item, NamedArgument,
    PositionalArgument, PostfixExpression, PostfixOperation, SourceFile, Statement,
};
use crate::{SourceExcerpt, SourceSpan};

/// What a named argument did that the callee's signature does not permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectionErrorKind {
    /// The named argument's name doesn't match any named parameter of the callee.
    UnknownNamedArgument { callee: String, arg: String },
    /// `=>` (source) was used on a parameter that is not a port `out` field —
    /// in the first pass that means any function named parameter.
    SourceArrowOnSink { callee: String, arg: String },
    /// An explicit `in`/`out` keyword on the named argument disagrees with the
    /// operator (`=` implies `in`, `=>` implies `out`).
    DirectionKeywordMismatch {
        keyword: ConnectionDirection,
        operator: NamedArgumentOperator,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedArgumentOperator {
    Sink,
    Source,
}

impl NamedArgumentOperator {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sink => "=",
            Self::Source => "=>",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DirectionError {
    pub kind: DirectionErrorKind,
    pub span: SourceSpan,
}

impl fmt::Display for DirectionErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNamedArgument { callee, arg } => {
                write!(f, "`{callee}` has no named parameter `{arg}`")
            }
            Self::SourceArrowOnSink { callee, arg } => write!(
                f,
                "`=>` cannot drive `{callee}`'s `{arg}`; only port `out` fields accept source connections"
            ),
            Self::DirectionKeywordMismatch { keyword, operator } => {
                let keyword = match keyword {
                    ConnectionDirection::In => "in",
                    ConnectionDirection::Out => "out",
                };
                write!(
                    f,
                    "direction keyword `{keyword}` is inconsistent with operator `{}`",
                    operator.as_str()
                )
            }
        }
    }
}

pub fn render_direction_errors(
    errors: &[DirectionError],
    source: &str,
    path: Option<&Path>,
    f: &mut impl fmt::Write,
) -> fmt::Result {
    for (index, error) in errors.iter().enumerate() {
        if index > 0 {
            writeln!(f)?;
        }
        writeln!(f, "error: {}", error.kind)?;
        if let Some(path) = path {
            writeln!(
                f,
                " --> {}:{}:{}",
                path.display(),
                error.span.start.row + 1,
                error.span.start.column + 1
            )?;
        } else {
            writeln!(
                f,
                " --> {}:{}",
                error.span.start.row + 1,
                error.span.start.column + 1
            )?;
        }
        if let Some(excerpt) = excerpt_for_span(source, &error.span) {
            writeln!(f, "  |")?;
            writeln!(f, "{:>2} | {}", excerpt.line_number, excerpt.line_text)?;
            writeln!(
                f,
                "  | {}{}",
                " ".repeat(excerpt.highlight_start),
                "^".repeat(
                    excerpt
                        .highlight_end
                        .saturating_sub(excerpt.highlight_start)
                )
            )?;
        }
    }
    Ok(())
}

fn excerpt_for_span(source: &str, span: &SourceSpan) -> Option<SourceExcerpt> {
    let line_text = source.lines().nth(span.start.row)?.to_owned();
    let start = span.start.column.min(line_text.len());
    let end = if span.start.row == span.end.row {
        span.end
            .column
            .max(start + 1)
            .min(line_text.len().max(start + 1))
    } else {
        line_text.len().max(start + 1)
    };
    Some(SourceExcerpt {
        line_number: span.start.row + 1,
        line_text,
        highlight_start: start,
        highlight_end: end,
    })
}

/// Run the direction-checking pass over a resolved file.
///
/// Structural pass that runs after name resolution and before HIR lowering
/// (see `planning/ir_pipeline.md`). Field directions on ports and named
/// parameters are explicit, so the check does not depend on type information.
///
/// For the first pass this covers calls to top-level `fn` definitions. Method
/// calls (`x.method(...)`) and path-rooted calls (`Type::member(...)`) are
/// deferred until type information is available.
pub fn check_directions(file: &SourceFile, resolve: &ResolveResult) -> Vec<DirectionError> {
    let mut callees = HashMap::new();
    collect_callees(&file.items, resolve, &mut callees);
    let mut errors = Vec::new();
    check_items(&file.items, &callees, resolve, &mut errors);
    errors
}

/// Walk a module's items (recursing into nested `mod`s) and check every fn
/// body. Modules carry no direction semantics — they only nest the item set.
fn check_items(
    items: &[Item],
    callees: &HashMap<DefId, &FunctionDefinition>,
    resolve: &ResolveResult,
    errors: &mut Vec<DirectionError>,
) {
    for item in items {
        match item {
            Item::Fn(func) => check_block(&func.body, callees, resolve, errors),
            Item::Impl(impl_block) => {
                for func in &impl_block.functions {
                    check_block(&func.body, callees, resolve, errors);
                }
            }
            Item::Mod(m) => check_items(m.items(), callees, resolve, errors),
            Item::Struct(_) | Item::Port(_) | Item::Use(_) => {}
        }
    }
}

fn collect_callees<'a>(
    items: &'a [Item],
    resolve: &ResolveResult,
    table: &mut HashMap<DefId, &'a FunctionDefinition>,
) {
    for item in items {
        match item {
            Item::Fn(func) => {
                if let Some(&Res::Def(_, def_id)) = resolve.resolutions.get(&func.name.id) {
                    table.insert(def_id, func);
                }
            }
            Item::Mod(m) => collect_callees(m.items(), resolve, table),
            _ => {}
        }
    }
}

fn check_block<'a>(
    block: &Block,
    callees: &HashMap<DefId, &'a FunctionDefinition>,
    resolve: &ResolveResult,
    errors: &mut Vec<DirectionError>,
) {
    for stmt in &block.statements {
        match stmt {
            Statement::Let(l) => check_expr(&l.value, callees, resolve, errors),
            Statement::Return(r) => check_expr(&r.value, callees, resolve, errors),
            Statement::Var(v) => {
                if let Some(init) = &v.init {
                    check_expr(init, callees, resolve, errors);
                }
            }
            Statement::Assignment(a) => {
                check_expr(&a.left, callees, resolve, errors);
                check_expr(&a.right, callees, resolve, errors);
            }
            Statement::Expression(e) => check_expr(&e.value, callees, resolve, errors),
        }
    }
    if let Some(tail) = &block.tail {
        check_expr(tail, callees, resolve, errors);
    }
}

fn check_expr<'a>(
    expr: &Expression,
    callees: &HashMap<DefId, &'a FunctionDefinition>,
    resolve: &ResolveResult,
    errors: &mut Vec<DirectionError>,
) {
    match expr {
        Expression::Identifier(_) | Expression::Number(_) | Expression::Path(_) => {}
        Expression::Binary(b) => {
            check_expr(&b.left, callees, resolve, errors);
            check_expr(&b.right, callees, resolve, errors);
        }
        Expression::Postfix(p) => check_postfix(p, callees, resolve, errors),
        Expression::RecordConstructor(r) => {
            for field in &r.fields {
                check_expr(&field.value, callees, resolve, errors);
            }
        }
        Expression::Block(b) => check_block(b, callees, resolve, errors),
        Expression::If(if_expr) => {
            check_expr(&if_expr.condition, callees, resolve, errors);
            check_block(&if_expr.then_branch, callees, resolve, errors);
            check_block(&if_expr.else_branch, callees, resolve, errors);
        }
        Expression::When(when_expr) => {
            check_expr(&when_expr.event, callees, resolve, errors);
            check_block(&when_expr.body, callees, resolve, errors);
        }
    }
}

fn check_postfix<'a>(
    expr: &PostfixExpression,
    callees: &HashMap<DefId, &'a FunctionDefinition>,
    resolve: &ResolveResult,
    errors: &mut Vec<DirectionError>,
) {
    check_expr(&expr.receiver, callees, resolve, errors);

    // Only direct calls `f { ... }(...)` to a top-level fn are direction-checked.
    // Anything that goes through a field access (`x.method`) is a method-style
    // call: resolving the callee needs type information we don't have yet.
    let direct_callee = match expr.receiver.as_ref() {
        Expression::Identifier(ident) => match resolve.resolutions.get(&ident.id) {
            Some(&Res::Def(DefKind::Fn, def_id)) => callees.get(&def_id).copied(),
            _ => None,
        },
        _ => None,
    };

    let mut callee_for_call: Option<&FunctionDefinition> = direct_callee;
    let mut consumed_call = false;
    for op in &expr.operations {
        match op {
            PostfixOperation::Field(_) => {
                callee_for_call = None;
            }
            PostfixOperation::NamedArguments(list) => {
                if !consumed_call {
                    if let Some(callee) = callee_for_call {
                        for arg in &list.arguments {
                            check_named_arg(arg, callee, errors);
                        }
                    }
                    consumed_call = true;
                }
                for arg in &list.arguments {
                    if let NamedArgument::Sink(s) = arg {
                        check_expr(&s.value, callees, resolve, errors);
                    }
                }
            }
            PostfixOperation::Arguments(list) => {
                consumed_call = true;
                for inner in &list.arguments {
                    match inner {
                        PositionalArgument::Value(expr) => {
                            check_expr(expr, callees, resolve, errors);
                        }
                        PositionalArgument::OutBind(_) => {
                            // The `target` identifier is a Local reference,
                            // resolved by the resolver. Direction check has
                            // nothing extra to do here — the callee's slot
                            // direction is read at the call shape level
                            // (TODO: validate that slot has Out direction).
                        }
                    }
                }
            }
        }
    }
}

fn check_named_arg(
    arg: &NamedArgument,
    callee: &FunctionDefinition,
    errors: &mut Vec<DirectionError>,
) {
    let (name, direction, operator) = match arg {
        NamedArgument::Sink(s) => (&s.name, s.direction, NamedArgumentOperator::Sink),
        NamedArgument::Source(s) => (&s.name, s.direction, NamedArgumentOperator::Source),
    };

    if let Some(keyword) = direction
        && !keyword_matches_operator(keyword, operator)
    {
        errors.push(DirectionError {
            kind: DirectionErrorKind::DirectionKeywordMismatch { keyword, operator },
            span: name.span.clone(),
        });
    }

    let Some(param) = callee
        .named_parameters
        .iter()
        .find(|p| p.name.text == name.text)
    else {
        errors.push(DirectionError {
            kind: DirectionErrorKind::UnknownNamedArgument {
                callee: callee.name.text.clone(),
                arg: name.text.clone(),
            },
            span: name.span.clone(),
        });
        return;
    };

    // Source-arrow (`=>`) requires an `out`-direction named param on the
    // callee — that's the only thing the function writes back through the
    // named slot. Sink-arrow (`=`) is the inverse.
    let param_is_out = matches!(param.direction, Some(crate::surface::ir::Direction::Out));
    if matches!(operator, NamedArgumentOperator::Source) && !param_is_out {
        errors.push(DirectionError {
            kind: DirectionErrorKind::SourceArrowOnSink {
                callee: callee.name.text.clone(),
                arg: name.text.clone(),
            },
            span: name.span.clone(),
        });
    }
}

fn keyword_matches_operator(keyword: ConnectionDirection, operator: NamedArgumentOperator) -> bool {
    matches!(
        (keyword, operator),
        (ConnectionDirection::In, NamedArgumentOperator::Sink)
            | (ConnectionDirection::Out, NamedArgumentOperator::Source)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve_file;
    use crate::surface::ir::parse_surface_source;

    fn check(source: &str) -> Vec<DirectionError> {
        let file = parse_surface_source(source).expect("parse failed");
        let resolve = resolve_file(&file);
        assert!(
            resolve.errors.is_empty(),
            "unexpected resolve errors: {:?}",
            resolve.errors
        );
        check_directions(&file, &resolve)
    }

    #[test]
    fn valid_named_arg_passes() {
        let errors = check(
            "fn target { c: uint(8) = 0 } ( a: uint(8) ) { let r = a; }\n\
             fn caller ( x: uint(8) ) { let r = target { c = 5 }(x); }",
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn shorthand_named_arg_passes() {
        // `target { c }(...)` is shorthand for `target { c = c }(...)`.
        let errors = check(
            "fn target { c: uint(8) = 0 } ( a: uint(8) ) { let r = a; }\n\
             fn caller ( c: uint(8), x: uint(8) ) { let r = target { c }(x); }",
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn unknown_named_arg_is_reported() {
        let errors = check(
            "fn target { c: uint(8) = 0 } ( a: uint(8) ) { let r = a; }\n\
             fn caller ( x: uint(8) ) { let r = target { typo = 5 }(x); }",
        );
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(matches!(
            &errors[0].kind,
            DirectionErrorKind::UnknownNamedArgument { callee, arg }
                if callee == "target" && arg == "typo"
        ));
    }

    #[test]
    fn source_arrow_on_fn_param_is_reported() {
        let errors = check(
            "fn target { c: uint(8) = 0 } ( a: uint(8) ) { let r = a; }\n\
             fn caller ( x: uint(8) ) { var captured: uint(8); let r = target { c => captured }(x); }",
        );
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(matches!(
            &errors[0].kind,
            DirectionErrorKind::SourceArrowOnSink { callee, arg }
                if callee == "target" && arg == "c"
        ));
    }

    #[test]
    fn direction_keyword_mismatch_is_reported() {
        let errors = check(
            "fn target { c: uint(8) = 0 } ( a: uint(8) ) { let r = a; }\n\
             fn caller ( x: uint(8) ) { let r = target { out c = 5 }(x); }",
        );
        // `out` with `=` is inconsistent; also `c` is still a valid name so the
        // unknown-arg path doesn't fire. We expect exactly the keyword mismatch.
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(matches!(
            &errors[0].kind,
            DirectionErrorKind::DirectionKeywordMismatch {
                keyword: ConnectionDirection::Out,
                operator: NamedArgumentOperator::Sink,
            }
        ));
    }

    #[test]
    fn method_call_is_not_direction_checked() {
        // `.reg` is a method-style call (receiver is a local). Direction
        // checking has no signature for it yet and must not error.
        let errors = check("fn f ( rstn: Reset, data: uint(8) ) { let r = data.reg(rstn, 0); }");
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn working_examples_pass_direction_check() {
        for (name, source) in crate::test_support::working_examples() {
            let errors = check(&source);
            assert!(
                errors.is_empty(),
                "example `{name}` had unexpected direction errors: {errors:?}",
            );
        }
    }

    #[test]
    fn direction_fail_unknown_named_arg() {
        let source = include_str!("../../../../examples/fail-expected/unknown-named-arg.plr");
        let file = parse_surface_source(source).expect("parse failed");
        let resolve = resolve_file(&file);
        assert!(
            resolve.errors.is_empty(),
            "resolver should accept this file; errors: {:?}",
            resolve.errors
        );
        let errors = check_directions(&file, &resolve);
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(matches!(
            &errors[0].kind,
            DirectionErrorKind::UnknownNamedArgument { callee, arg }
                if callee == "target" && arg == "typo"
        ));
    }

    #[test]
    fn direction_fail_source_arrow_on_fn_param() {
        let source =
            include_str!("../../../../examples/fail-expected/source-arrow-on-fn-param.plr");
        let file = parse_surface_source(source).expect("parse failed");
        let resolve = resolve_file(&file);
        assert!(
            resolve.errors.is_empty(),
            "resolver should accept this file; errors: {:?}",
            resolve.errors
        );
        let errors = check_directions(&file, &resolve);
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(matches!(
            &errors[0].kind,
            DirectionErrorKind::SourceArrowOnSink { callee, arg }
                if callee == "target" && arg == "rstn"
        ));
    }
}
