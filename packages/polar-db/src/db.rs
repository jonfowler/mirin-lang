//! The query database (salsa).
//!
//! `RootDatabase` is the single in-process engine that `planning/query_engine.md`
//! and `planning/lsp.md` call for — shared by the batch CLI and the future LSP.
//!
//! Note there is deliberately **no `parse` query**. A `tree_sitter::Tree` is not
//! `salsa::Update` (it is FFI-owned and not structurally comparable), so it
//! cannot be a tracked value — the CST wrinkle the Q0 spike confirmed. Parsing
//! is therefore a cheap transient *inside* the queries that need the tree
//! (`ast_id_map`, and `item_tree` in Q1c), each of which returns an owned,
//! comparable summary. This is where Polar diverges from rust-analyzer, whose
//! rowan green trees *are* storable and so back a real `parse` query.

/// A source file input. Its `text` is the only mutable input in the system;
/// salsa tracks the revision and drives all downstream invalidation. Created
/// and updated through the [`crate::vfs::Vfs`] bridge.
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub path: std::path::PathBuf,
    #[returns(ref)]
    pub text: String,
}

/// The root query database.
#[salsa::db]
#[derive(Default, Clone)]
pub struct RootDatabase {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for RootDatabase {}
