//! Multi-file crate loading: resolve a root `.mrn` file's `mod foo;`
//! declarations to on-disk files (`dir/foo.mrn`, children under `dir/foo/`),
//! transitively, into a [`Vfs`] — shared by the CLI and the test harnesses.

use std::fs;
use std::path::{Path, PathBuf};

use crate::base::db::{RootDatabase, SourceRoot};
use crate::base::parser::parse_text;
use crate::base::vfs::Vfs;

/// Load the root file and, transitively, every `mod foo;` file it pulls in,
/// then build the crate's [`SourceRoot`] over the loaded set.
pub fn load_crate(
    db: &mut RootDatabase,
    vfs: &mut Vfs,
    root_path: &Path,
) -> std::io::Result<SourceRoot> {
    let root_dir = root_path.parent().unwrap_or(Path::new(".")).to_owned();
    // Worklist of (file path, dir its own `mod foo;` files resolve in).
    let mut work = vec![(root_path.to_owned(), root_dir)];
    while let Some((path, dir)) = work.pop() {
        if vfs.file(&path).is_some() {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        vfs.set_file_text(db, path.clone(), text.clone());
        // Discover the file modules this file declares (at any nesting) and
        // queue the ones that exist on disk.
        let tree = parse_text(&text);
        let mut found = Vec::new();
        discover_file_mods(&tree.root_node(), &dir, &text, &mut found);
        for (mod_path, child_dir) in found {
            if mod_path.exists() {
                work.push((mod_path, child_dir));
            }
        }
    }
    Ok(vfs.source_root(db, root_path))
}

/// Walk a container node (the file root or an inline `mod` body) for module
/// declarations. An inline `mod m { … }` recurses into its body under `dir/m`;
/// a file `mod m;` yields `(dir/m.mrn, dir/m)` to load.
fn discover_file_mods(
    container: &tree_sitter::Node,
    dir: &Path,
    source: &str,
    out: &mut Vec<(PathBuf, PathBuf)>,
) {
    let mut cursor = container.walk();
    for child in container.children(&mut cursor) {
        if child.kind() != "module_definition" {
            continue;
        }
        let Some(name) = child
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        else {
            continue;
        };
        let child_dir = dir.join(name);
        match child.child_by_field_name("body") {
            Some(body) => discover_file_mods(&body, &child_dir, source, out),
            None => out.push((dir.join(format!("{name}.mrn")), child_dir)),
        }
    }
}
