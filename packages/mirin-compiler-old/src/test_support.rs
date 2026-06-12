//! Helpers shared by unit tests across the crate.
//!
//! Both `examples/working/` and `examples/fail-expected/` are discovered at
//! test time via `std::fs::read_dir`, so adding a new `.mrn` file under
//! either directory automatically picks it up — no need to extend an
//! `include_str!` list per pass.

#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};

/// Path to the repository root, derived from the package's manifest dir.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// `(name, source)` for every `.mrn` file under `examples/working/`, sorted by
/// name for deterministic test output.
pub fn working_examples() -> Vec<(String, String)> {
    read_plr_dir(&repo_root().join("examples").join("working"))
}

/// `(name, source)` for every `.mrn` file under `examples/fail-expected/`,
/// sorted by name.
#[allow(dead_code)]
pub fn fail_expected_examples() -> Vec<(String, String)> {
    read_plr_dir(&repo_root().join("examples").join("fail-expected"))
}

fn read_plr_dir(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("mrn") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("mrn filename")
            .to_owned();
        let source =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        out.push((name, source));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
