use std::path::PathBuf;
use std::{env, fs, process};

use polar_compiler::{
    ParseError, check_directions, check_drivers, discharge_width_obligations, hir, lower_cst,
    parse_source_with_diagnostics, render_direction_errors, render_driver_errors,
    render_parse_error, render_resolve_errors, resolve_file, typeck,
};

fn main() {
    let mut args = env::args_os();
    let program = args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .unwrap_or_else(|| "polar-compiler".to_owned());

    let Some(path) = args.next() else {
        eprintln!("usage: {program} <path-to-.plr-file>");
        process::exit(2);
    };

    if args.next().is_some() {
        eprintln!("usage: {program} <path-to-.plr-file>");
        process::exit(2);
    }

    let path = PathBuf::from(path);

    let source = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", path.display());
            process::exit(2);
        }
    };

    let parsed = match parse_source_with_diagnostics(&source) {
        Ok(p) => p,
        Err(err) => {
            let mut rendered = String::new();
            render_parse_error(&err, Some(&path), &mut rendered)
                .expect("rendering parse error should not fail");
            eprintln!("{rendered}");
            process::exit(1);
        }
    };

    if !parsed.diagnostics.is_empty() {
        let mut rendered = String::new();
        render_parse_error(
            &ParseError::Syntax(parsed.diagnostics),
            Some(&path),
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
        render_resolve_errors(&result.errors, &source, Some(&path), &mut rendered)
            .expect("rendering resolve errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let direction_errors = check_directions(&file, &result);
    if !direction_errors.is_empty() {
        let mut rendered = String::new();
        render_direction_errors(&direction_errors, &source, Some(&path), &mut rendered)
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
                    path.display(),
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
        render_driver_errors(&driver_errors, &source, Some(&path), &mut rendered)
            .expect("rendering driver errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let tc = typeck::check_file(&hir, &result);
    if !tc.errors.is_empty() {
        let mut rendered = String::new();
        typeck::render_type_errors(&tc.errors, &source, Some(&path), &mut rendered)
            .expect("rendering type errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    let width_check = discharge_width_obligations(&tc.residual_obligations);
    if !width_check.errors.is_empty() {
        let mut rendered = String::new();
        typeck::render_type_errors(&width_check.errors, &source, Some(&path), &mut rendered)
            .expect("rendering width errors should not fail");
        eprintln!("{rendered}");
        process::exit(1);
    }

    print!("{}", parsed.cst);
}
