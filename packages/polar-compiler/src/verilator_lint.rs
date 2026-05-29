//! Verilator-lint coverage for the emitter.
//!
//! For every `.plr` file under `examples/working/`, we lower it through the
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
//! Two warning classes are suppressed because they're inherent to Polar's
//! emission model rather than user-facing bugs:
//!
//! - `DECLFILENAME`: Polar emits multiple modules per `.sv` file, so the
//!   filename can't match every module's name.
//! - `MULTITOP`: Polar files often define more than one top-level module
//!   (e.g. several test entry points alongside their helpers).
//!
//! Per-example overrides — extra verilator arguments, warning suppressions,
//! parameter overrides — live as `// verilator: …` comment lines inside the
//! `.plr` source. Tokens after the directive are appended to the verilator
//! invocation verbatim. Multiple directive lines may appear in one file; the
//! Polar parser already strips comments, so the directives are inert from
//! the language's POV.

#![cfg(test)]

use std::path::PathBuf;
use std::process::Command;

use crate::hir::{flatten_aggregates, lower_to_hir};
use crate::resolve::resolve_file;
use crate::surface_ir::parse_surface_source;
use crate::sv_emit::emit;
use crate::sv_lower::lower_to_sv;
use crate::test_support::working_examples;
use crate::typeck;

/// Pipeline an example all the way to SV text.
fn build_sv(src: &str) -> String {
    let surface = parse_surface_source(src).expect("parse");
    let resolve = resolve_file(&surface);
    let hir = lower_to_hir(&surface, &resolve).expect("lower");
    let tc = typeck::check_file(&hir, &resolve);
    let block_lowered = crate::hir::lower_block_expressions::lower_block_expressions(
        &hir,
        &tc.expr_types,
        &tc.local_types,
    );
    let hir = block_lowered.file;
    let local_types = block_lowered.local_types;
    let hir = crate::hir::lower_method_calls(&hir, &resolve, &tc.method_resolutions);
    let hir = crate::hir::desugar_user_calls(&hir).expect("desugar");
    let flat = flatten_aggregates(&hir, &tc.expr_types, &local_types).expect("flatten");
    let sv = lower_to_sv(&flat, &resolve);
    emit(&sv).expect("emit")
}

/// Extract per-example verilator arguments from `// verilator: …` lines in
/// the raw `.plr` source. Each token after the directive becomes one
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
    path.push("polar-verilator-lint");
    std::fs::create_dir_all(&path).expect("create lint tmp dir");
    path.push(format!("{example_name}.sv"));
    std::fs::write(&path, sv_text).expect("write lint tmp file");

    let output = Command::new("verilator")
        .arg("--lint-only")
        .arg("-Wall")
        // See module docs — these two are inherent to Polar's emission model.
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
