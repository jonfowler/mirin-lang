//! `polar-compiler` CLI — compile a `.plr` file (and the `mod foo;` files it
//! pulls in) to SystemVerilog through the query stack. The batch driver fills
//! the [`Vfs`] from disk once, builds the crate's [`SourceRoot`], reports any
//! front-end diagnostics, and writes `verilog(crate)` to `<out>/<stem>.sv`.
//!
//! The query-based compiler reached corpus parity with the original at Q5-mono
//! and became the primary `polar-compiler`; the original lives on as
//! `polar-compiler-old`, a parity oracle.

use std::path::{Path, PathBuf};
use std::{env, fs, process};

use polar_compiler::{
    DefKind, RootDatabase, SourceRoot, Span, Vfs, ast_id_map, body, check_drivers, crate_def_map,
    completeness, directions, infer, load_crate, parse_text, render, reserved_words, sig_of,
    syntax_errors, verilog,
};

struct CliArgs {
    input: PathBuf,
    out_dir: PathBuf,
    emit_cst: bool,
}

fn print_usage(program: &str) {
    eprintln!("usage: {program} [--emit sv|cst] [--out <dir>] <path-to-.plr-file>");
    eprintln!();
    eprintln!("  --emit sv   (default) write SystemVerilog to <out-dir>/<stem>.sv");
    eprintln!("  --emit cst  print the root file's concrete syntax tree to stdout");
    eprintln!("  --out <dir> output directory for `--emit sv` (default: ./sv/)");
}

fn parse_args(program: &str) -> Result<CliArgs, i32> {
    let mut args = env::args_os().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut out_dir = PathBuf::from("./sv/");
    let mut emit_cst = false;

    while let Some(raw) = args.next() {
        let Some(s) = raw.to_str().map(str::to_owned) else {
            eprintln!("error: non-UTF8 argument");
            return Err(2);
        };
        match s.as_str() {
            "--emit" => {
                let value = args.next().ok_or_else(|| {
                    eprintln!("error: `--emit` expects a value (sv or cst)");
                    2
                })?;
                match value.to_string_lossy().as_ref() {
                    "sv" => emit_cst = false,
                    "cst" => emit_cst = true,
                    other => {
                        eprintln!("error: unknown emit mode `{other}` (expected sv or cst)");
                        return Err(2);
                    }
                }
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
        out_dir,
        emit_cst,
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

    // `--emit cst`: parse just the root file and print its tree (a debug aid).
    if args.emit_cst {
        match fs::read_to_string(&args.input) {
            Ok(src) => println!("{}", parse_text(&src).root_node().to_sexp()),
            Err(e) => {
                eprintln!("error: failed to read {}: {e}", args.input.display());
                process::exit(2);
            }
        }
        return;
    }

    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    let krate = match load_crate(&mut db, &mut vfs, &args.input) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", args.input.display());
            process::exit(2);
        }
    };

    // Syntax errors short-circuit before semantic analysis (a parse-recovered
    // tree would otherwise lower to partial, wrong output).
    let mut syntax = Vec::new();
    for &file in krate.files(&db) {
        let path = file.path(&db).to_string_lossy().into_owned();
        let source = file.text(&db);
        for e in syntax_errors(&db, file) {
            syntax.push(render(&path, source, e.span, &e.message));
        }
    }
    if !syntax.is_empty() {
        eprintln!("{}", syntax.join("\n\n"));
        process::exit(1);
    }

    let diagnostics = collect_diagnostics(&db, krate);
    if !diagnostics.is_empty() {
        for d in &diagnostics {
            eprintln!("{d}");
        }
        process::exit(1);
    }

    // Reserved-word collisions in the emitted SV are a hard error (the output
    // would be invalid SystemVerilog otherwise).
    let reserved = reserved_words(&db, krate);
    if !reserved.is_empty() {
        for r in reserved {
            eprintln!("error: {r}");
        }
        process::exit(1);
    }

    let sv = verilog(&db, krate);
    if let Err(e) = write_sv(&args.input, &args.out_dir, sv) {
        eprintln!("error: failed to write SystemVerilog output: {e}");
        process::exit(2);
    }
}

/// Run the front-end query stack over every def and collect its diagnostics as
/// rendered lines. Body diagnostics carry def-relative spans, resolved here to
/// an absolute source location; the rest still print structurally (their spans
/// land in later slices).
fn collect_diagnostics(db: &RootDatabase, krate: SourceRoot) -> Vec<String> {
    let map = crate_def_map(db, krate);
    let mut out: Vec<String> = Vec::new();
    // Crate-level (name resolution): render with the item's anchor when present.
    for d in map.diagnostics() {
        match d.anchor {
            Some((file, ast_id)) => {
                let path = file.path(db).to_string_lossy().into_owned();
                let source = file.text(db);
                let span = ast_id_map(db, file)
                    .range_of(ast_id)
                    .map(|(s, e)| Span {
                        start: s as u32,
                        end: e as u32,
                    })
                    .unwrap_or_default();
                out.push(render(&path, source, span, &d.message()));
            }
            None => out.push(format!("error: {}", d.message())),
        }
    }
    for def in map.defs().collect::<Vec<_>>() {
        match map.def_data(def).map(|d| d.kind) {
            Some(DefKind::Fn | DefKind::Method) => {
                // The def's absolute start, to lift def-relative body spans.
                let file = def.file(db);
                let path = file.path(db).to_string_lossy().into_owned();
                let source = file.text(db);
                let def_start = ast_id_map(db, file)
                    .range_of(def.ast_id(db))
                    .map(|(s, _)| s as u32)
                    .unwrap_or(0);
                let abs = |s: Span| Span {
                    start: def_start + s.start,
                    end: def_start + s.end,
                };
                for d in &sig_of(db, krate, def).diagnostics {
                    out.push(render(&path, source, abs(d.span), &d.message()));
                }
                for d in body(db, krate, def).diagnostics() {
                    out.push(render(&path, source, abs(d.span), &d.message()));
                }
                for d in completeness(db, krate, def) {
                    out.push(render(&path, source, abs(d.span), &d.message()));
                }
                for d in check_drivers(db, krate, def) {
                    out.push(render(&path, source, abs(d.span), &d.message()));
                }
                for d in directions(db, krate, def) {
                    out.push(render(&path, source, abs(d.span), &d.message()));
                }
                for d in infer(db, krate, def).diagnostics() {
                    out.push(render(&path, source, abs(d.span), &d.message()));
                }
            }
            Some(DefKind::Struct | DefKind::Port) => {
                let _ = sig_of(db, krate, def);
            }
            _ => {}
        }
    }
    out
}

fn write_sv(input: &Path, out_dir: &Path, text: &str) -> std::io::Result<()> {
    fs::create_dir_all(out_dir)?;
    let stem = input
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("out"));
    let mut out_path = out_dir.join(stem);
    out_path.set_extension("sv");
    fs::write(out_path, text)
}
