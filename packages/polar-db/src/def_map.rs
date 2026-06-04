//! `crate_def_map` — name resolution's firewall #2 (`planning/query_engine.md`
//! §3.1).
//!
//! Builds the crate's **module tree** and **name tables** from the per-file
//! [`item_tree`](crate::item_tree)s. Depends only on item-tree *names and
//! structure*, never on bodies or types, so a body edit cannot reach it: the
//! item_tree firewall absorbs the edit (its value is unchanged), this query
//! backdates, and every dependent survives. This is the boundary that keeps
//! goto-def / privacy / signature resolution cached across body edits.
//!
//! Ports the *name-resolution half* of `polar-compiler`'s `resolve.rs`
//! (`collect_items` → the module + def tree). The body-resolution half
//! (`resolve_items`) is deliberately **not** here — it lands in Q3 behind the
//! `sig_of`/`body` split. The whole local repo is one crate (§3.5); this query
//! is keyed on the crate's [`SourceRoot`](crate::db::SourceRoot) (root file +
//! file set), which is what lets it resolve `mod foo;` to another file.
//!
//! **Scope so far:** the module tree — root, inline `mod`, and `mod foo;` file
//! modules (Q2b) — and name tables for the named items (`fn`/`struct`/`port`/
//! `mod`) in the `{Module, Item}` namespaces. Still to come: `use` imports +
//! privacy (Q2c), and the impl-method index + `DefPath` table + the
//! `struct S = S` collision check (Q2d).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::db::{SourceFile, SourceRoot};
use crate::ids::{DefId, DefKind, Namespace};
use crate::item_tree::{Item, ModItem, ModKind, item_tree};

/// Index into [`CrateDefMap::modules`]. The root is always `ModuleId(0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::Update)]
pub struct ModuleId(u32);

/// What a module *is*.
//
// No `Debug`: the salsa-interned `DefId` carries no std `Debug` (its fields need
// the db to read), so the types that embed one omit it too.
#[derive(Clone, Copy, PartialEq, Eq, salsa::Update)]
pub enum ModuleKind<'db> {
    /// The crate root — the top-level scope of the root file.
    Root,
    /// A `mod foo { … }` (or, from Q2b, `mod foo;`); carries the module's `DefId`.
    Named(DefId<'db>),
}

/// One module's data: what it is, its parent, and the names defined directly in
/// it. Modeled on `resolve.rs::ModuleData` (the prelude + import-priority parts
/// arrive in later slices). Lookups go through [`CrateDefMap`].
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct ModuleData<'db> {
    kind: ModuleKind<'db>,
    parent: Option<ModuleId>,
    /// Names defined directly in this module, keyed by `(name, namespace)`.
    items: HashMap<(String, Namespace), DefId<'db>>,
}

impl<'db> ModuleData<'db> {
    pub fn kind(&self) -> ModuleKind<'db> {
        self.kind
    }

    pub fn parent(&self) -> Option<ModuleId> {
        self.parent
    }

    /// Iterate this module's `(name, namespace) → DefId` entries.
    pub fn items(&self) -> impl Iterator<Item = (&(String, Namespace), &DefId<'db>)> {
        self.items.iter()
    }
}

/// Per-def metadata, recoverable from a `DefId` alone (the way rustc exposes
/// `tcx.def_kind`). The `DefPath` lands here in Q2d.
#[derive(Debug, Clone, PartialEq, Eq, salsa::Update)]
pub struct DefData<'db> {
    pub kind: DefKind,
    pub name: String,
    /// The module this def is declared in.
    pub module: ModuleId,
    /// Marker so the lifetime is used even before `DefPath` (which references
    /// other defs) lands; lets Q2d add owner-carrying fields without churn.
    _krate: std::marker::PhantomData<DefId<'db>>,
}

/// The crate's name-resolution map: the module tree plus per-def metadata. The
/// return value of the [`crate_def_map`] query.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct CrateDefMap<'db> {
    modules: Vec<ModuleData<'db>>,
    root: ModuleId,
    defs: HashMap<DefId<'db>, DefData<'db>>,
}

impl<'db> CrateDefMap<'db> {
    pub fn root(&self) -> ModuleId {
        self.root
    }

