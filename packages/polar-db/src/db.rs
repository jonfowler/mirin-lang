//! The query database — thin store.
//!
//! The deliberately minimal engine of `planning/query_engine.md` §7: a memo
//! table per query, coarse revision-based invalidation, no backdating yet. The
//! point of starting here is to nail down the *query signatures* and the
//! pure-function discipline (every query a function of the db, no hidden mutable
//! state) so the store underneath can later be swapped for `salsa` without
//! touching call sites. Q0 has exactly one query — `parse` — over the `Vfs`
//! input; later slices stack `item_tree`, `crate_def_map`, `infer`, … on top.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use tree_sitter::Tree;

use crate::parser;
use crate::vfs::{FileId, Revision, Vfs};

/// Result of parsing one file: the tree-sitter tree plus the exact text it was
/// parsed from. Held behind an `Arc` because `Tree` is not cheap to clone and
/// downstream queries (Q1's `item_tree`) share it.
pub struct ParseTree {
    pub tree: Tree,
    pub text: Arc<str>,
}

/// A memoized result tagged with the revision it was verified at.
struct Memo<T> {
    value: T,
    verified_at: Revision,
}

/// The query database. Owns the input [`Vfs`] and one memo table per query.
///
/// Queries take `&self` and memoize through `RefCell` interior mutability —
/// the same demand-driven shape salsa gives, minus red-green. Mutating an input
/// (`vfs_mut().set_text(..)`) advances revisions; a query recomputes only when
/// an input it read is newer than its memo.
pub struct Db {
    vfs: Vfs,
    parse_memo: RefCell<HashMap<FileId, Memo<Arc<ParseTree>>>>,
}

impl Db {
    pub fn new(vfs: Vfs) -> Self {
        Self {
            vfs,
            parse_memo: RefCell::new(HashMap::new()),
        }
    }

    pub fn vfs(&self) -> &Vfs {
        &self.vfs
    }

    /// Mutable access to inputs. Going through here is what advances revisions.
    pub fn vfs_mut(&mut self) -> &mut Vfs {
        &mut self.vfs
    }

    /// QUERY: parse a file into its tree-sitter tree.
    ///
    /// Memoized on the file's `changed_at` revision: re-parses only when the
    /// file's text has changed since the cached result. Whole-file reparse is
    /// cheap and is the right granularity — incrementality lives *above* parse,
    /// not in tree-sitter's byte-level incremental mode (§ query_engine.md "the
    /// two incrementalities").
    pub fn parse(&self, file: FileId) -> Arc<ParseTree> {
        let changed_at = self.vfs.changed_at(file);
        if let Some(memo) = self.parse_memo.borrow().get(&file) {
            if memo.verified_at >= changed_at {
                return Arc::clone(&memo.value);
            }
        }
        let text = self.vfs.text(file);
        let tree = parser::parse_text(&text);
        let value = Arc::new(ParseTree { tree, text });
        self.parse_memo.borrow_mut().insert(
            file,
            Memo {
                value: Arc::clone(&value),
                verified_at: changed_at,
            },
        );
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCUMULATOR: &str = "\
fn accumulator
  { dom clk: Clock, rstn: Reset @clk = high }
  ( data: uint(8) @clk )
  -> uint(8) @clk
  {
    var acc: uint(8) @clk = (acc + data).reg(rstn, 0);
    return acc;
  }
";

    fn db_with(text: &str) -> (Db, FileId) {
        let mut vfs = Vfs::new();
        let f = vfs.intern("test.plr");
        vfs.set_text(f, text);
        (Db::new(vfs), f)
    }

    #[test]
    fn parses_a_valid_file_without_errors() {
        let (db, f) = db_with(ACCUMULATOR);
        let parsed = db.parse(f);
        let root = parsed.tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(
            !root.has_error(),
            "a known-good example should parse without ERROR nodes"
        );
        assert_eq!(
            root.end_byte(),
            ACCUMULATOR.len(),
            "the tree should span the whole source"
        );
    }

    #[test]
    fn invalid_input_still_yields_a_tree() {
        // Error recovery: tree-sitter returns a tree with ERROR nodes rather
        // than failing. This is why it is a good IDE frontend.
        let (db, f) = db_with("fn { let ");
        let parsed = db.parse(f);
        assert_eq!(parsed.tree.root_node().kind(), "source_file");
        assert!(parsed.tree.root_node().has_error());
    }

    #[test]
    fn memoizes_until_the_file_changes() {
        let (mut db, f) = db_with("fn a {}");
        let first = db.parse(f);
        let again = db.parse(f);
        assert!(
            Arc::ptr_eq(&first, &again),
            "an unchanged file must hit the memo (same Arc)"
        );

        db.vfs_mut().set_text(f, "fn b {}");
        let after = db.parse(f);
        assert!(
            !Arc::ptr_eq(&first, &after),
            "a changed file must re-parse (new Arc)"
        );
    }

    #[test]
    fn distinct_files_are_independent() {
        let mut vfs = Vfs::new();
        let a = vfs.intern("a.plr");
        let b = vfs.intern("b.plr");
        vfs.set_text(a, ACCUMULATOR);
        vfs.set_text(b, "fn b {}"); // deliberately incomplete
        let db = Db::new(vfs);
        let pa = db.parse(a);
        let pb = db.parse(b);
        assert_ne!(&*pa.text, &*pb.text, "each file keeps its own parse");
        assert!(
            !pa.tree.root_node().has_error(),
            "the valid file parses cleanly"
        );
        assert!(
            pb.tree.root_node().has_error(),
            "the incomplete file carries an ERROR node, independently"
        );
    }
}
