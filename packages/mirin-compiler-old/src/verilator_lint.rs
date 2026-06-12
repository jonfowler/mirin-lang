//! Verilator-lint coverage for the emitter.
//!
//! For every `.mrn` file under `examples/working/`, we lower it through the
//! whole pipeline, write the resulting SystemVerilog to a temp file, and run
//! `verilator --lint-only -Wall` over it. The test fails loudly if any
//! example produces lint warnings or errors.
//!
//! If `verilator` isn't on `PATH`, the test panics — verilator is a required
//! development dependency, not an optional one. The point is to catch
//! regressions that string-contains tests on emitter output can't (port
//! direction mismatches, undeclared identifiers, width mismatches, missing
//! instance bindings).
//!
//! Two warning classes are suppressed because they're inherent to Mirin's
//! emission model rather than user-facing bugs:
//!
//! - `DECLFILENAME`: Mirin emits multiple modules per `.sv` file, so the
//!   filename can't match every module's name.
//! - `MULTITOP`: Mirin files often define more than one top-level module
//!   (e.g. several test entry points alongside their helpers).
//!
//! Per-example overrides — extra verilator arguments, warning suppressions,
//! parameter overrides — live as `// verilator: …` comment lines inside the
//! `.mrn` source. Tokens after the directive are appended to the verilator
//! invocation verbatim. Multiple directive lines may appear in one file; the
//! Mirin parser already strips comments, so the directives are inert from
//! the language's POV.

#![cfg(test)]

use std::path::PathBuf;
use std::process::Command;

use crate::hir::lower_to_hir;
use crate::hirt::typeck;
use crate::hirtl::flatten::flatten_aggregates;
use crate::resolve::resolve_file;
use crate::surface::ir::parse_surface_source;
use crate::svir::emit::emit;
use crate::svir::lower::lower_to_sv;
use crate::test_support::working_examples;

/// Pipeline an example all the way to SV text.
fn build_sv(src: &str) -> String {
    let surface = parse_surface_source(src).expect("parse");
    let mut resolve = resolve_file(&surface);
    let hir = lower_to_hir(&surface, &resolve).expect("lower");
    let tc = typeck::check_file(&hir, &resolve);
    let mono = crate::hirtl::monomorphise::monomorphise(
        hir,
        tc.expr_types,
        tc.local_types,
        tc.method_resolutions,
        tc.fn_residuals,
        &tc.call_generics,
        &mut resolve,
    );
    let hir = mono.file;
    let block_lowered = crate::hirtl::lower_block_expressions::lower_block_expressions(
        &hir,
        &mono.expr_types,
        &mono.local_types,
    );
    let hir = block_lowered.file;
    let local_types = block_lowered.local_types;
    let hir =
        crate::hirtl::method_lower::lower_method_calls(&hir, &resolve, &mono.method_resolutions);
    let hir = crate::hirtl::out_args::desugar_user_calls(&hir).expect("desugar");
    let flat = flatten_aggregates(&hir, &resolve, &mono.expr_types, &local_types).expect("flatten");
    let sv = lower_to_sv(&flat, &resolve, &mono.fn_residuals);
    emit(&sv).expect("emit")
}

/// Extract per-example verilator arguments from `// verilator: …` lines in
/// the raw `.mrn` source. Each token after the directive becomes one
/// command-line argument.
fn extra_args_from_source(source: &str) -> Vec<String> {
    let mut args = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("// verilator:") {
            for tok in rest.split_whitespace() {
                args.push(tok.to_owned());
            }
        }
    }
    args
}

/// Run verilator over `sv_text` (written to a temp file named after
/// `example_name`). Returns `Ok(())` on a clean lint, `Err(stderr)` on any
/// warning or error. Panics if verilator can't be invoked at all.
fn verilator_lint(sv_text: &str, example_name: &str, extra_args: &[String]) -> Result<(), String> {
    let mut path: PathBuf = std::env::temp_dir();
    path.push("mirin-verilator-lint");
    std::fs::create_dir_all(&path).expect("create lint tmp dir");
    path.push(format!("{example_name}.sv"));
    std::fs::write(&path, sv_text).expect("write lint tmp file");

    let output = Command::new("verilator")
        .arg("--lint-only")
        .arg("-Wall")
        // See module docs — these two are inherent to Mirin's emission model.
        .arg("-Wno-DECLFILENAME")
        .arg("-Wno-MULTITOP")
        .args(extra_args)
        .arg(&path)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "failed to invoke `verilator`: {e}\n\
                 verilator is a required development dependency for this crate.\n\
                 Install it (e.g. `nix-env -iA nixpkgs.verilator` or your package manager) and re-run."
            )
        });

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!(
            "verilator exited with {}:\n--- stderr ---\n{stderr}\n--- stdout ---\n{stdout}",
            output.status
        ))
    }
}

#[test]
fn working_examples_lint_clean_under_verilator() {
    let examples = working_examples();
    assert!(
        !examples.is_empty(),
        "expected at least one example under examples/working/"
    );

    let mut failures: Vec<(String, String)> = Vec::new();
    for (name, source) in examples {
        let extra = extra_args_from_source(&source);
        let sv = build_sv(&source);
        if let Err(report) = verilator_lint(&sv, &name, &extra) {
            failures.push((name, report));
        }
    }

    if !failures.is_empty() {
        let mut msg = format!("{} example(s) failed verilator lint:\n", failures.len());
        for (name, report) in &failures {
            msg.push_str(&format!("\n=== {name} ===\n{report}\n"));
        }
        panic!("{msg}");
    }
}
