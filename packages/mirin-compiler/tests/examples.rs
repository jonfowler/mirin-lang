//! Example-driven tests: run the query stack over the real `.mrn` files in
//! `examples/working/` (the same corpus the old compiler checks).
//!
//! The new analogue of `mirin-compiler`'s example tests — point the front end at
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

use mirin_compiler::{
    DefKind, RootDatabase, SourceRoot, Vfs, body, check_drivers, completeness, crate_def_map,
    directions, infer, load_crate, mir_of, mono_check, pretty_mir, reserved_words, sig_of,
    syntax_errors, verilog,
};

/// Count of monomorphisation-time (ground-residual) diagnostics for a source.
fn mono_diag_count(src: &str) -> usize {
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", src.to_owned());
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    mono_check(&db, krate).len()
}

fn working_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/working")
}

fn fail_expected_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/fail-expected")
}

fn examples() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(working_dir()).expect("examples/working") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("mrn") {
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
    "inline_attr.mrn",
    "pack.mrn",
    "unpack.mrn",
    "vec_bitpack.mrn",
    "tuple_bitpack.mrn",
    "const_if.mrn",
    "slice_bits.mrn",
    "slice_vec.mrn",
    "slice_offset.mrn",
    "slice_elide.mrn",
    "slice_set.mrn",
    "slice_vec_set.mrn",
    "slice_const_expr.mrn",
    "slice_param.mrn",
    "assoc_const_value.mrn",
    "resize.mrn",
    "ram.mrn",
    "ram_write.mrn",
    "vec_elem_domain.mrn",
    "dataflow_stage.mrn",
    "df_example.mrn",
    "df_example_poly.mrn",
    "return_place.mrn",
    "struct_pattern.mrn",
    "named_result.mrn",
    "tuples.mrn",
    "tuple_register.mrn",
    "vec_of_tuples.mrn",
    "tuple_multi_domain.mrn",
    "range_and_index_set.mrn",
    "for_loops.mrn",
    "for_instances.mrn",
    "vectors.mrn",
    "vec_repeat.mrn",
    "bits_type.mrn",
    "signed.mrn",
    "typed_literal.mrn",
    "literal_inference.mrn",
    "operators.mrn",
    "comparison_ops.mrn",
    "comparison_const.mrn",
    "div_mod.mrn",
    "div_mod_const.mrn",
    "shift_ops.mrn",
    "bitwise_ops.mrn",
    "trait_assoc_const.mrn",
    "trait_generic.mrn",
    "trait_bounded_impl.mrn",
    "trait_concrete.mrn",
    "impl_parametric_owner.mrn",
    "impl_multi_method.mrn",
    "param_instance.mrn",
    "inline_verilog.mrn",
    "record_out_conn.mrn",
    "stream_connect.mrn",
    "const_arith.mrn",
    "const_fn_if.mrn",
    "const_fn_localparam.mrn",
    "const_fn_module_param.mrn",
    "const_fn_loop_bound.mrn",
    "const_fn_value_use.mrn",
    "const_fn_chained.mrn",
    "const_fn_reg_vec.mrn",
    "adder_tree.mrn",
    "const_domain_annotation.mrn",
    "const_out_params.mrn",
    "const_param_value.mrn",
    "fold_sum.mrn",
    "const_record_config.mrn",
    "accumulator.mrn",
    "add_constant.mrn",
    "const_then_clocked.mrn",
    "counter.mrn",
    "delay.mrn",
    "delay_impl.mrn",
    "dual_clock_lift.mrn",
    "equal_width_fn.mrn",
    "if_expression.mrn",
    "inferred_dom_reg.mrn",
    "field_drivers.mrn",
    "lift_func.mrn",
    "module_wrapped.mrn",
    "mult_add.mrn",
    "multi_call.mrn",
    "packet_struct.mrn",
    "parameterized_port.mrn",
    "parametric_struct.mrn",
    "parametric_struct_extended.mrn",
    "struct_mixed_domains.mrn",
    "struct_mixed_return.mrn",
    "parametric_struct_domain.mrn",
    "reg_const_input.mrn",
    "struct_two_clocks.mrn",
    "parametric_width_fn.mrn",
    "parametric_width_port.mrn",
    "pipeline.mrn",
    "pub_use_reexport.mrn",
    "shift_register.mrn",
    "simple_port.mrn",
    "use_across_modules.mrn",
    "when_counter.mrn",
];

