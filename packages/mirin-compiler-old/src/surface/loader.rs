//! Multi-file loader — turns a crate root `.mrn` file plus its `mod foo;`
//! declarations into one combined [`SourceFile`].
//!
//! File-based modules (`planning/modules.md` §4.2): `mod foo;` inside
//! `dir/X.mrn` loads from `dir/foo.mrn`, and that module's own children live
//! under `dir/foo/`. A `.mrn` file is part of the crate only if some ancestor
//! declares it with `mod` — the filesystem does not define the graph.
//!
//! The loader produces a single `SourceFile` whose every module is
//! [`ModuleBody::Inline`]: file modules have been read, parsed, and spliced in.
//! Name resolution and HIR lowering are therefore unchanged — they already
//! recurse through inline modules.
//!
//! ## Spans across files
//!
//! Each file's source is appended to one combined buffer; the file's CST spans
//! are offset by its byte/row base before lowering, so spans are global
//! offsets into the combined buffer and a single source string renders correct
//! excerpts. (The `-->` path/line in a diagnostic is reported against the crate
//! root for sub-file errors; precise per-file source mapping is deferred to the
//! VFS work, `planning/modules.md` §8 item 6.)

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::surface::ir::{Item, LowerError, ModuleBody, SourceFile, lower_cst_seeded};
use crate::surface::parser::tree_sitter::{
    CstChild, CstNode, ParseError, SourceSpan, SyntaxDiagnostic, parse_source_with_diagnostics,
};

/// Supplies `.mrn` source text for a path. The filesystem implementation
/// ([`FsProvider`]) is the default; tests use an in-memory map. This is also
/// the seam a future VFS overlays editor buffers onto.
pub trait SourceProvider {
    fn read(&self, path: &Path) -> Result<String, std::io::Error>;
}

/// Reads source straight from disk.
pub struct FsProvider;

impl SourceProvider for FsProvider {
    fn read(&self, path: &Path) -> Result<String, std::io::Error> {
        std::fs::read_to_string(path)
    }
}

/// In-memory provider for tests: maps absolute-ish paths to source text.
#[derive(Default)]
pub struct MapProvider {
    files: BTreeMap<PathBuf, String>,
}

impl MapProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: impl Into<PathBuf>, source: impl Into<String>) {
        self.files.insert(path.into(), source.into());
    }
}

impl SourceProvider for MapProvider {
    fn read(&self, path: &Path) -> Result<String, std::io::Error> {
        self.files.get(path).cloned().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no in-memory file at {}", path.display()),
            )
        })
    }
}

/// A loaded crate: the combined `SourceFile`, the combined source buffer that
/// its spans index into, and any syntax diagnostics gathered across files.
#[derive(Debug)]
pub struct LoadedCrate {
    pub file: SourceFile,
    pub source: String,
    pub diagnostics: Vec<SyntaxDiagnostic>,
}

#[derive(Debug)]
pub enum LoadError {
    /// Could not read a file a `mod` declaration pointed at.
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    /// A file failed to parse outright.
    Parse { path: PathBuf, error: ParseError },
    /// CST → surface lowering failed.
    Lower { path: PathBuf, error: LowerError },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Read { path, error } => {
                write!(f, "cannot read module file {}: {error}", path.display())
            }
            LoadError::Parse { path, error } => {
                write!(f, "failed to parse {}: {error}", path.display())
            }
            LoadError::Lower { path, error } => {
                write!(f, "failed to lower {}: {}", path.display(), error.message)
            }
        }
    }
}

/// Load a crate rooted at `root_path` using the filesystem.
pub fn load_crate_from_fs(root_path: &Path) -> Result<LoadedCrate, LoadError> {
    load_crate(&FsProvider, root_path)
}

