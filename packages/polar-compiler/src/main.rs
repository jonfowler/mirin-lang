use std::path::{Path, PathBuf};
use std::{env, fs, process};

use polar_compiler::{
    ParseError, check_directions, check_drivers, check_width_obligations, emit_sv,
    flatten_aggregates, hir, lower_cst, lower_to_sv, parse_source_with_diagnostics,
    render_direction_errors, render_driver_errors, render_emit_errors, render_parse_error,
    render_resolve_errors, resolve_file, typeck,
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

    let source = match fs::read_to_string(&args.input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", args.input.display());
            process::exit(2);
        }
    };

    let parsed = match parse_source_with_diagnostics(&source) {
        Ok(p) => p,
        Err(err) => {
            let mut rendered = String::new();
            render_parse_error(&err, Some(&args.input), &mut rendered)
                .expect("rendering parse error should not fail");
            eprintln!("{rendered}");
            process::exit(1);
        }
    };

    if !parsed.diagnostics.is_empty() {
        let mut rendered = String::new();
        render_parse_error(
            &ParseError::Syntax(parsed.diagnostics),
            Some(&args.input),
            &mut rendered,
        )
        .expect("rendering parse diagnostics should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let file = match lower_cst(&parsed.cst, &source) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let result = resolve_file(&file);
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

    match args.emit {
        EmitMode::Cst => {
            print!("{}", parsed.cst);
        }
        EmitMode::Sv => {
            let flat = match flatten_aggregates(&hir, &tc.expr_types) {
                Ok(f) => f,
                Err(errors) => {
                    let mut rendered = String::new();
                    polar_compiler::render_flatten_errors(
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
            let sv_file = lower_to_sv(&flat, &result);
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