/// `(name-resolution, sig, body, inference, driver, direction)` diagnostic counts.
fn diagnostic_counts(src: &str) -> (usize, usize, usize, usize, usize, usize) {
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", src.to_owned());
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    crate_diagnostic_counts(&db, krate)
}

/// The same counts over an already-loaded (possibly multi-file) crate.
fn crate_diagnostic_counts(
    db: &RootDatabase,
    krate: SourceRoot,
) -> (usize, usize, usize, usize, usize, usize) {
    let map = crate_def_map(db, krate);

    let (mut sig_d, mut body_d, mut infer_d, mut driver_d, mut dir_d) = (0, 0, 0, 0, 0);
    for def in map.defs().collect::<Vec<_>>() {
        match map.def_data(def).map(|d| d.kind) {
            Some(DefKind::Fn | DefKind::Method) => {
                sig_d += sig_of(db, krate, def).diagnostics.len();
                body_d += body(db, krate, def).diagnostics().len();
                infer_d += infer(db, krate, def).diagnostics().len();
                driver_d += check_drivers(db, krate, def).len();
                driver_d += completeness(db, krate, def).len();
                dir_d += directions(db, krate, def).len();
            }
            // Struct/port/impl HEADERS carry only signature diagnostics (an
            // impl header has no body) — e.g. a generic owner written un-applied.
            Some(DefKind::Struct | DefKind::Port | DefKind::Impl) => {
                sig_d += sig_of(db, krate, def).diagnostics.len();
            }
            _ => {}
        }
    }
    (
        map.diagnostics().len(),
        sig_d,
        body_d,
        infer_d,
        driver_d,
        dir_d,
    )
}

/// Dev aid: per-example diagnostic tally. `cargo test -p mirin-compiler --test examples
/// report -- --ignored --nocapture`.
#[test]
#[ignore]
fn report() {
    for (name, src) in examples() {
        let (n, sg, b, i, d, dir) = diagnostic_counts(&src);
        let tag = if n + sg + b + i + d + dir == 0 {
            "CLEAN"
        } else {
            "----"
        };
        eprintln!(
            "{tag} {name:<32} nameres={n} sig={sg} body={b} infer={i} drivers={d} dirs={dir}"
        );
    }
}

