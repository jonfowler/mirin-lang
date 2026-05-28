//! Single-driver check: every `var` must have exactly one `Equation` driver.
//!
//! Runs on HIR before type-checking. Two error kinds:
//!
//! - `Undriven`: a `var` declaration has no equation whose LHS is that local.
//! - `MultipleDrivers`: two or more equations share the same LHS.
//!
//! `let` bindings and parameters are excluded — `let` has exactly one value by
//! construction, and parameters are never driven by equations.

use std::collections::HashMap;

use super::{HirFn, HirSourceFile, HirStmt, LocalId};
use crate::SourceSpan;

#[derive(Debug, Clone)]
pub struct DriverError {
    pub kind: DriverErrorKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverErrorKind {
    /// A `var` declaration has no equation driving it.
    Undriven { name: String },
    /// A `var` has more than one equation driving it.
    MultipleDrivers { name: String },
}

impl std::fmt::Display for DriverErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Undriven { name } => write!(f, "`{name}` is declared but never driven"),
            Self::MultipleDrivers { name } => {
                write!(f, "`{name}` has more than one driver")
            }
        }
    }
}

pub fn check_drivers(file: &HirSourceFile) -> Vec<DriverError> {
    let mut errors = Vec::new();
    for item in &file.items {
        if let super::HirItem::Fn(func) = item {
            check_fn(func, &mut errors);
        }
    }
    errors
}

fn check_fn(func: &HirFn, errors: &mut Vec<DriverError>) {
    // Collect VarDecl locals: local → (name, decl_span).
    let mut var_decls: HashMap<LocalId, (String, SourceSpan)> = HashMap::new();
    // Count equations per lhs LocalId.
    let mut equation_counts: HashMap<LocalId, (u32, SourceSpan)> = HashMap::new();

    for stmt in &func.body.statements {
        match stmt {
            HirStmt::VarDecl(v) => {
                let name = func
                    .locals
                    .get(v.local.0 as usize)
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| format!("<local {}>", v.local.0));
                var_decls.insert(v.local, (name, v.span.clone()));
            }
            HirStmt::Equation(eq) => {
                let entry = equation_counts
                    .entry(eq.lhs)
                    .or_insert((0, eq.span.clone()));
                entry.0 += 1;
                // Keep the span of the second driver for the error site.
                if entry.0 == 2 {
                    entry.1 = eq.span.clone();
                }
            }
            _ => {}
        }
    }

    for (local, (name, decl_span)) in &var_decls {
        match equation_counts.get(local) {
            None => {
                errors.push(DriverError {
                    kind: DriverErrorKind::Undriven { name: name.clone() },
                    span: decl_span.clone(),
                });
            }
            Some((count, dup_span)) if *count > 1 => {
                errors.push(DriverError {
                    kind: DriverErrorKind::MultipleDrivers { name: name.clone() },
                    span: dup_span.clone(),
                });
            }
            _ => {}
        }
    }
}

pub fn render_driver_errors(
    errors: &[DriverError],
    source: &str,
    path: Option<&std::path::Path>,
    f: &mut impl std::fmt::Write,
) -> std::fmt::Result {
    for (i, error) in errors.iter().enumerate() {
        if i > 0 {
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

fn excerpt_for_span(source: &str, span: &SourceSpan) -> Option<crate::SourceExcerpt> {
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
    Some(crate::SourceExcerpt {
        line_number: span.start.row + 1,
        line_text,
        highlight_start: start,
        highlight_end: end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::lower_to_hir;
    use crate::resolve::resolve_file;
    use crate::surface_ir::parse_surface_source;

    fn drivers(source: &str) -> Vec<DriverError> {
        let file = parse_surface_source(source).expect("parse failed");
        let resolve = resolve_file(&file);
        assert!(
            resolve.errors.is_empty(),
            "resolve errors: {:?}",
            resolve.errors
        );
        let hir = lower_to_hir(&file, &resolve).expect("hir lowering");
        check_drivers(&hir)
    }

    #[test]
    fn single_driver_is_ok() {
        let errs = drivers(
            "fn f(rstn: Reset @clk) { var count: uint(8) @clk; count = (count + 1).reg(rstn, 0); }",
        );
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn var_with_inline_init_is_ok() {
        let errs = drivers(
            "fn f(rstn: Reset @clk, data: uint(8) @clk) { var acc: uint(8) @clk = (acc + data).reg(rstn, 0); }",
        );
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn undriven_var_is_reported() {
        let errs = drivers("fn f() { var x: uint(8); }");
        assert!(
            errs.iter()
                .any(|e| matches!(&e.kind, DriverErrorKind::Undriven { name } if name == "x")),
            "expected Undriven for x, got: {errs:?}"
        );
    }

    #[test]
    fn multiple_drivers_is_reported() {
        let errs = drivers("fn f(a: uint(8), b: uint(8)) { var x: uint(8); x = a; x = b; }");
        assert!(
            errs.iter().any(
                |e| matches!(&e.kind, DriverErrorKind::MultipleDrivers { name } if name == "x")
            ),
            "expected MultipleDrivers for x, got: {errs:?}"
        );
    }

    #[test]
    fn examples_pass_driver_check() {
        let examples: &[(&str, &str)] = &[
            (
                "add_constant",
                include_str!("../../../../examples/add_constant.plr"),
            ),
            (
                "accumulator",
                include_str!("../../../../examples/accumulator.plr"),
            ),
            ("counter", include_str!("../../../../examples/counter.plr")),
            (
                "mult_add",
                include_str!("../../../../examples/mult_add.plr"),
            ),
            (
                "packet_struct",
                include_str!("../../../../examples/packet_struct.plr"),
            ),
            (
                "pipeline",
                include_str!("../../../../examples/pipeline.plr"),
            ),
            (
                "shift_register",
                include_str!("../../../../examples/shift_register.plr"),
            ),
        ];
        for (name, source) in examples {
            let errs = drivers(source);
            assert!(
                errs.is_empty(),
                "example `{name}` had driver errors: {errs:?}"
            );
        }
    }
}
