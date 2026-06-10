//! Example-driven tests: run the query stack over the real `.plr` files in
//! `examples/working/` (the same corpus the old compiler checks).
//!
//! The new analogue of `polar-compiler`'s example tests — point the front end at
//! real source instead of inline strings. Two tests:
//!
//! - [`every_working_example_runs_the_query_stack`] — a robustness smoke test:
//!   every example lowers + infers without panicking (exercises the real grammar
//!   surface).
//! - [`clean_examples_typecheck_without_diagnostics`] — a ratchet: the examples
//!   that use only features the new front end already supports must produce zero
//!   diagnostics; the rest must still produce some. As deferred features land
//!   (named-arg/out-arg calls → Q5, parametric field substitution → Q4/Q5), a
//!   file flips from the second set to the first, and this test fails until it is
//!   promoted into `CLEAN` — keeping the supported surface honest.

use std::path::{Path, PathBuf};

use polar_compiler::{
    DefKind, RootDatabase, SourceRoot, Vfs, body, check_drivers, crate_def_map, directions, infer,
    sig_of, verilog,
};

fn working_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/working")
}

fn examples() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(working_dir()).expect("examples/working") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("plr") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            out.push((name, std::fs::read_to_string(&path).unwrap()));
        }
    }
    out.sort();
    out
}

/// Examples that type-check clean through the whole front-end stack (name
/// resolution, body lowering, inference incl. parametric instantiation, driver +
/// direction checks). The entire working corpus is now clean — a fully
/// functional type checker over it.
const CLEAN: &[&str] = &[
    "accumulator.plr",
    "add_constant.plr",
    "counter.plr",
    "delay.plr",
    "delay_impl.plr",
    "equal_width_fn.plr",
    "if_expression.plr",
    "inferred_dom_reg.plr",
    "lift_func.plr",
    "module_wrapped.plr",
    "mult_add.plr",
    "multi_call.plr",
    "packet_struct.plr",
    "parameterized_port.plr",
    "parametric_struct.plr",
    "parametric_struct_extended.plr",
    "parametric_width_fn.plr",
    "parametric_width_port.plr",
    "pipeline.plr",
    "pub_use_reexport.plr",
    "shift_register.plr",
    "simple_port.plr",
    "use_across_modules.plr",
    "when_counter.plr",
];

/// `(name-resolution, body, inference, driver, direction)` diagnostic counts.
fn diagnostic_counts(src: &str) -> (usize, usize, usize, usize, usize) {
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.plr", src.to_owned());
    let krate: SourceRoot = vfs.source_root(&mut db, "t.plr");
    let map = crate_def_map(&db, krate);

    let (mut body_d, mut infer_d, mut driver_d, mut dir_d) = (0, 0, 0, 0);
    for def in map.defs().collect::<Vec<_>>() {
        match map.def_data(def).map(|d| d.kind) {
            Some(DefKind::Fn | DefKind::Method) => {
                let _ = sig_of(&db, krate, def);
                body_d += body(&db, krate, def).diagnostics().len();
                infer_d += infer(&db, krate, def).diagnostics().len();
                driver_d += check_drivers(&db, krate, def).len();
                dir_d += directions(&db, krate, def).len();
            }
            Some(DefKind::Struct | DefKind::Port) => {
                let _ = sig_of(&db, krate, def);
            }
            _ => {}
        }
    }
    (map.diagnostics().len(), body_d, infer_d, driver_d, dir_d)
}

/// Dev aid: per-example diagnostic tally. `cargo test -p polar-compiler --test examples
/// report -- --ignored --nocapture`.
#[test]
#[ignore]
fn report() {
    for (name, src) in examples() {
        let (n, b, i, d, dir) = diagnostic_counts(&src);
        let tag = if n + b + i + d + dir == 0 {
            "CLEAN"
        } else {
            "----"
        };
        eprintln!("{tag} {name:<32} nameres={n} body={b} infer={i} drivers={d} dirs={dir}");
    }
}

/// Dev aid: dump the emitted SystemVerilog for every example. `cargo test -p
/// polar-compiler --test examples dump_verilog -- --ignored --nocapture`.
#[test]
#[ignore]
fn dump_verilog() {
    for (name, src) in examples() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.plr", src);
        let krate = vfs.source_root(&mut db, "t.plr");
        eprintln!("===== {name} =====\n{}", verilog(&db, krate));
    }
}

