//! Salsa engine spike — feature `spike-salsa`, not part of the default build.
//!
//! Purpose (`planning/query_engine.md` §7): implement the Q0 `parse` query a
//! second time, on the real `salsa` crate, so the engine choice (thin store vs
//! salsa) is made from hands-on contact rather than speculation. Findings are
//! recorded inline below; the thin store in [`crate::db`] remains the default.
//!
//! ## Findings — VERDICT: salsa is clean enough to adopt.
//!
//! Compiled as written except for **one** fix: the `.set_text(..).to(..)`
//! setter needs `use salsa::Setter` in scope. The input struct, tracked fn,
//! `#[salsa::db]` shell, and `#[returns(ref)]` getter all worked first try.
//!
//! - **Ergonomics.** The input + tracked-fn boilerplate is small and reads
//!   cleanly; memoization and input-revision tracking are automatic (the test
//!   confirms one execution for two calls, recompute after a `set_text`). This
//!   is materially less code than the thin store's hand-written memo table.
//! - **`'db` lifetime.** Not felt yet — `parse_sexp` returns an owned `String`,
//!   so no tracked *struct* (which is what carries `'db`) appears. It will show
//!   up at Q1 when `item_tree` returns a tracked struct of items.
//! - **The CST wrinkle, confirmed.** A tracked return value must be `'static +
//!   salsa::Update`. `tree_sitter::Tree` is neither, so it cannot be a tracked
//!   output. The forced response is the design we want regardless: do not store
//!   the CST in the db — lower it to an owned, comparable value (a sexp `String`
//!   here; our own `item_tree` in Q1) and let the tree stay a transient inside
//!   the query body.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::parser;

/// Counts executions of the tracked `parse_sexp` body, so a test can confirm
/// salsa actually memoized (no recompute when the input is unchanged).
pub static PARSE_RUNS: AtomicU32 = AtomicU32::new(0);

/// The salsa database. Compare with [`crate::db::Db`]: salsa supplies the
/// storage, memoization, and revision machinery; we supply only this shell.
#[salsa::db]
#[derive(Default, Clone)]
pub struct SpikeDb {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for SpikeDb {}

/// The file-text input. Compare with [`crate::vfs::Vfs`]: salsa makes the input
/// a first-class tracked struct with a generated setter and revision tracking.
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub text: String,
}

/// QUERY (salsa): parse a file and return its root S-expression.
///
/// Returns an owned `String` precisely because the tree-sitter `Tree` cannot be
/// a tracked value (see module findings).
#[salsa::tracked]
pub fn parse_sexp(db: &dyn salsa::Database, file: SourceFile) -> String {
    PARSE_RUNS.fetch_add(1, Ordering::Relaxed);
    let tree = parser::parse_text(file.text(db).as_str());
    tree.root_node().to_sexp()
}

#[cfg(test)]
mod tests {
    // `salsa::Setter` brings the generated `.set_text(..).to(..)` setter into scope.
    use salsa::Setter;

    use super::*;

    #[test]
    fn salsa_parses_memoizes_and_invalidates() {
        PARSE_RUNS.store(0, Ordering::Relaxed);
        let mut db = SpikeDb::default();
        let file = SourceFile::new(&db, "fn a {}".to_string());

        let first = parse_sexp(&db, file);
        assert!(first.contains("source_file"));

        // Second call, unchanged input: salsa returns the memo, no recompute.
        let second = parse_sexp(&db, file);
        assert_eq!(first, second);
        assert_eq!(
            PARSE_RUNS.load(Ordering::Relaxed),
            1,
            "salsa should memoize: one body execution for two identical calls"
        );

        // Mutate the input: salsa invalidates and recomputes exactly once.
        file.set_text(&mut db).to("fn bb {}".to_string());
        let _ = parse_sexp(&db, file);
        assert_eq!(
            PARSE_RUNS.load(Ordering::Relaxed),
            2,
            "changing the input should trigger exactly one recompute"
        );
    }
}
