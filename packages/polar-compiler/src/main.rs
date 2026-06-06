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
    DefKind, RootDatabase, SourceRoot, Vfs, body, check_drivers, crate_def_map, directions, infer,
    parse_text, render, sig_of, syntax_errors, verilog,
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

    let sv = verilog(&db, krate);
    if let Err(e) = write_sv(&args.input, &args.out_dir, sv) {
        eprintln!("error: failed to write SystemVerilog output: {e}");
        process::exit(2);
    }
}

/// Load the root file and, transitively, every `mod foo;` file it pulls in
/// (`dir/foo.plr`, children in `dir/foo/`), then build the crate's
/// [`SourceRoot`] over the loaded set.
fn load_crate(
    db: &mut RootDatabase,
    vfs: &mut Vfs,
    root_path: &Path,
) -> std::io::Result<SourceRoot> {
    let root_dir = root_path.parent().unwrap_or(Path::new(".")).to_owned();
    // Worklist of (file path, dir its own `mod foo;` files resolve in).
    let mut work = vec![(root_path.to_owned(), root_dir)];
    while let Some((path, dir)) = work.pop() {
        if vfs.file(&path).is_some() {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        vfs.set_file_text(db, path.clone(), text.clone());
        // Discover the file modules this file declares (at any nesting) and
        // queue the ones that exist on disk.
        let tree = parse_text(&text);
        let mut found = Vec::new();
        discover_file_mods(&tree.root_node(), &dir, &text, &mut found);
        for (mod_path, child_dir) in found {
            if mod_path.exists() {
                work.push((mod_path, child_dir));
            }
        }
    }
    Ok(vfs.source_root(db, root_path))
}

/// Walk a container node (the file root or an inline `mod` body) for module
/// declarations. An inline `mod m { … }` recurses into its body under `dir/m`;
/// a file `mod m;` yields `(dir/m.plr, dir/m)` to load.
fn discover_file_mods(
    container: &tree_sitter::Node,
    dir: &Path,
    source: &str,
    out: &mut Vec<(PathBuf, PathBuf)>,
) {
    let mut cursor = container.walk();
    for child in container.children(&mut cursor) {
        if child.kind() != "module_definition" {
            continue;
        }
        let Some(name) = child
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        else {
            continue;
        };
        let child_dir = dir.join(name);
        match child.child_by_field_name("body") {
            Some(body) => discover_file_mods(&body, &child_dir, source, out),
            None => out.push((dir.join(format!("{name}.plr")), child_dir)),
        }
    }
}

/// Run the front-end query stack over every def and collect its diagnostics as
/// rendered lines. (Spans arrive with the Q6 diagnostics infra; until then the
/// CLI prints each diagnostic's structured form.)
fn collect_diagnostics(db: &RootDatabase, krate: SourceRoot) -> Vec<String> {
    let map = crate_def_map(db, krate);
    let mut out: Vec<String> = map
        .diagnostics()
        .iter()
        .map(|d| format!("error: {d:?}"))
        .collect();
    for def in map.defs().collect::<Vec<_>>() {
        match map.def_data(def).map(|d| d.kind) {
            Some(DefKind::Fn | DefKind::Method) => {
                let _ = sig_of(db, krate, def);
                for d in body(db, krate, def).diagnostics() {
                    out.push(format!("error: {d:?}"));
                }
                for d in infer(db, krate, def).diagnostics() {
                    out.push(format!("error: {d:?}"));
                }
                for d in check_drivers(db, krate, def) {
                    out.push(format!("error: {d:?}"));
                }
                for d in directions(db, krate, def) {
                    out.push(format!("error: {d:?}"));
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