/// Dev aid: dump the emitted SystemVerilog for every example. `cargo test -p
/// mirin-compiler --test examples dump_verilog -- --ignored --nocapture`.
#[test]
#[ignore]
fn dump_verilog() {
    for (name, src) in examples() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src);
        let krate = vfs.source_root(&mut db, "t.mrn");
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
    "inline_attr.mrn",
    "pack.mrn",
    "unpack.mrn",
    "vec_bitpack.mrn",
    "tuple_bitpack.mrn",
    "const_if.mrn",
    "slice_bits.mrn",
    "slice_vec.mrn",
    "slice_offset.mrn",
    "slice_elide.mrn",
    "slice_set.mrn",
    "slice_vec_set.mrn",
    "slice_const_expr.mrn",
    "slice_param.mrn",
    "resize.mrn",
    "ram.mrn",
    "ram_write.mrn",
    "vec_elem_domain.mrn",
    "dataflow_stage.mrn",
    "df_example.mrn",
    "df_example_poly.mrn",
    "return_place.mrn",
    "struct_pattern.mrn",
    "named_result.mrn",
    "tuples.mrn",
    "tuple_register.mrn",
    "vec_of_tuples.mrn",
    "tuple_multi_domain.mrn",
    "range_and_index_set.mrn",
    "for_loops.mrn",
    "for_instances.mrn",
    "vectors.mrn",
    "vec_repeat.mrn",
    "bits_type.mrn",
    "signed.mrn",
    "typed_literal.mrn",
    "literal_inference.mrn",
    "operators.mrn",
    "comparison_ops.mrn",
    "comparison_const.mrn",
    "div_mod.mrn",
    "div_mod_const.mrn",
    "shift_ops.mrn",
    "bitwise_ops.mrn",
    "trait_assoc_const.mrn",
    "trait_generic.mrn",
    "trait_bounded_impl.mrn",
    "trait_concrete.mrn",
    "impl_multi_method.mrn",
    "impl_parametric_owner.mrn",
    "param_instance.mrn",
    "inline_verilog.mrn",
    "record_out_conn.mrn",
    "stream_connect.mrn",
    "const_arith.mrn",
    "const_fn_if.mrn",
    "const_fn_localparam.mrn",
    "const_fn_module_param.mrn",
    "const_fn_loop_bound.mrn",
    "const_fn_value_use.mrn",
    "const_fn_chained.mrn",
    "const_fn_reg_vec.mrn",
    "adder_tree.mrn",
    "const_domain_annotation.mrn",
    "const_out_params.mrn",
    "const_param_value.mrn",
    "fold_sum.mrn",
    "const_record_config.mrn",
    "accumulator.mrn",
    "add_constant.mrn",
    "const_then_clocked.mrn",
    "counter.mrn",
    "delay.mrn",
    "delay_impl.mrn",
    "dual_clock_lift.mrn",
    "equal_width_fn.mrn",
    "if_expression.mrn",
    "inferred_dom_reg.mrn",
    "field_drivers.mrn",
    "lift_func.mrn",
    "module_wrapped.mrn",
    "mult_add.mrn",
    "multi_call.mrn",
    "packet_struct.mrn",
    "parameterized_port.mrn",
    "parametric_struct.mrn",
    "parametric_width_fn.mrn",
    "reg_const_input.mrn",
    "struct_two_clocks.mrn",
    "parametric_width_port.mrn",
    "pipeline.mrn",
    "pub_use_reexport.mrn",
    "shift_register.mrn",
    "simple_port.mrn",
    "use_across_modules.mrn",
    "when_counter.mrn",
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
/// Multi-file projects: each `examples/working/projects/<name>/main.mrn`
/// is a crate root loaded with the real file-module loader.
fn projects() -> Vec<(String, PathBuf)> {
    let dir = working_dir().join("projects");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        let main = path.join("main.mrn");
        if main.exists() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            out.push((name, main));
        }
    }
    out.sort();
    out
}