/// Load a crate rooted at `root_path` using the given source provider.
pub fn load_crate(
    provider: &dyn SourceProvider,
    root_path: &Path,
) -> Result<LoadedCrate, LoadError> {
    let mut loader = Loader {
        provider,
        next_id: 0,
        source: String::new(),
        diagnostics: Vec::new(),
    };
    // The root module's children live in the root file's directory.
    let root_dir = root_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let items = loader.load_file(root_path, &root_dir)?;
    Ok(LoadedCrate {
        file: SourceFile {
            // The root file's own span; the combined buffer is the source of
            // truth for rendering.
            span: SourceSpan::default(),
            items,
        },
        source: loader.source,
        diagnostics: loader.diagnostics,
    })
}

struct Loader<'p> {
    provider: &'p dyn SourceProvider,
    /// Crate-wide `NodeId` counter, threaded across every file so ids are
    /// unique across the whole crate (not just per file).
    next_id: u32,
    /// Combined source buffer; every file is appended, and spans are offset to
    /// index into it.
    source: String,
    diagnostics: Vec<SyntaxDiagnostic>,
}

impl Loader<'_> {
    /// Read, parse, and lower `path`, then recursively splice its file modules.
    /// `child_dir` is where `path`'s own `mod foo;` children are looked up.
    fn load_file(&mut self, path: &Path, child_dir: &Path) -> Result<Vec<Item>, LoadError> {
        let source = self.provider.read(path).map_err(|error| LoadError::Read {
            path: path.to_path_buf(),
            error,
        })?;
        let parsed = parse_source_with_diagnostics(&source).map_err(|error| LoadError::Parse {
            path: path.to_path_buf(),
            error,
        })?;

        // Append this file to the combined buffer; record its offsets so all
        // spans become global into the combined source.
        let byte_base = self.source.len();
        let row_base = self.source.bytes().filter(|&b| b == b'\n').count();
        self.source.push_str(&source);
        self.source.push('\n');

        let mut cst = parsed.cst;
        offset_node(&mut cst.root, byte_base, row_base);
        for mut diag in parsed.diagnostics {
            offset_span(&mut diag.span, byte_base, row_base);
            self.diagnostics.push(diag);
        }

        let (mut file, next_id) =
            lower_cst_seeded(&cst, &self.source, self.next_id).map_err(|error| {
                LoadError::Lower {
                    path: path.to_path_buf(),
                    error,
                }
            })?;
        self.next_id = next_id;

        self.fill_modules(&mut file.items, child_dir)?;
        Ok(file.items)
    }

    /// Replace every `ModuleBody::File` under `items` with the loaded items,
    /// and recurse through inline modules (whose file-children live in a
    /// subdirectory named after the module).
    fn fill_modules(&mut self, items: &mut [Item], dir: &Path) -> Result<(), LoadError> {
        for item in items {
            let Item::Mod(m) = item else { continue };
            let child_dir = dir.join(&m.name.text);
            match &mut m.body {
                ModuleBody::Inline(inner) => self.fill_modules(inner, &child_dir)?,
                ModuleBody::File => {
                    let file_path = dir.join(format!("{}.mrn", m.name.text));
                    let loaded = self.load_file(&file_path, &child_dir)?;
                    m.body = ModuleBody::Inline(loaded);
                }
            }
        }
        Ok(())
    }
}

fn offset_node(node: &mut CstNode, byte: usize, row: usize) {
    offset_span(&mut node.span, byte, row);
    for CstChild { node, .. } in &mut node.children {
        offset_node(node, byte, row);
    }
}

