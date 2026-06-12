use std::path::{Path, PathBuf};
use std::{env, fs, process};

use polar_compiler_old::hirt::typeck;
use polar_compiler_old::{
    ParseError, check_directions, check_drivers, check_width_obligations, emit_sv,
    flatten_aggregates, hir, load_crate_from_fs, lower_to_sv, parse_source_with_diagnostics,
    render_direction_errors, render_driver_errors, render_emit_errors, render_parse_error,
    render_resolve_errors, resolve_file,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmitMode {
    Sv,
    Cst,
}

#[derive(Debug)]
struct CliArgs {
    input: PathBuf,
    emit: EmitMode,
    out_dir: PathBuf,
}

fn print_usage(program: &str) {
    eprintln!("usage: {program} [--emit sv|cst] [--out <dir>] <path-to-.plr-file>");
    eprintln!();
    eprintln!("  --emit sv   (default) write SystemVerilog to <out-dir>/<stem>.sv");
    eprintln!("  --emit cst  print the concrete syntax tree to stdout");
    eprintln!("  --out <dir> output directory for `--emit sv` (default: ./sv/)");
}

fn parse_args(program: &str) -> Result<CliArgs, i32> {
    let mut args = env::args_os().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut emit = EmitMode::Sv;
    let mut out_dir: PathBuf = PathBuf::from("./sv/");

    while let Some(raw) = args.next() {
        let s = match raw.to_str() {
            Some(s) => s.to_owned(),
            None => {
                eprintln!("error: non-UTF8 argument");
                return Err(2);
            }
        };
        match s.as_str() {
            "--emit" => {
                let value = args.next().ok_or_else(|| {
                    eprintln!("error: `--emit` expects a value (sv or cst)");
                    2
                })?;
                let value = value.to_string_lossy().into_owned();
                emit = match value.as_str() {
                    "sv" => EmitMode::Sv,
                    "cst" => EmitMode::Cst,
                    other => {
                        eprintln!("error: unknown emit mode `{other}` (expected sv or cst)");
                        return Err(2);
                    }
                };
            }
            "--out" => {
                let value = args.next().ok_or_else(|| {
                    eprintln!("error: `--out` expects a directory path");
                    2
                })?;
                out_dir = PathBuf::from(value);
            }
            "-h" | "--help" => {
                print_usage(program);
                return Err(0);
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                print_usage(program);
                return Err(2);
            }
            _ => {
                if input.is_some() {
                    eprintln!("error: expected exactly one input file");
                    print_usage(program);
                    return Err(2);
                }
                input = Some(PathBuf::from(s));
            }
        }
    }

    let input = input.ok_or_else(|| {
        print_usage(program);
        2
    })?;
    Ok(CliArgs {
        input,
        emit,
        out_dir,
    })
}

fn main() {
    let program = env::args_os()
        .next()
        .and_then(|a| a.into_string().ok())
        .unwrap_or_else(|| "polar-compiler".to_owned());

    let args = match parse_args(&program) {
        Ok(a) => a,
        Err(code) => process::exit(code),
    };

    // `--emit cst` is a single-file debug aid: parse just the root file and
    // print its concrete syntax tree (the multi-file loader has no single CST).
    if args.emit == EmitMode::Cst {
        emit_root_cst(&args.input);
        return;
    }

    // Load the whole crate: the root file plus every `mod foo;` it pulls in,
    // producing one combined `SourceFile` over one combined source buffer.
    let loaded = match load_crate_from_fs(&args.input) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            // A file we couldn't read is an environment/IO failure (exit 2);
            // parse/lower failures are compile errors (exit 1).
            process::exit(match e {
                polar_compiler_old::LoadError::Read { .. } => 2,
                _ => 1,
            });
        }
    };
    let source = loaded.source;
    let file = loaded.file;

    if !loaded.diagnostics.is_empty() {
        let mut rendered = String::new();
        render_parse_error(
            &ParseError::Syntax(loaded.diagnostics),
            Some(&args.input),
            &mut rendered,
        )
        .expect("rendering parse diagnostics should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let mut result = resolve_file(&file);
    if !result.errors.is_empty() {
        let mut rendered = String::new();
        render_resolve_errors(&result.errors, &source, Some(&args.input), &mut rendered)
            .expect("rendering resolve errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let direction_errors = check_directions(&file, &result);
    if !direction_errors.is_empty() {
        let mut rendered = String::new();
        render_direction_errors(&direction_errors, &source, Some(&args.input), &mut rendered)
            .expect("rendering direction errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let hir = match hir::lower_to_hir(&file, &result) {
        Ok(h) => h,
        Err(errors) => {
            for (i, err) in errors.iter().enumerate() {
                if i > 0 {
                    eprintln!();
                }
                eprintln!(
                    "error: {} ({}:{}:{})",
                    err.kind,
                    args.input.display(),
                    err.span.start.row + 1,
                    err.span.start.column + 1,
                );
            }
            process::exit(1);
        }
    };

    let driver_errors = check_drivers(&hir);
    if !driver_errors.is_empty() {
        let mut rendered = String::new();
        render_driver_errors(&driver_errors, &source, Some(&args.input), &mut rendered)
            .expect("rendering driver errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let tc = typeck::check_file(&hir, &result);
    if !tc.errors.is_empty() {
        let mut rendered = String::new();
        typeck::render_type_errors(&tc.errors, &source, Some(&args.input), &mut rendered)
            .expect("rendering type errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let width_check = check_width_obligations(&tc.residual_obligations);
    if !width_check.errors.is_empty() {
        let mut rendered = String::new();
        typeck::render_type_errors(
            &width_check.errors,
            &source,
            Some(&args.input),
            &mut rendered,
        )
        .expect("rendering width errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }
    // TODO: thread `width_check.unresolved_widths` into the SV emitter so
    // they can be lowered to parameter-level arithmetic. Today's examples
    // produce none, so dropping them is a no-op; this matters once
    // parametric widths are in scope.
    let _ = width_check.unresolved_widths;
    let _ = width_check.unresolved_domain_kinds;

    // Monomorphise Type-kind generic fns: synthesise a specialised
    // module per concrete instantiation, leave Const/Domain generics
    // polymorphic. Original Type-kind-generic fns stay in the HIR but
    // are skipped by sv_lower (no SV construct matches a
    // type-polymorphic module).
    let mono = polar_compiler_old::hirtl::monomorphise::monomorphise(
        hir,
        tc.expr_types,
        tc.local_types,
        tc.method_resolutions,
        tc.fn_residuals,
        &tc.call_generics,
        &mut result,
    );
    let hir = mono.file;
    let mono_expr_types = mono.expr_types;
    let mono_local_types = mono.local_types;
    let mono_method_resolutions = mono.method_resolutions;
    let mono_fn_residuals = mono.fn_residuals;

    // Flatten block/if expressions into a result-local + statement-form
    // `if`. After this, no `HirExprKind::Block` / `HirExprKind::If`
    // remains in HIR; downstream passes only see `HirStmt::If`.
    let block_lowered =
        polar_compiler_old::lower_block_expressions(&hir, &mono_expr_types, &mono_local_types);
    let hir = block_lowered.file;
    let local_types = block_lowered.local_types;

    // Rewrite each `HirExprKind::MethodCall` into a regular `Call` against
    // the resolved method's `DefId`. After this pass no `MethodCall`
    // remains in HIR; downstream passes treat methods like user fns.
    let hir = polar_compiler_old::lower_method_calls(&hir, &result, &mono_method_resolutions);

    // Rewrite user-function calls into out-arg form so that, after flatten,
    // they sit at expression-statement position with binding leaves passed
    // as out-arguments. sv_lower then emits each as a single SV instance.
    let hir = match polar_compiler_old::desugar_user_calls(&hir) {
        Ok(h) => h,
        Err(errors) => {
            for (i, err) in errors.iter().enumerate() {
                if i > 0 {
                    eprintln!();
                }
                eprintln!(
                    "error: {} ({}:{}:{})",
                    err.kind,
                    args.input.display(),
                    err.span.start.row + 1,
                    err.span.start.column + 1,
                );
            }
            process::exit(1);
        }
    };

    // Only `--emit sv` reaches here; `--emit cst` returned early above.
    let flat = match flatten_aggregates(&hir, &result, &mono_expr_types, &local_types) {
        Ok(f) => f,
        Err(errors) => {
            let mut rendered = String::new();
            polar_compiler_old::render_flatten_errors(
                &errors,
                &source,
                Some(&args.input),
                &mut rendered,
            )
            .expect("rendering flatten errors should not fail");
            eprintln!("{rendered}");
            process::exit(1);
        }
    };
    let sv_file = lower_to_sv(&flat, &result, &mono_fn_residuals);
    let sv_text = match emit_sv(&sv_file) {
        Ok(s) => s,
        Err(errors) => {
            let mut rendered = String::new();
            render_emit_errors(&errors, &mut rendered)
                .expect("rendering emit errors should not fail");
            eprintln!("{rendered}");
            process::exit(1);
        }
    };
    if let Err(e) = write_sv_output(&args.input, &args.out_dir, &sv_text) {
        eprintln!("error: failed to write SystemVerilog output: {e}");
        process::exit(2);
    }
}

/// Parse just the root file and print its CST to stdout (the `--emit cst`
/// debug path). Multi-file loading does not apply here — there is no single
/// CST across files.
fn emit_root_cst(input: &Path) {
    let source = match fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", input.display());
            process::exit(2);
        }
    };
    match parse_source_with_diagnostics(&source) {
        Ok(parsed) => print!("{}", parsed.cst),
        Err(err) => {
            let mut rendered = String::new();
            render_parse_error(&err, Some(input), &mut rendered)
                .expect("rendering parse error should not fail");
            eprintln!("{rendered}");
            process::exit(1);
        }
    }
}

fn write_sv_output(input: &Path, out_dir: &Path, text: &str) -> std::io::Result<()> {
    fs::create_dir_all(out_dir)?;
    let stem = input
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("out"));
    let mut out_path = out_dir.join(stem);
    out_path.set_extension("sv");
    fs::write(out_path, text)
}