/// Every project loads, resolves, and type-checks clean across all its
/// files, and emits non-empty SystemVerilog.
#[test]
fn projects_typecheck_clean() {
    let projects = projects();
    assert!(!projects.is_empty(), "no projects found");
    for (name, main) in projects {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load_crate(&mut db, &mut vfs, &main)
            .unwrap_or_else(|e| panic!("{name}: load failed: {e}"));
        let counts = crate_diagnostic_counts(&db, krate);
        assert_eq!(
            counts,
            (0, 0, 0, 0, 0, 0),
            "project {name} produced diagnostics \
             (nameres, sig, body, infer, drivers, directions) = {counts:?}"
        );
        let sv = verilog(&db, krate);
        assert!(!sv.is_empty(), "project {name} emitted no SV");
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Byte-for-byte SystemVerilog parity gate. Emits SV for the VERILATOR_CLEAN
/// corpus + projects and compares against committed snapshots under
/// `tests/golden/`. This is the gate the MIR emission retarget
/// (`planning/mir.md` S3) must reproduce exactly: lint-clean alone (the
/// `corpus_is_verilator_clean` test) would pass a miscompile — wrong width,
/// swapped leaf, mis-resolved trait instance — silently, because the output is
/// never compared, only linted.
///
/// Regenerate after an *intended* emission change (review the diff!):
///   MIRIN_UPDATE_GOLDEN=1 cargo test -p mirin-compiler --test examples golden_sv
#[test]
fn golden_sv_snapshot() {
    let update = std::env::var_os("MIRIN_UPDATE_GOLDEN").is_some();
    let dir = golden_dir();
    if update {
        std::fs::create_dir_all(&dir).unwrap();
    }

    // (name, emitted SV) for every gated case: single-file VERILATOR_CLEAN
    // examples plus the multi-file projects.
    let mut cases: Vec<(String, String)> = Vec::new();
    for (name, src) in examples() {
        if VERILATOR_CLEAN.contains(&name.as_str()) {
            let mut db = RootDatabase::default();
            let mut vfs = Vfs::new();
            vfs.set_file_text(&mut db, "t.mrn", src);
            let krate = vfs.source_root(&mut db, "t.mrn");
            cases.push((
                name.trim_end_matches(".mrn").to_owned(),
                verilog(&db, krate).to_string(),
            ));
        }
    }
    for (name, main) in projects() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load_crate(&mut db, &mut vfs, &main).unwrap();
        cases.push((format!("project_{name}"), verilog(&db, krate).to_string()));
    }

    assert!(!cases.is_empty(), "no golden cases found");
    let mut mismatches = Vec::new();
    for (name, sv) in cases {
        let path = dir.join(format!("{name}.sv"));
        if update {
            std::fs::write(&path, &sv).unwrap();
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(want) if want == sv => {}
            Ok(_) => mismatches.push(name),
            Err(_) => panic!(
                "missing golden {path:?} — run `MIRIN_UPDATE_GOLDEN=1 cargo test \
                 -p mirin-compiler --test examples golden_sv` to create it"
            ),
        }
    }
    assert!(
        mismatches.is_empty(),
        "golden SV mismatch (run MIRIN_UPDATE_GOLDEN=1 to update, then review the diff): {mismatches:?}"
    );
}

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
    let dir = std::env::temp_dir().join("mirin_compiler_verilator");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, src) in examples() {
        if !VERILATOR_CLEAN.contains(&name.as_str()) {
            continue;
        }
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src.clone());
        let krate = vfs.source_root(&mut db, "t.mrn");
        let sv = verilog(&db, krate);
        lint_sv(&dir, &name, &src, sv);
    }
    // Multi-file projects lint too.
    for (name, main) in projects() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load_crate(&mut db, &mut vfs, &main).unwrap();
        let src = std::fs::read_to_string(&main).unwrap();
        let sv = verilog(&db, krate);
        lint_sv(&dir, &name, &src, sv);
    }
}

/// Write one emitted SV file and lint it: `-Wall` minus the cosmetic lints
/// (filename≠module name, intentionally-unused port-field signals, multiple
/// uninstantiated top modules). Parameter values come from the example's
/// `// verilator: -G…` directive.
fn lint_sv(dir: &Path, name: &str, src: &str, sv: &str) {
    let path = dir.join(format!("{}.sv", name.trim_end_matches(".mrn")));
    std::fs::write(&path, sv).unwrap();
    let out = std::process::Command::new("verilator")
        .args([
            "--lint-only",
            "-Wall",
            "-Wno-DECLFILENAME",
            "-Wno-UNUSEDSIGNAL",
            "-Wno-MULTITOP",
        ])
        .args(verilator_directive(src))
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

/// Every `fail-expected/` example must produce at least one failure signal:
/// a syntax error, a front-end diagnostic, or an SV reserved-word collision.
/// The inverse ratchet of `CLEAN` — when a checker regresses and one of these
/// starts passing, this fails.
#[test]
fn fail_expected_examples_produce_diagnostics() {
    for entry in std::fs::read_dir(fail_expected_dir()).expect("examples/fail-expected") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("mrn") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&path).unwrap();

        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src.clone());
        let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
        let file = vfs.file("t.mrn").unwrap();
        let syntax = syntax_errors(&db, file).len();
        let reserved = reserved_words(&db, krate).len();
        let counts = diagnostic_counts(&src);
        let total = syntax
            + reserved
            + counts.0
            + counts.1
            + counts.2
            + counts.3
            + counts.4
            + counts.5
            + mono_diag_count(&src);
        assert!(
            total > 0,
            "{name} is in fail-expected but produced no diagnostics"
        );
    }
}