fn offset_span(span: &mut SourceSpan, byte: usize, row: usize) {
    span.start_byte += byte;
    span.end_byte += byte;
    span.start.row += row;
    span.end.row += row;
    // Columns are unchanged: each file starts at column 0 after the preceding
    // file's trailing newline.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve_file;

    fn load(files: &[(&str, &str)], root: &str) -> LoadedCrate {
        let mut provider = MapProvider::new();
        for (path, src) in files {
            provider.insert(*path, *src);
        }
        load_crate(&provider, Path::new(root)).expect("load should succeed")
    }

    #[test]
    fn loads_file_module_body() {
        let crate_ = load(
            &[
                ("/crate/main.mrn", "mod util;\n"),
                ("/crate/util.mrn", "fn helper(a: uint(8)) { let r = a; }\n"),
            ],
            "/crate/main.mrn",
        );
        // The `mod util;` body was loaded inline.
        let Item::Mod(m) = &crate_.file.items[0] else {
            panic!("expected a module item");
        };
        assert_eq!(m.name.text, "util");
        assert!(matches!(m.body, ModuleBody::Inline(ref items) if items.len() == 1));
    }

    #[test]
    fn loaded_module_resolves() {
        let crate_ = load(
            &[
                ("/crate/main.mrn", "mod util;\n"),
                (
                    "/crate/util.mrn",
                    "fn helper(a: uint(8)) { let r = helper(a); }\n",
                ),
            ],
            "/crate/main.mrn",
        );
        let r = resolve_file(&crate_.file);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        // `helper` lives under `util`, so its path is qualified.
        let helper = r.def_id("helper").unwrap();
        let names: Vec<String> = r
            .def_paths
            .def_path(helper)
            .segments
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, vec!["util", "helper"]);
    }

    #[test]
    fn node_ids_are_unique_across_files() {
        let crate_ = load(
            &[
                ("/c/main.mrn", "mod a;\nmod b;\n"),
                ("/c/a.mrn", "fn fa(x: uint(8)) { let r = x; }\n"),
                ("/c/b.mrn", "fn fb(x: uint(8)) { let r = x; }\n"),
            ],
            "/c/main.mrn",
        );
        // Resolution depends on NodeId uniqueness; a collision would make two
        // bindings alias. A clean resolve over the combined tree confirms it.
        let r = resolve_file(&crate_.file);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(r.def_id("fa").is_some() && r.def_id("fb").is_some());
    }

    #[test]
    fn nested_directory_layout() {
        // main.mrn → mod util; (util.mrn) → mod cfg; (util/cfg.mrn)
        let crate_ = load(
            &[
                ("/c/main.mrn", "mod util;\n"),
                ("/c/util.mrn", "mod cfg;\n"),
                ("/c/util/cfg.mrn", "fn parse(x: uint(8)) { let r = x; }\n"),
            ],
            "/c/main.mrn",
        );
        let r = resolve_file(&crate_.file);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        let parse = r.def_id("parse").unwrap();
        let names: Vec<String> = r
            .def_paths
            .def_path(parse)
            .segments
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, vec!["util", "cfg", "parse"]);
    }

    #[test]
    fn missing_module_file_is_load_error() {
        let mut provider = MapProvider::new();
        provider.insert("/c/main.mrn", "mod gone;\n");
        let err = load_crate(&provider, Path::new("/c/main.mrn")).unwrap_err();
        assert!(matches!(err, LoadError::Read { .. }), "got: {err}");
    }

    #[test]
    fn span_offsetting_keeps_text_aligned() {
        // The second file's identifier text must extract correctly from the
        // combined buffer — proving spans were offset consistently.
        let crate_ = load(
            &[
                ("/c/main.mrn", "mod sub;\n"),
                (
                    "/c/sub.mrn",
                    "fn distinctive_name(x: uint(8)) { let r = x; }\n",
                ),
            ],
            "/c/main.mrn",
        );
        let Item::Mod(m) = &crate_.file.items[0] else {
            panic!("expected module");
        };
        let ModuleBody::Inline(items) = &m.body else {
            panic!("expected inline");
        };
        let Item::Fn(f) = &items[0] else {
            panic!("expected fn");
        };
        assert_eq!(f.name.text, "distinctive_name");
        // The recorded span indexes into the combined buffer at the fn name.
        let s = &f.name.span;
        assert_eq!(&crate_.source[s.start_byte..s.end_byte], "distinctive_name");
    }
}
