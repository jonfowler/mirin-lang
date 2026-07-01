//! Virtual file system — the compiler's **input** boundary.
//!
//! Maps `path → SourceFile` (a salsa input) so all source text enters through
//! one place, never via direct `fs` reads. The batch CLI fills it from disk
//! once; the LSP overlays unsaved editor buffers. Setting a file's text is the
//! sole input mutation; salsa owns the revision counter underneath.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use salsa::Setter;

use crate::base::db::{RootDatabase, SourceFile, SourceRoot};

/// The path → input map. Holds salsa `SourceFile` handles (which are `Copy`),
/// interning each path to a stable handle on first sight.
#[derive(Default)]
pub struct Vfs {
    by_path: HashMap<PathBuf, SourceFile>,
    /// The reused `SourceRoot` input; its `files` are refreshed by
    /// [`Vfs::source_root`] so resolution sees the current set.
    root: Option<SourceRoot>,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a file's text, creating the salsa input on first sight and updating
    /// it (advancing salsa's revision) thereafter. Returns the file handle.
    pub fn set_file_text(
        &mut self,
        db: &mut RootDatabase,
        path: impl Into<PathBuf>,
        text: impl Into<String>,
    ) -> SourceFile {
        let path = path.into();
        let text = text.into();
        match self.by_path.get(&path) {
            Some(&file) => {
                file.set_text(db).to(text);
                file
            }
            None => {
                let file = SourceFile::new(db, path.clone(), text);
                self.by_path.insert(path, file);
                file
            }
        }
    }

    /// The handle for an already-known path, if any.
    pub fn file(&self, path: impl AsRef<Path>) -> Option<SourceFile> {
        self.by_path.get(path.as_ref()).copied()
    }

    /// Build (or refresh) the [`SourceRoot`] for a crate rooted at `root_path`,
    /// covering every file currently in the VFS. Call after the files a query
    /// needs are loaded; re-calling updates the file set in place (reusing the
    /// same input, so resolution stays incremental). Panics if `root_path` is
    /// not loaded.
    /// The prelude's in-VFS path. Never a real on-disk file; the leading
    /// `$` keeps it out of `mod` resolution's way.
    pub const PRELUDE_PATH: &str = "$prelude.mrn";

    pub fn source_root(
        &mut self,
        db: &mut RootDatabase,
        root_path: impl AsRef<Path>,
    ) -> SourceRoot {
        // When the root file under analysis is itself a `prelude.mrn` (e.g.
        // editing the compiler's own prelude in the LSP), it already provides
        // every prelude def. Injecting the bundled copy as well would define
        // each operator trait twice — making `a * b` and friends ambiguous
        // ("multiple applicable methods `mul`: implemented by traits Mul, Mul").
        // Treat the open file AS the prelude: skip injection and drop any
        // synthetic copy left in the VFS by a prior analysis.
        let root_is_prelude = root_path.as_ref().file_name()
            == Some(std::ffi::OsStr::new("prelude.mrn"))
            && root_path.as_ref() != Path::new(Self::PRELUDE_PATH);
        // Every crate carries the prelude source (rustc's `core` move):
        // operator traits + builtin impls as real,
        // checked code, collected into the `$prelude` module by the def map.
        if !root_is_prelude && self.file(Self::PRELUDE_PATH).is_none() {
            self.set_file_text(
                db,
                Self::PRELUDE_PATH,
                include_str!("../prelude.mrn").to_owned(),
            );
        }
        let root_file = self
            .file(&root_path)
            .expect("root file must be loaded before building its SourceRoot");
        // Deterministic order so the input's value is stable across rebuilds.
        let mut files: Vec<SourceFile> = self
            .by_path
            .values()
            .copied()
            .filter(|f| !(root_is_prelude && f.path(db) == Path::new(Self::PRELUDE_PATH)))
            .collect();
        files.sort_by(|a, b| a.path(db).cmp(b.path(db)));
        match self.root {
            Some(root) => {
                root.set_root_file(db).to(root_file);
                root.set_files(db).to(files);
                root
            }
            None => {
                let root = SourceRoot::new(db, root_file, files);
                self.root = Some(root);
                root
            }
        }
    }
}