/// `mono_check` decides a ground residual at the call site: a literal-arg call
/// that makes two width params unequal is a compile-time diagnostic; the same
/// shape called with equal widths is clean (the residual grounds true).
#[test]
fn mono_check_decides_ground_residuals() {
    const CALLEE: &str = "fn add_mismatch {const n: integer, const m: integer} \
        (a: uint(n), b: uint(m)) -> uint(n) { a + b }\n";

    let bad = format!(
        "{CALLEE}fn use_bad (a: uint(8), b: uint(4)) -> uint(8) {{ add_mismatch(a, b) }}\n"
    );
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", bad);
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    let diags = mono_check(&db, krate);
    assert_eq!(diags.len(), 1, "expected one ground-mismatch diagnostic");
    let msg = diags[0].message();
    assert!(
        msg.contains("add_mismatch") && (msg.contains("8 != 4") || msg.contains("4 != 8")),
        "unexpected message: {msg}"
    );

    // Equal widths: the residual grounds true, no diagnostic.
    let good =
        format!("{CALLEE}fn use_ok (a: uint(8), b: uint(8)) -> uint(8) {{ add_mismatch(a, b) }}\n");
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", good);
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    assert!(
        mono_check(&db, krate).is_empty(),
        "equal-width instantiation should not diagnose"
    );

    // Still-symbolic instantiation (a passthrough param) stays deferred — the
    // ground check must not fire on a non-literal arg.
    let sym = format!(
        "{CALLEE}fn wrap {{const k: integer}} (a: uint(k), b: uint(k)) -> uint(k) \
         {{ add_mismatch(a, b) }}\n"
    );
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", sym);
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    assert!(
        mono_check(&db, krate).is_empty(),
        "symbolic instantiation should defer, not diagnose"
    );
}

/// `mono_check`'s literal-fit check is sign-aware: `128` fits `uint(8)` but not
/// `sint(8)` (max 127). The earlier unsigned-only bound (`value >= 2^w`) missed
/// the signed-overflow case.
#[test]
fn mono_check_fit_is_sign_aware() {
    let src = "fn f {const n: integer} (x: sint(n)) -> sint(n) { sint(n)::128 }\n\
        fn use_bad (x: sint(8)) -> sint(8) { f(x) }\n";
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", src);
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    let diags = mono_check(&db, krate);
    assert!(
        diags
            .iter()
            .any(|d| d.message().contains("sint(8)") && d.message().contains("128")),
        "expected a sign-aware fit diagnostic: {:?}",
        diags.iter().map(|d| d.message()).collect::<Vec<_>>()
    );
}

/// `mono_check` composes one level: a bad width in an inner callee's signature
/// (`inner: uint(k - 10)`), invisible in the wrapper's own signature
/// (`wrap: uint(k) -> uint(k)`), is caught by substituting the wrapper's
/// instantiation (k=4) into its call to the inner callee → `uint(-6)`.
#[test]
fn mono_check_composes_one_level() {
    let src = "fn inner {dom clk: Clock, const k: integer} (x: uint(k) @clk) \
        -> uint(k - 10) @clk { x.resize() }\n\
        fn wrap {dom clk: Clock, const k: integer} (x: uint(k) @clk) -> uint(k) @clk \
        { let tmp = inner(x); x.resize() }\n\
        fn use_bad {dom clk: Clock} (x: uint(4) @clk) -> uint(4) @clk { wrap(x) }\n";
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", src);
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    let diags = mono_check(&db, krate);
    assert!(
        diags
            .iter()
            .any(|d| d.message().contains("inner") && d.message().contains("-6")),
        "expected a depth-1 composed diagnostic: {:?}",
        diags.iter().map(|d| d.message()).collect::<Vec<_>>()
    );
}

