//! Virtual file system — the compiler's **input** layer.
//!
//! All source text enters through the VFS as `(text, revision)`, never via
//! direct `fs` reads. This is the seam (`planning/query_engine.md` §5,
//! `planning/lsp.md`) that lets one in-process engine serve both the batch CLI
//! (fills the VFS from disk once) and the language server (overlays unsaved
//! editor buffers and bumps revisions on `didChange`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Stable handle to a file. Interned: the same path always maps to the same
/// `FileId` for the life of the VFS, so it is a safe memo key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// Monotonic revision counter, bumped on every input mutation. The seed of the
/// red-green incrementality story (`planning/query_engine.md` §1.2): a memo is
/// stale iff an input it read changed at a revision newer than the memo's.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Revision(pub u64);

struct FileRecord {
    #[allow(dead_code)] // surfaced once diagnostics carry file paths
    path: PathBuf,
    text: Arc<str>,
    /// Revision at which this file's text last changed.
    changed_at: Revision,
}

/// The input store: a `path → (text, revision)` overlay.
#[derive(Default)]
pub struct Vfs {
    by_path: HashMap<PathBuf, FileId>,
    files: Vec<FileRecord>,
    revision: Revision,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current global revision.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Intern a path, returning its stable [`FileId`]. A freshly interned file
    /// starts with empty text; call [`set_text`](Self::set_text) to populate it.
    pub fn intern(&mut self, path: impl Into<PathBuf>) -> FileId {
        let path = path.into();
        if let Some(&id) = self.by_path.get(&path) {
            return id;
        }
        let id = FileId(self.files.len() as u32);
        self.revision.0 += 1;
        self.files.push(FileRecord {
            path: path.clone(),
            text: Arc::from(""),
            changed_at: self.revision,
        });
        self.by_path.insert(path, id);
        id
    }

    /// Set a file's text, bumping the global revision and the file's
    /// `changed_at`. This is the only input mutation in the system.
    pub fn set_text(&mut self, file: FileId, text: impl Into<Arc<str>>) {
        self.revision.0 += 1;
        let rec = &mut self.files[file.0 as usize];
        rec.text = text.into();
        rec.changed_at = self.revision;
    }

    /// A file's current text (cheap `Arc` clone).
    pub fn text(&self, file: FileId) -> Arc<str> {
        Arc::clone(&self.files[file.0 as usize].text)
    }

    /// The revision at which `file` last changed — the input timestamp a query
    /// memo compares against to decide whether to recompute.
    pub fn changed_at(&self, file: FileId) -> Revision {
        self.files[file.0 as usize].changed_at
    }
}