    pub fn module(&self, id: ModuleId) -> &ModuleData<'db> {
        &self.modules[id.0 as usize]
    }

    pub fn modules(&self) -> &[ModuleData<'db>] {
        &self.modules
    }

    pub fn def_data(&self, def: DefId<'db>) -> Option<&DefData<'db>> {
        self.defs.get(&def)
    }

    pub fn num_defs(&self) -> usize {
        self.defs.len()
    }

    /// Resolve a bare name in one module's table (no prelude/ancestor/import
    /// fallback yet — those land in Q2c).
    pub fn resolve_local(&self, module: ModuleId, name: &str, ns: Namespace) -> Option<DefId<'db>> {
        self.modules[module.0 as usize]
            .items
            .get(&(name.to_owned(), ns))
            .copied()
    }
}

/// QUERY: the crate's name-resolution map, built from the root file's
/// `item_tree` and the file modules it pulls in (`mod foo;`).
#[salsa::tracked(returns(ref))]
pub fn crate_def_map<'db>(db: &'db dyn salsa::Database, krate: SourceRoot) -> CrateDefMap<'db> {
    let mut collector = Collector::new(db, krate);
    let root_module = collector.new_module(ModuleKind::Root, None);
    debug_assert_eq!(root_module, collector.map.root);
    let root = krate.root_file(db);
    let tree = item_tree(db, root);
    // File modules declared at the crate root resolve next to the root file.
    let root_dir = root
        .path(db)
        .parent()
        .map(Path::to_owned)
        .unwrap_or_default();
    collector.collect_items(&tree.top_level, root, root_module, &root_dir);
    collector.map
}

struct Collector<'db> {
    db: &'db dyn salsa::Database,
    map: CrateDefMap<'db>,
    /// Path → file, for resolving `mod foo;` to another file in the crate.
    files: HashMap<PathBuf, SourceFile>,
}

impl<'db> Collector<'db> {
    fn new(db: &'db dyn salsa::Database, krate: SourceRoot) -> Self {
        let files = krate
            .files(db)
            .iter()
            .map(|&f| (f.path(db).clone(), f))
            .collect();
        Self {
            db,
            map: CrateDefMap {
                modules: Vec::new(),
                root: ModuleId(0),
                defs: HashMap::new(),
            },
            files,
        }
    }

    fn new_module(&mut self, kind: ModuleKind<'db>, parent: Option<ModuleId>) -> ModuleId {
        let id = ModuleId(self.map.modules.len() as u32);
        self.map.modules.push(ModuleData {
            kind,
            parent,
            items: HashMap::new(),
        });
        id
    }

    /// Collect the items declared in one module. `file` is the file they live in;
    /// `dir` is the directory in which a `mod foo;` among them resolves to
    /// `dir/foo.plr`.
    fn collect_items(&mut self, items: &[Item], file: SourceFile, module: ModuleId, dir: &Path) {
        for item in items {
            match item {
                Item::Fn(f) => {
                    self.declare(file, f.ast_id, &f.name, DefKind::Fn, module);
                }
                Item::Struct(s) => {
                    self.declare(file, s.ast_id, &s.name, DefKind::Struct, module);
                }
                Item::Port(p) => {
                    self.declare(file, p.ast_id, &p.name, DefKind::Port, module);
                }
                Item::Mod(m) => self.collect_mod(m, file, module, dir),
                // Impls (the method index) and `use` imports are name-resolution
                // concerns too, but land in Q2d / Q2c respectively.
                Item::Impl(_) | Item::Use(_) => {}
            }
        }
    }

