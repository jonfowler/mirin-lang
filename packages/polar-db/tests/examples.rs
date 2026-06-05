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

use polar_db::{
    DefKind, RootDatabase, SourceRoot, Vfs, body, check_drivers, crate_def_map, directions, infer,
    sig_of,
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

/// Dev aid: per-example diagnostic tally. `cargo test -p polar-db --test examples
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