/// Examples whose emitted SystemVerilog is complete today and should lint clean.
/// The deferred Q5-mono pieces are excluded: `equal_width_fn` (needs the
/// width-obligation `initial assert`) and `parametric_struct_extended` (needs
/// type-kind fn monomorphisation). The parametric examples that *are* done carry
/// a `// verilator: -G…=N` directive (a parameter value for elaboration), which
/// this harness reads and forwards.
const VERILATOR_CLEAN: &[&str] = &[
    "accumulator.plr",
    "add_constant.plr",
    "counter.plr",
    "delay.plr",
    "delay_impl.plr",
    "equal_width_fn.plr",
    "if_expression.plr",
    "inferred_dom_reg.plr",
    "lift_func.plr",
    "module_wrapped.plr",
    "mult_add.plr",
    "multi_call.plr",
    "packet_struct.plr",
    "parameterized_port.plr",
    "parametric_struct.plr",
    "parametric_width_fn.plr",
    "parametric_width_port.plr",
    "pipeline.plr",
    "pub_use_reexport.plr",
    "shift_register.plr",
    "simple_port.plr",
    "use_across_modules.plr",
    "when_counter.plr",
];

/// The `-G…` parameter-value flags from an example's leading `// verilator: …`
/// directive (the `-Wno-…` tokens are already covered by the base flag set).
fn verilator_directive(src: &str) -> Vec<String> {
    src.lines()
        .find_map(|l| l.trim().strip_prefix("// verilator:"))
        .map(|rest| {
            rest.split_whitespace()
                .filter(|t| t.starts_with("-G"))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Emit SystemVerilog for the corpus and lint it with verilator. Skips (passes)
/// when verilator is not installed, so CI without it stays green — the
/// verification the project settled on (verilator lint over the new output).
#[test]
fn corpus_is_verilator_clean() {
    if std::process::Command::new("verilator")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("verilator not installed — skipping lint");
        return;
    }
    let dir = std::env::temp_dir().join("polar_compiler_verilator");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, src) in examples() {
        if !VERILATOR_CLEAN.contains(&name.as_str()) {
            continue;
        }
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.plr", src.clone());
        let krate = vfs.source_root(&mut db, "t.plr");
        let sv = verilog(&db, krate);
        let path = dir.join(name.replace(".plr", ".sv"));
        std::fs::write(&path, sv).unwrap();
        // `-Wall` minus the cosmetic/expected lints: filename≠module name,
        // intentionally-unused port-field signals, and multiple uninstantiated
        // top modules (test harnesses with several roots). Parameter values come
        // from the example's `// verilator: -G…` directive.
        let out = std::process::Command::new("verilator")
            .args([
                "--lint-only",
                "-Wall",
                "-Wno-DECLFILENAME",
                "-Wno-UNUSEDSIGNAL",
                "-Wno-MULTITOP",
            ])
            .args(verilator_directive(&src))
            .arg(&path)
            .output()
            .expect("run verilator");
        assert!(
            out.status.success(),
            "verilator rejected {name}:\n{}\n--- sv ---\n{}",
            String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(&path).unwrap_or_default(),
        );
    }
}

#[test]
fn every_working_example_runs_the_query_stack() {
    // No panic on any example == the smoke test passes.
    for (name, src) in examples() {
        let _ = diagnostic_counts(&src);
        eprintln!("ran: {name}");
    }
}

#[test]
fn clean_examples_typecheck_without_diagnostics() {
    for (name, src) in examples() {
        let counts = diagnostic_counts(&src);
        let total = counts.0 + counts.1 + counts.2 + counts.3 + counts.4;
        if CLEAN.contains(&name.as_str()) {
            assert_eq!(
                counts,
                (0, 0, 0, 0, 0),
                "{name} is listed CLEAN but produced diagnostics \
                 (nameres, body, infer, drivers, directions) = {counts:?}"
            );
        } else {
            assert!(
                total > 0,
                "{name} is no longer producing diagnostics — promote it into CLEAN"
            );
        }
    }
}
