//! Virtual file system — the compiler's **input** boundary.
//!
//! Maps `path → SourceFile` (a salsa input) so all source text enters through
//! one place, never via direct `fs` reads. The batch CLI fills it from disk
//! once; the LSP overlays unsaved editor buffers. Setting a file's text is the
//! sole input mutation; salsa owns the revision counter underneath.
//! See `planning/query_engine.md` §5, `planning/lsp.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use salsa::Setter;

use crate::db::{RootDatabase, SourceFile};

/// The path → input map. Holds salsa `SourceFile` handles (which are `Copy`),
/// interning each path to a stable handle on first sight.
#[derive(Default)]
pub struct Vfs {
    by_path: HashMap<PathBuf, SourceFile>,
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
}