    /// Mint a `DefId` for a named item and enter it into its module's name table.
    fn declare(
        &mut self,
        file: SourceFile,
        ast_id: crate::ast_id::FileAstId,
        name: &str,
        kind: DefKind,
        module: ModuleId,
    ) -> DefId<'db> {
        let def = DefId::new(self.db, file, ast_id);
        self.map.defs.insert(
            def,
            DefData {
                kind,
                name: name.to_owned(),
                module,
                _krate: std::marker::PhantomData,
            },
        );
        // First binding wins; duplicate-name diagnostics arrive with the error
        // surface (Q6). Until then this keeps a deterministic table.
        self.map.modules[module.0 as usize]
            .items
            .entry((name.to_owned(), kind.namespace()))
            .or_insert(def);
        def
    }

    /// Collect a `mod m`. Its declaration is recorded in `parent`; its body comes
    /// either from the inline `{ … }` (same file) or, for `mod m;`, from the file
    /// `dir/m.plr`. Either way the module's *own* children resolve their file
    /// modules in `dir/m` — each level owns a deeper directory, so the file tree
    /// strictly deepens and cannot cycle.
    fn collect_mod(&mut self, m: &ModItem, file: SourceFile, parent: ModuleId, dir: &Path) {
        let def = self.declare(file, m.ast_id, &m.name, DefKind::Mod, parent);
        let sub = self.new_module(ModuleKind::Named(def), Some(parent));
        let child_dir = dir.join(&m.name);
        match &m.kind {
            ModKind::Inline(children) => {
                self.collect_items(children, file, sub, &child_dir);
            }
            ModKind::File => {
                let mod_path = dir.join(format!("{}.plr", m.name));
                if let Some(&mod_file) = self.files.get(&mod_path) {
                    let tree = item_tree(self.db, mod_file);
                    self.collect_items(&tree.top_level, mod_file, sub, &child_dir);
                }
                // else: unresolved module file. The `mod` name still resolves (to
                // an empty module); the "file not found" diagnostic lands in Q6.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::RootDatabase;
    use crate::vfs::Vfs;

    /// A 'static projection of the def map — names/kinds/module structure with
    /// the `'db`-bound `DefId`s dropped — so a test can compare two revisions
    /// without holding a db borrow across the mutating edit between them.
    fn summary(map: &CrateDefMap) -> Vec<(usize, String, DefKind, Namespace)> {
        let mut out = Vec::new();
        for (i, module) in map.modules().iter().enumerate() {
            for ((name, ns), _def) in module.items() {
                out.push((
                    i,
                    name.clone(),
                    {
                        let d =
                            map.def_data(map.resolve_local(ModuleId(i as u32), name, *ns).unwrap());
                        d.unwrap().kind
                    },
                    *ns,
                ));
            }
        }
        out.sort();
        out
    }

    /// Load one file as the crate root and build its `SourceRoot`.
    fn single(db: &mut RootDatabase, vfs: &mut Vfs, path: &str, text: &str) -> SourceRoot {
        vfs.set_file_text(db, path, text);
        vfs.source_root(db, path)
    }

    const SAMPLE: &str = "\
pub fn top (x: uint(8)) -> uint(8) { return x; }
struct S = S { a: uint(8) }
port P = p { in a: uint(8) }
mod inner {
  fn nested () -> uint(8) { return 0; }
}
";

    #[test]
    fn mints_a_def_per_named_item_in_the_right_namespace() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "t.plr", SAMPLE);
        let map = crate_def_map(&db, krate);
        let root = map.root();

        // Types and fns share the Item namespace.
        let top = map
            .resolve_local(root, "top", Namespace::Item)
            .expect("fn top");
        assert_eq!(map.def_data(top).unwrap().kind, DefKind::Fn);
        assert_eq!(
            map.resolve_local(root, "S", Namespace::Item)
                .map(|d| map.def_data(d).unwrap().kind),
            Some(DefKind::Struct)
        );
        assert_eq!(
            map.resolve_local(root, "P", Namespace::Item)
                .map(|d| map.def_data(d).unwrap().kind),
            Some(DefKind::Port)
        );
        // `mod` lives in the Module namespace, separate from items: `inner` is
        // not an Item, and `top` is not a Module.
        assert!(map.resolve_local(root, "inner", Namespace::Item).is_none());
        assert!(map.resolve_local(root, "top", Namespace::Module).is_none());
        assert_eq!(
            map.resolve_local(root, "inner", Namespace::Module)
                .map(|d| map.def_data(d).unwrap().kind),
            Some(DefKind::Mod)
        );
    }

    #[test]
    fn inline_mod_nests_a_named_module_with_its_own_table() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "t.plr", SAMPLE);
        let map = crate_def_map(&db, krate);

        // root + inner = 2 modules.
        assert_eq!(map.modules().len(), 2);
        let inner_def = map
            .resolve_local(map.root(), "inner", Namespace::Module)
            .unwrap();
        // The named module points back at its def, and its parent is the root.
        let inner_mod = map
            .modules()
            .iter()
            .position(|m| matches!(m.kind(), ModuleKind::Named(d) if d == inner_def))
            .map(|i| ModuleId(i as u32))
            .expect("named module for inner");
        assert_eq!(map.module(inner_mod).parent(), Some(map.root()));
        // `nested` resolves inside `inner`, not at the root.
        assert!(
            map.resolve_local(inner_mod, "nested", Namespace::Item)
                .is_some()
        );
        assert!(
            map.resolve_local(map.root(), "nested", Namespace::Item)
                .is_none()
        );
    }

    #[test]
    fn def_id_is_stable_across_an_unrelated_body_edit() {
        // The firewall: editing a body leaves crate_def_map value-equal (it is a
        // pure function of the item_tree, which backdates), so dependents survive.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "t.plr", SAMPLE);
        let before = summary(crate_def_map(&db, krate));

        vfs.set_file_text(
            &mut db,
            "t.plr",
            // `top`'s body changed; every item's identity is unchanged.
            "\
pub fn top (x: uint(8)) -> uint(8) { return x + x + x; }
struct S = S { a: uint(8) }
port P = p { in a: uint(8) }
mod inner {
  fn nested () -> uint(8) { return 0; }
}
",
        );
        let after = summary(crate_def_map(&db, krate));
        assert_eq!(before, after, "a body edit must not change the def map");
    }

    // ----- Q2b: `mod foo;` file modules -----

    /// The named module created for `name` (under any parent), if any.
    fn named_module(map: &CrateDefMap, name: &str, ns: Namespace) -> Option<ModuleId> {
        // `name` resolves to the mod def somewhere; find the module pointing at it.
        let def = map
            .modules()
            .iter()
            .enumerate()
            .find_map(|(i, _)| map.resolve_local(ModuleId(i as u32), name, ns))?;
        map.modules()
            .iter()
            .position(|m| matches!(m.kind(), ModuleKind::Named(d) if d == def))
            .map(|i| ModuleId(i as u32))
    }

    #[test]
    fn mod_foo_loads_a_sibling_file() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(
            &mut db,
            "top.plr",
            "mod child;\nfn root_fn () -> uint(8) { return 0; }",
        );
        vfs.set_file_text(
            &mut db,
            "child.plr",
            "fn helper () -> uint(8) { return 0; }",
        );
        let krate = vfs.source_root(&mut db, "top.plr");
        let map = crate_def_map(&db, krate);

        // The root holds `child` (Module ns) and `root_fn` (Item ns).
        assert!(
            map.resolve_local(map.root(), "root_fn", Namespace::Item)
                .is_some()
        );
        let child = named_module(map, "child", Namespace::Module).expect("child module");
        assert_eq!(map.module(child).parent(), Some(map.root()));
        // `helper` from child.plr lives in the child module, not the root.
        assert!(
            map.resolve_local(child, "helper", Namespace::Item)
                .is_some()
        );
        assert!(
            map.resolve_local(map.root(), "helper", Namespace::Item)
                .is_none()
        );
    }

    #[test]
    fn nested_file_modules_resolve_into_a_subdirectory() {
        // `mod a;` at the root → a.plr; `mod b;` inside a → a/b.plr.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "top.plr", "mod a;");
        vfs.set_file_text(
            &mut db,
            "a.plr",
            "mod b;\nfn in_a () -> uint(8) { return 0; }",
        );
        vfs.set_file_text(&mut db, "a/b.plr", "fn in_b () -> uint(8) { return 0; }");
        let krate = vfs.source_root(&mut db, "top.plr");
        let map = crate_def_map(&db, krate);

        let a = named_module(map, "a", Namespace::Module).expect("module a");
        assert!(map.resolve_local(a, "in_a", Namespace::Item).is_some());
        let b = named_module(map, "b", Namespace::Module).expect("module b");
        assert_eq!(map.module(b).parent(), Some(a));
        assert!(map.resolve_local(b, "in_b", Namespace::Item).is_some());
    }

    #[test]
    fn missing_module_file_yields_an_empty_module() {
        // `mod ghost;` with no ghost.plr: the name still resolves, body is empty.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "top.plr", "mod ghost;");
        let map = crate_def_map(&db, krate);

        let ghost = named_module(map, "ghost", Namespace::Module).expect("ghost module");
        assert_eq!(map.module(ghost).items().count(), 0);
    }

    #[test]
    fn editing_a_file_modules_body_does_not_change_the_def_map() {
        // The firewall across files: a body edit in child.plr backdates its
        // item_tree, so the crate-wide def map is unchanged.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "top.plr", "mod child;");
        vfs.set_file_text(
            &mut db,
            "child.plr",
            "fn helper () -> uint(8) { return 0; }",
        );
        let krate = vfs.source_root(&mut db, "top.plr");
        let before = summary(crate_def_map(&db, krate));

        vfs.set_file_text(
            &mut db,
            "child.plr",
            "fn helper () -> uint(8) { return 0 + 1 + 2; }",
        );
        let after = summary(crate_def_map(&db, krate));
        assert_eq!(
            before, after,
            "a body edit in a file module must not change the def map"
        );
    }
}