/// `mono_check` catches a parametric width that grounds non-positive at a
/// literal instantiation — a `uint(n - m)` return with n=4, m=8 → width -4,
/// which infer leaves symbolic (decided only at the call).
#[test]
fn mono_check_catches_ground_negative_width() {
    let src = "fn combine {dom clk: Clock, const n: integer, const m: integer} \
        (a: uint(n) @clk, b: uint(m) @clk) -> uint(n - m) @clk { a.resize() }\n\
        fn use_bad {dom clk: Clock} (a: uint(4) @clk, b: uint(8) @clk) -> uint(4) @clk \
        { combine(a, b) }\n";
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", src);
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    let diags = mono_check(&db, krate);
    assert!(
        diags
            .iter()
            .any(|d| d.message().contains("-4") && d.message().contains("must be >= 1")),
        "expected a negative-width diagnostic: {:?}",
        diags.iter().map(|d| d.message()).collect::<Vec<_>>()
    );
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
fn every_working_example_lowers_to_mir() {
    // MIR lowering is negative-space: any unhandled HIR shape panics loudly.
    // Building MIR for every fn/method in the corpus surfaces such gaps here
    // rather than later, while nothing else consumes MIR yet.
    for (name, src) in examples() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src);
        let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
        let map = crate_def_map(&db, krate);
        for def in map.defs().collect::<Vec<_>>() {
            if let Some(DefKind::Fn | DefKind::Method) = map.def_data(def).map(|d| d.kind) {
                let _ = mir_of(&db, krate, def);
            }
        }
        eprintln!("mir: {name}");
    }
}

#[test]
fn mir_pretty_dump_renders_unified_call_with_types() {
    // The `--emit mir` consumer: `value + 3` must lower to a unified resolved
    // call (`add`) with the operand types baked on the nodes. Validates the
    // pretty-printer and, through it, the S1 call-unification + types-on-node.
    let src = std::fs::read_to_string(working_dir().join("add_constant.mrn")).unwrap();
    let mut db = RootDatabase::default();
    let mut vfs = Vfs::new();
    vfs.set_file_text(&mut db, "t.mrn", src);
    let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
    let map = crate_def_map(&db, krate);
    let def = map
        .defs()
        .find(|d| {
            map.def_data(*d)
                .map(|dd| dd.name == "addConstant")
                .unwrap_or(false)
        })
        .expect("addConstant def");
    let dump = pretty_mir(&db, krate, mir_of(&db, krate, def));
    assert!(
        dump.contains("call add"),
        "expected unified call in:\n{dump}"
    );
    assert!(dump.contains("uint(8)"), "expected baked types in:\n{dump}");
}

#[test]
fn fail_expected_examples_lower_to_mir_without_panicking() {
    // Negative-space robustness: MIR lowering must DEGRADE (to `Missing` /
    // degenerate places) on ill-typed bodies, not crash. The fail-expected
    // corpus is the error-body suite — a hard crash here means a negative-space
    // panic fired on input that merely failed to type-check.
    for entry in std::fs::read_dir(fail_expected_dir()).expect("fail-expected") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("mrn") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "t.mrn", src);
        let krate: SourceRoot = vfs.source_root(&mut db, "t.mrn");
        let map = crate_def_map(&db, krate);
        for def in map.defs().collect::<Vec<_>>() {
            if let Some(DefKind::Fn | DefKind::Method) = map.def_data(def).map(|d| d.kind) {
                let _ = mir_of(&db, krate, def);
            }
        }
    }
}

#[test]
fn clean_examples_typecheck_without_diagnostics() {
    for (name, src) in examples() {
        let counts = diagnostic_counts(&src);
        let total = counts.0 + counts.1 + counts.2 + counts.3 + counts.4 + counts.5;
        if CLEAN.contains(&name.as_str()) {
            assert_eq!(
                counts,
                (0, 0, 0, 0, 0, 0),
                "{name} is listed CLEAN but produced diagnostics \
                 (nameres, sig, body, infer, drivers, directions) = {counts:?}"
            );
            assert_eq!(
                mono_diag_count(&src),
                0,
                "{name} is listed CLEAN but produced mono-check diagnostics"
            );
        } else {
            assert!(
                total > 0,
                "{name} is no longer producing diagnostics — promote it into CLEAN"
            );
        }
    }
}
