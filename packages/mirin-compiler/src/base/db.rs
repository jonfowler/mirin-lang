//! The query database (salsa).
//!
//! `RootDatabase` is the single in-process engine shared by the batch CLI and the
//! future LSP.
//!
//! Note there is deliberately **no `parse` query**. A `tree_sitter::Tree` is not
//! `salsa::Update` (it is FFI-owned and not structurally comparable), so it
//! cannot be a tracked value — the CST wrinkle the Q0 spike confirmed. Parsing
//! is therefore a cheap transient *inside* the queries that need the tree
//! (`ast_id_map`, and `item_tree` in Q1c), each of which returns an owned,
//! comparable summary. This is where Mirin diverges from rust-analyzer, whose
//! rowan green trees *are* storable and so back a real `parse` query.

/// A source file input. Its `text` is the only mutable *content* input in the
/// system; salsa tracks the revision and drives all downstream invalidation.
/// Created and updated through the [`crate::base::vfs::Vfs`] bridge.
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub path: std::path::PathBuf,
    #[returns(ref)]
    pub text: String,
}

/// The crate's file set — the minimal "crate handle" a query needs to resolve
/// `mod foo;` declarations to other files (one
/// local crate, no crate graph). Keyed on this, `crate_def_map` can map a
/// computed module path to its [`SourceFile`]. NOT a crate-graph node; just the
/// root file plus the set of files reachable for module resolution.
///
/// `files` changes only when a file is added or removed (a text edit mutates the
/// individual `SourceFile`, leaving this input untouched), so editing a body
/// never invalidates name resolution through here.
#[salsa::input]
pub struct SourceRoot {
    /// The crate root — where module resolution starts.
    pub root_file: SourceFile,
    /// Every file in the crate, sorted by path for a deterministic value.
    #[returns(ref)]
    pub files: Vec<SourceFile>,
}

/// The root query database.
#[salsa::db]
#[derive(Default, Clone)]
pub struct RootDatabase {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for RootDatabase {}
