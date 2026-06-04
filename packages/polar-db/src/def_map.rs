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
//! modules (Q2b); name tables in the `{Module, Item}` namespaces with
//! constructors (`struct Bus = bus`) and the `struct S = S` collision check;
//! `use` imports to a fixpoint with privacy (Q2c); the impl-method index and the
//! stable `DefPath`/`DefPathHash` table (Q2d). Still to come: the prelude /
//! ancestor lookup that body resolution needs (Q3).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::db::{SourceFile, SourceRoot};
use crate::ids::{
    DefId, DefKind, DefPath, DefPathHash, DefPathSegment, DefPathSegmentKind, DefRole, Namespace,
    StableCrateId,
};
use crate::item_tree::{
    ImplItem, Item, ModItem, ModKind, UseTree, Visibility as SurfaceVisibility, item_tree,
};

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

/// A resolved accessibility scope (the surface `pub`/`pub(crate)`/… resolved
/// against the module tree). Mirrors `resolve.rs::Visibility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::Update)]
pub enum Visibility {
    /// `pub` — visible everywhere.
    Public,
    /// `pub(crate)` — visible anywhere in the crate.
    Crate,
    /// Visible within the given module's subtree. Covers private (the defining
    /// module), `pub(super)` (the parent), and `pub(in path)`.
    Restricted(ModuleId),
}

/// How a name entered a module's table — drives import priority
/// (`Def > Import > Glob`). Mirrors `resolve.rs::BindingSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::Update)]
pub enum BindingSource {
    /// Defined directly in this module.
    Def,
    /// Brought in by an explicit `use a::b;` (or group/`as`).
    Import,
    /// Brought in by a glob `use a::*;`.
    Glob,
}

/// One entry in a module's name table: what it resolves to, how it got there,
/// and the binding's visibility (a def's own, or a `pub use` re-export's).
#[derive(Clone, Copy, PartialEq, Eq, salsa::Update)]
pub struct Binding<'db> {
    pub def: DefId<'db>,
    pub source: BindingSource,
    pub vis: Visibility,
}

/// A name-resolution diagnostic carried by the def map (RA's `DefMap` carries
/// its diagnostics the same way). Spans arrive with the diagnostics infra (Q6);
/// for now the offending name/path is enough to test behaviour.
#[derive(Debug, Clone, PartialEq, Eq, salsa::Update)]
pub enum DefDiagnostic {
    /// A `mod foo;` whose file was not found in the crate.
    UnresolvedModule { name: String },
    /// A `use` path that resolved to nothing.
    UnresolvedImport { path: Vec<String> },
    /// A `use` that names a binding not accessible from the importing module.
    PrivateImport { name: String },
    /// Two defs collide on `(name, namespace)` in one module — e.g. a type and
    /// its constructor sharing a name (`struct S = S`).
    DuplicateDef { name: String },
    /// An `impl T { … }` whose owner type `T` did not resolve.
    UnresolvedImplOwner { name: String },
}

/// One module's data: what it is, its parent, the names visible in it (defs +
/// imports), and its segment path from the crate root (for building contained
/// defs' `DefPath`s). Modeled on `resolve.rs::ModuleData`.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct ModuleData<'db> {
    kind: ModuleKind<'db>,
    parent: Option<ModuleId>,
    /// Names in this module, keyed by `(name, namespace)`.
    items: HashMap<(String, Namespace), Binding<'db>>,
    /// The module names from the crate root *to* this module (exclusive of its
    /// own defs). Empty at the root.
    path_prefix: Vec<String>,
}

impl<'db> ModuleData<'db> {
    pub fn kind(&self) -> ModuleKind<'db> {
        self.kind
    }

    pub fn parent(&self) -> Option<ModuleId> {
        self.parent
    }

    /// Iterate this module's `(name, namespace) → Binding` entries.
    pub fn items(&self) -> impl Iterator<Item = (&(String, Namespace), &Binding<'db>)> {
        self.items.iter()
    }
}

/// Per-def metadata, recoverable from a `DefId` alone (the way rustc exposes
/// `tcx.def_kind`).
//
// No `Debug`: holds a `DefId` (`owner`), which has no std `Debug`.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct DefData<'db> {
    pub kind: DefKind,
    pub name: String,
    /// The module this def is declared in.
    pub module: ModuleId,
    /// The def's own accessibility scope.
    pub visibility: Visibility,
    /// The type a `Ctor`/`Method` belongs to (`None` for everything else).
    pub owner: Option<DefId<'db>>,
}

/// The crate's name-resolution map: the module tree, per-def metadata, and the
/// diagnostics produced while resolving it. The return value of [`crate_def_map`].
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct CrateDefMap<'db> {
    modules: Vec<ModuleData<'db>>,
    root: ModuleId,
    defs: HashMap<DefId<'db>, DefData<'db>>,
    def_to_module: HashMap<DefId<'db>, ModuleId>,
    /// The impl-method index: `(owner type, method name) → method def`. Built
    /// from `impl T { … }` blocks; type-directed dispatch (Q3 `infer`) reads it.
    impl_methods: HashMap<(DefId<'db>, String), DefId<'db>>,
    /// Stable `DefId → DefPath`/`DefPathHash` identity, plus the reverse index.
    def_paths: HashMap<DefId<'db>, DefPath>,
    def_path_hashes: HashMap<DefId<'db>, DefPathHash>,
    hash_to_def: HashMap<DefPathHash, DefId<'db>>,
    diagnostics: Vec<DefDiagnostic>,
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

    pub fn diagnostics(&self) -> &[DefDiagnostic] {
        &self.diagnostics
    }

    /// The method `name` on owner type `owner`, via the impl-method index.
    pub fn impl_method(&self, owner: DefId<'db>, name: &str) -> Option<DefId<'db>> {
        self.impl_methods.get(&(owner, name.to_owned())).copied()
    }

    /// A def's stable `DefPath`.
    pub fn def_path(&self, def: DefId<'db>) -> Option<&DefPath> {
        self.def_paths.get(&def)
    }

    /// A def's stable `DefPathHash` (the cross-session / cross-crate id).
    pub fn def_path_hash(&self, def: DefId<'db>) -> Option<DefPathHash> {
        self.def_path_hashes.get(&def).copied()
    }

    /// The def a `DefPathHash` denotes (for loading cached/external data).
    pub fn def_for_hash(&self, hash: DefPathHash) -> Option<DefId<'db>> {
        self.hash_to_def.get(&hash).copied()
    }

    /// The module a named-module def opens, if any.
    pub fn module_of_def(&self, def: DefId<'db>) -> Option<ModuleId> {
        self.def_to_module.get(&def).copied()
    }

    /// `true` if `module` is `ancestor` or a descendant of it.
    pub fn is_within(&self, module: ModuleId, ancestor: ModuleId) -> bool {
        let mut cur = Some(module);
        while let Some(m) = cur {
            if m == ancestor {
                return true;
            }
            cur = self.modules[m.0 as usize].parent;
        }
        false
    }

    /// The full binding for a name in one module's table.
    pub fn binding_local(
        &self,
        module: ModuleId,
        name: &str,
        ns: Namespace,
    ) -> Option<Binding<'db>> {
        self.modules[module.0 as usize]
            .items
            .get(&(name.to_owned(), ns))
            .copied()
    }

    /// Resolve a bare name in one module's table (defs + imports). No
    /// prelude/ancestor fallback yet — that lands with body resolution (Q3).
    pub fn resolve_local(&self, module: ModuleId, name: &str, ns: Namespace) -> Option<DefId<'db>> {
        self.binding_local(module, name, ns).map(|b| b.def)
    }
}

/// QUERY: the crate's name-resolution map, built from the root file's
/// `item_tree` and the file modules it pulls in (`mod foo;`).
#[salsa::tracked(returns(ref))]
pub fn crate_def_map<'db>(db: &'db dyn salsa::Database, krate: SourceRoot) -> CrateDefMap<'db> {
    let mut collector = Collector::new(db, krate);
    let root_module = collector.new_module(ModuleKind::Root, None, Vec::new());
    debug_assert_eq!(root_module, collector.map.root);
    let root = krate.root_file(db);
    let tree = item_tree(db, root);
    // File modules declared at the crate root resolve next to the root file.
    let root_dir = root
        .path(db)
        .parent()
        .map(Path::to_owned)
        .unwrap_or_default();
    // Phase 1: module + def tree (names, modules, `use`s + impls recorded).
    collector.collect_items(&tree.top_level, root, root_module, &root_dir);
    // Phase 1.5: refine `pub(in path)` visibilities now the whole tree exists.
    collector.resolve_pending_visibilities();
    // Phase 2: resolve `use` imports to a fixpoint.
    collector.resolve_imports();
    // Phase 3: resolve impl owners and build the impl-method index.
    collector.resolve_impls();
    // Phase 4: privacy + unresolved-import diagnostics over every `use` leaf.
    collector.check_uses();
    // Phase 5: build the stable DefPath / DefPathHash table over every def.
    collector.build_def_paths();
    collector.map
}

struct Collector<'db> {
    db: &'db dyn salsa::Database,
    map: CrateDefMap<'db>,
    /// Path → file, for resolving `mod foo;` to another file in the crate.
    files: HashMap<PathBuf, SourceFile>,
    /// `(module, use-tree)` for every `use`, recorded during collection and
    /// consumed by the import fixpoint and the privacy check.
    uses: Vec<(ModuleId, UseTree)>,
    /// `pub(in path)` defs to re-resolve once the whole tree is built:
    /// `(module, name, ns, path)`.
    pending_vis: Vec<(ModuleId, String, Namespace, Vec<String>)>,
    /// `(module, file, impl-item)` for every `impl` block, resolved after the
    /// module tree + imports are built (the owner type may be defined later or
    /// imported).
    impls: Vec<(ModuleId, SourceFile, ImplItem)>,
    /// Defs in declaration (source) order — a deterministic sequence for
    /// `DefPath` disambiguation, since the `defs` map's iteration order is not.
    def_order: Vec<DefId<'db>>,
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
                def_to_module: HashMap::new(),
                impl_methods: HashMap::new(),
                def_paths: HashMap::new(),
                def_path_hashes: HashMap::new(),
                hash_to_def: HashMap::new(),
                diagnostics: Vec::new(),
            },
            files,
            uses: Vec::new(),
            pending_vis: Vec::new(),
            impls: Vec::new(),
            def_order: Vec::new(),
        }
    }

    fn new_module(
        &mut self,
        kind: ModuleKind<'db>,
        parent: Option<ModuleId>,
        path_prefix: Vec<String>,
    ) -> ModuleId {
        let id = ModuleId(self.map.modules.len() as u32);
        self.map.modules.push(ModuleData {
            kind,
            parent,
            items: HashMap::new(),
            path_prefix,
        });
        if let ModuleKind::Named(def) = kind {
            self.map.def_to_module.insert(def, id);
        }
        id
    }

    /// Collect the items declared in one module. `file` is the file they live in;
    /// `dir` is the directory in which a `mod foo;` among them resolves to
    /// `dir/foo.plr`.
    fn collect_items(&mut self, items: &[Item], file: SourceFile, module: ModuleId, dir: &Path) {
        for item in items {
            match item {
                Item::Fn(f) => {
                    self.declare(
                        file,
                        f.ast_id,
                        DefRole::Item,
                        &f.name,
                        DefKind::Fn,
                        module,
                        &f.visibility,
                        None,
                    );
                }
                Item::Struct(s) => self.declare_adt(file, s, DefKind::Struct, module),
                Item::Port(p) => self.declare_adt(file, p, DefKind::Port, module),
                Item::Mod(m) => self.collect_mod(m, file, module, dir),
                Item::Use(u) => self.uses.push((module, u.tree.clone())),
                Item::Impl(i) => self.impls.push((module, file, i.clone())),
            }
        }
    }

    /// Declare a struct/port: the type def plus its term-level constructor (a
    /// distinct `DefKind::Ctor` sharing the type's `FileAstId` via `DefRole::Ctor`,
    /// owned by the type). Both names land in the `Item` namespace, so a type and
    /// a constructor that share a name (`struct S = S`) collide.
    fn declare_adt(
        &mut self,
        file: SourceFile,
        item: &crate::item_tree::NamedItem,
        kind: DefKind,
        module: ModuleId,
    ) {
        let ty = self.declare(
            file,
            item.ast_id,
            DefRole::Item,
            &item.name,
            kind,
            module,
            &item.visibility,
            None,
        );
        // The constructor is as visible as its type, and owned by it.
        self.declare(
            file,
            item.ast_id,
            DefRole::Ctor,
            &item.constructor,
            DefKind::Ctor,
            module,
            &item.visibility,
            Some(ty),
        );
    }

    /// Mint a `DefId` for a named item and enter it into its module's name table.
    /// A clash on `(name, namespace)` is a `DuplicateDef` (the first binding
    /// wins; the def itself is still recorded so callers can find it).
    #[allow(clippy::too_many_arguments)]
    fn declare(
        &mut self,
        file: SourceFile,
        ast_id: crate::ast_id::FileAstId,
        role: DefRole,
        name: &str,
        kind: DefKind,
        module: ModuleId,
        surface_vis: &SurfaceVisibility,
        owner: Option<DefId<'db>>,
    ) -> DefId<'db> {
        let def = DefId::new(self.db, file, ast_id, role);
        let ns = kind.namespace();
        // `pub(in path)` may name a module declared later, so resolve it in a
        // later pass; everything else is fixed now.
        let vis = match surface_vis {
            SurfaceVisibility::Restricted(path) => {
                self.pending_vis
                    .push((module, name.to_owned(), ns, path.clone()));
                Visibility::Restricted(module)
            }
            _ => self.resolve_visibility(surface_vis, module),
        };
        self.map.defs.insert(
            def,
            DefData {
                kind,
                name: name.to_owned(),
                module,
                visibility: vis,
                owner,
            },
        );
        self.def_order.push(def);
        // First binding wins; a clash on `(name, ns)` is a DuplicateDef.
        let duplicate = match self.map.modules[module.0 as usize]
            .items
            .entry((name.to_owned(), ns))
        {
            std::collections::hash_map::Entry::Occupied(_) => true,
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Binding {
                    def,
                    source: BindingSource::Def,
                    vis,
                });
                false
            }
        };
        if duplicate {
            self.map.diagnostics.push(DefDiagnostic::DuplicateDef {
                name: name.to_owned(),
            });
        }
        def
    }

    /// Collect a `mod m`. Its declaration is recorded in `parent`; its body comes
    /// either from the inline `{ … }` (same file) or, for `mod m;`, from the file
    /// `dir/m.plr`. Either way the module's *own* children resolve their file
    /// modules in `dir/m` — each level owns a deeper directory, so the file tree
    /// strictly deepens and cannot cycle.
    fn collect_mod(&mut self, m: &ModItem, file: SourceFile, parent: ModuleId, dir: &Path) {
        let def = self.declare(
            file,
            m.ast_id,
            DefRole::Item,
            &m.name,
            DefKind::Mod,
            parent,
            &m.visibility,
            None,
        );
        let mut prefix = self.map.modules[parent.0 as usize].path_prefix.clone();
        prefix.push(m.name.clone());
        let sub = self.new_module(ModuleKind::Named(def), Some(parent), prefix);
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
                } else {
                    self.map.diagnostics.push(DefDiagnostic::UnresolvedModule {
                        name: m.name.clone(),
                    });
                }
            }
        }
    }

    // ----- visibility -----

    /// Resolve a surface visibility to an accessibility scope, relative to the
    /// module the item is declared in.
    fn resolve_visibility(&self, vis: &SurfaceVisibility, module: ModuleId) -> Visibility {
        match vis {
            SurfaceVisibility::Inherited => Visibility::Restricted(module),
            SurfaceVisibility::Public => Visibility::Public,
            SurfaceVisibility::Crate => Visibility::Crate,
            SurfaceVisibility::Super => {
                let parent = self.map.modules[module.0 as usize].parent.unwrap_or(module);
                Visibility::Restricted(parent)
            }
            SurfaceVisibility::Restricted(path) => {
                let segs: Vec<&str> = path.iter().map(String::as_str).collect();
                match self.resolve_path_to_module(&segs, module) {
                    Some(m) => Visibility::Restricted(m),
                    None => Visibility::Restricted(module),
                }
            }
        }
    }

    /// Phase 1.5: re-resolve the `pub(in path)` defs deferred during collection,
    /// updating both the def's metadata and its module-table binding.
    fn resolve_pending_visibilities(&mut self) {
        let pending = std::mem::take(&mut self.pending_vis);
        for (module, name, ns, path) in pending {
            let segs: Vec<&str> = path.iter().map(String::as_str).collect();
            let vis = match self.resolve_path_to_module(&segs, module) {
                Some(m) => Visibility::Restricted(m),
                None => Visibility::Restricted(module),
            };
            if let Some(binding) = self.map.modules[module.0 as usize]
                .items
                .get_mut(&(name.clone(), ns))
            {
                binding.vis = vis;
                let def = binding.def;
                if let Some(data) = self.map.defs.get_mut(&def) {
                    data.visibility = vis;
                }
            }
        }
    }

    // ----- imports (fixpoint) -----

    /// Resolve every `use` to a fixpoint — explicit imports and globs converge
    /// in a few passes because a glob's imported set, and any chained import, can
    /// grow as other imports land. Polar has no macros, so this is the only
    /// fixpoint resolution needs.
    fn resolve_imports(&mut self) {
        let uses = std::mem::take(&mut self.uses);
        loop {
            let mut changed = false;
            for (module, tree) in &uses {
                let vis = Visibility::Restricted(*module); // plain `use`: module-private
                changed |= self.import_tree(*module, &[], tree, vis);
            }
            if !changed {
                break;
            }
        }
        self.uses = uses;
    }

    /// Apply one use-tree to `module` under `prefix`. Returns whether any new
    /// binding was inserted.
    fn import_tree(
        &mut self,
        module: ModuleId,
        prefix: &[String],
        tree: &UseTree,
        vis: Visibility,
    ) -> bool {
        match tree {
            UseTree::Path { segments, alias } => {
                let full: Vec<&str> = prefix.iter().chain(segments).map(String::as_str).collect();
                self.import_leaf(module, &full, alias.as_deref(), vis)
            }
            UseTree::Group {
                prefix: gp,
                children,
            } => {
                let new_prefix: Vec<String> = prefix.iter().chain(gp).cloned().collect();
                let mut changed = false;
                for child in children {
                    changed |= self.import_tree(module, &new_prefix, child, vis);
                }
                changed
            }
            UseTree::Glob { prefix: gp } => {
                let full: Vec<&str> = prefix.iter().chain(gp).map(String::as_str).collect();
                self.import_glob(module, &full, vis)
            }
        }
    }

    /// Import a single leaf `prefix::…::name [as alias]` into `module`.
    fn import_leaf(
        &mut self,
        module: ModuleId,
        segments: &[&str],
        alias: Option<&str>,
        vis: Visibility,
    ) -> bool {
        let Some((&last, prefix)) = segments.split_last() else {
            return false;
        };
        // `use a::self` — import the module `a` itself.
        if last == "self" {
            let Some((&modname, _)) = prefix.split_last() else {
                return false;
            };
            let Some(def) = self.resolve_path(prefix, module, Namespace::Module) else {
                return false;
            };
            let name = alias.unwrap_or(modname);
            return self.import_binding(module, name, Namespace::Module, def, vis);
        }
        let name = alias.unwrap_or(last);
        let mut changed = false;
        // Import the name in whichever namespace(s) it resolves to.
        for ns in [Namespace::Module, Namespace::Item] {
            if let Some(def) = self.resolve_path(segments, module, ns) {
                changed |= self.import_binding(module, name, ns, def, vis);
            }
        }
        changed
    }

    /// Import every accessible name from the module named by `prefix`.
    fn import_glob(&mut self, module: ModuleId, prefix: &[&str], vis: Visibility) -> bool {
        let Some(target) = self.resolve_path_to_module(prefix, module) else {
            return false;
        };
        if target == module {
            return false;
        }
        // Snapshot the target's accessible entries (avoid borrowing while
        // mutating). A glob imports only names visible from `module`.
        let entries: Vec<(String, Namespace, DefId<'db>)> = self.map.modules[target.0 as usize]
            .items
            .iter()
            .filter(|(_, b)| self.vis_accessible(b.vis, module))
            .map(|((name, ns), b)| (name.clone(), *ns, b.def))
            .collect();
        let mut changed = false;
        for (name, ns, def) in entries {
            changed |= self.import_glob_binding(module, &name, ns, def, vis);
        }
        changed
    }

    /// Insert an explicit-import binding, respecting priority (`Def > Import`).
    fn import_binding(
        &mut self,
        module: ModuleId,
        name: &str,
        ns: Namespace,
        def: DefId<'db>,
        vis: Visibility,
    ) -> bool {
        self.insert_binding(module, name, ns, def, BindingSource::Import, vis)
    }

    /// Insert a glob-import binding (lowest priority).
    fn import_glob_binding(
        &mut self,
        module: ModuleId,
        name: &str,
        ns: Namespace,
        def: DefId<'db>,
        vis: Visibility,
    ) -> bool {
        self.insert_binding(module, name, ns, def, BindingSource::Glob, vis)
    }

    /// Insert a binding respecting priority `Def > Import > Glob`. Returns
    /// whether the table changed (drives the fixpoint).
    fn insert_binding(
        &mut self,
        module: ModuleId,
        name: &str,
        ns: Namespace,
        def: DefId<'db>,
        source: BindingSource,
        vis: Visibility,
    ) -> bool {
        let table = &mut self.map.modules[module.0 as usize].items;
        let key = (name.to_owned(), ns);
        match table.get(&key) {
            None => {
                table.insert(key, Binding { def, source, vis });
                true
            }
            Some(existing) => match (existing.source, source) {
                // A local def always wins.
                (BindingSource::Def, _) => false,
                // An explicit import overrides a glob.
                (BindingSource::Glob, BindingSource::Import) => {
                    table.insert(key, Binding { def, source, vis });
                    true
                }
                // Otherwise keep the existing (idempotent / lenient on conflicts).
                _ => false,
            },
        }
    }

    // ----- path resolution -----

    /// Resolve `crate`/`super`/`self` anchors. Returns the module the first
    /// *named* segment is looked up in and that segment's index.
    fn path_anchor(&self, segments: &[&str], from: ModuleId) -> (ModuleId, usize) {
        match segments.first().copied() {
            Some("crate") => (self.map.root, 1),
            Some("self") => (from, 1),
            Some("super") => {
                let mut module = from;
                let mut i = 0;
                while segments.get(i).copied() == Some("super") {
                    module = self.map.modules[module.0 as usize].parent.unwrap_or(module);
                    i += 1;
                }
                (module, i)
            }
            _ => (from, 0),
        }
    }

    /// Resolve a path's final segment to a def in `final_ns`. Intermediate
    /// segments must be modules (the `{Module, Item}` split makes this
    /// unambiguous — they always resolve in the `Module` namespace).
    fn resolve_path(
        &self,
        segments: &[&str],
        from: ModuleId,
        final_ns: Namespace,
    ) -> Option<DefId<'db>> {
        let (mut module, start) = self.path_anchor(segments, from);
        if start >= segments.len() {
            return None;
        }
        let mut i = start;
        while i + 1 < segments.len() {
            let def = self
                .map
                .resolve_local(module, segments[i], Namespace::Module)?;
            if self.map.def_data(def).map(|d| d.kind) != Some(DefKind::Mod) {
                return None;
            }
            module = self.map.module_of_def(def)?;
            i += 1;
        }
        self.map.resolve_local(module, segments[i], final_ns)
    }

    /// Resolve a path whose final segment must itself be a module.
    fn resolve_path_to_module(&self, segments: &[&str], from: ModuleId) -> Option<ModuleId> {
        if segments.is_empty() {
            // Empty prefix (`{ … }` / `*` at the current module).
            return Some(from);
        }
        let def = self.resolve_path(segments, from, Namespace::Module)?;
        if self.map.def_data(def).map(|d| d.kind) != Some(DefKind::Mod) {
            return None;
        }
        self.map.module_of_def(def)
    }

    // ----- privacy -----

    /// Is a binding of visibility `vis` nameable from `use_module`?
    fn vis_accessible(&self, vis: Visibility, use_module: ModuleId) -> bool {
        match vis {
            Visibility::Public | Visibility::Crate => true,
            Visibility::Restricted(scope) => self.map.is_within(use_module, scope),
        }
    }

    /// Phase 3: over every `use` leaf, emit an unresolved-import or private-import
    /// diagnostic. Mirrors `resolve.rs::check_use_privacy`.
    fn check_uses(&mut self) {
        let uses = std::mem::take(&mut self.uses);
        let mut leaves: Vec<(ModuleId, Vec<String>)> = Vec::new();
        for (module, tree) in &uses {
            use_leaves(tree, &[], &mut |segs| leaves.push((*module, segs)));
        }
        for (module, mut segs) in leaves {
            // A trailing `self` names the prefix module.
            if segs.last().map(String::as_str) == Some("self") {
                segs.pop();
            }
            if segs.is_empty() {
                continue;
            }
            let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
            self.check_path_access(&refs, module);
        }
        self.uses = uses;
    }

    /// Walk a path's bindings (intermediate modules, then the final name) from
    /// `from`, recording the first failure as a diagnostic.
    fn check_path_access(&mut self, segments: &[&str], from: ModuleId) {
        let (mut module, start) = self.path_anchor(segments, from);
        if start >= segments.len() {
            return;
        }
        let mut i = start;
        while i + 1 < segments.len() {
            // The relative first segment resolves in `from`'s own scope, always
            // accessible; later segments are checked for visibility.
            let own_scope = i == start && start == 0;
            let Some(binding) = self
                .map
                .binding_local(module, segments[i], Namespace::Module)
            else {
                self.map.diagnostics.push(DefDiagnostic::UnresolvedImport {
                    path: segments.iter().map(|s| s.to_string()).collect(),
                });
                return;
            };
            if !own_scope && !self.vis_accessible(binding.vis, from) {
                self.push_private(binding.def);
                return;
            }
            let Some(next) = self.map.module_of_def(binding.def) else {
                return;
            };
            module = next;
            i += 1;
        }
        // Final segment — in whichever namespace it resolves.
        let own_scope = i == start && start == 0;
        for ns in [Namespace::Module, Namespace::Item] {
            if let Some(binding) = self.map.binding_local(module, segments[i], ns) {
                if !own_scope && !self.vis_accessible(binding.vis, from) {
                    self.push_private(binding.def);
                }
                return;
            }
        }
        self.map.diagnostics.push(DefDiagnostic::UnresolvedImport {
            path: segments.iter().map(|s| s.to_string()).collect(),
        });
    }

    fn push_private(&mut self, def: DefId<'db>) {
        if let Some(data) = self.map.def_data(def) {
            self.map.diagnostics.push(DefDiagnostic::PrivateImport {
                name: data.name.clone(),
            });
        }
    }

    // ----- impl-method index -----

    /// Resolve each `impl T { … }`: look up the owner type `T` in the impl's
    /// module (Item ns; runs after imports so an imported owner resolves), then
    /// mint a `Method` def per method and index it under `(owner, method name)`.
    fn resolve_impls(&mut self) {
        let impls = std::mem::take(&mut self.impls);
        for (module, file, item) in impls {
            let owner = self.map.resolve_local(module, &item.owner, Namespace::Item);
            let owner = match owner.and_then(|o| self.map.def_data(o).map(|d| (o, d.kind))) {
                Some((o, DefKind::Struct | DefKind::Port)) => o,
                _ => {
                    self.map
                        .diagnostics
                        .push(DefDiagnostic::UnresolvedImplOwner {
                            name: item.owner.clone(),
                        });
                    continue;
                }
            };
            for method in &item.methods {
                let def = DefId::new(self.db, file, method.ast_id, DefRole::Item);
                let vis = self.resolve_visibility(&method.visibility, module);
                self.map.defs.insert(
                    def,
                    DefData {
                        kind: DefKind::Method,
                        name: method.name.clone(),
                        module,
                        visibility: vis,
                        owner: Some(owner),
                    },
                );
                self.def_order.push(def);
                // Last writer wins on a duplicate method name (lenient; the
                // duplicate-method diagnostic lands with the error surface, Q6).
                self.map
                    .impl_methods
                    .insert((owner, method.name.clone()), def);
            }
        }
    }

    // ----- stable identity -----

    /// Build each def's `DefPath` (its module prefix plus its own name) and
    /// `DefPathHash`, disambiguating defs that would otherwise share a path.
    /// Iterates defs in source order so disambiguators are deterministic.
    /// Mirrors `resolve.rs::build_def_path_table`.
    ///
    /// Note: a method's path is `module::method` (the owner is not yet a path
    /// component), so sibling impls with same-named methods are separated only by
    /// the disambiguator — fine for identity, to be refined when owners join the
    /// path.
    fn build_def_paths(&mut self) {
        let scid = StableCrateId::root();
        let mut disambig: HashMap<Vec<String>, u32> = HashMap::new();
        let order = std::mem::take(&mut self.def_order);
        for def in order {
            let Some(data) = self.map.defs.get(&def) else {
                continue;
            };
            let mut names = self.map.modules[data.module.0 as usize].path_prefix.clone();
            names.push(data.name.clone());
            let disamb = {
                let counter = disambig.entry(names.clone()).or_insert(0);
                let d = *counter;
                *counter += 1;
                d
            };
            let mut segments: Vec<DefPathSegment> = names
                .into_iter()
                .map(|n| DefPathSegment {
                    kind: DefPathSegmentKind::Named(n),
                    disambiguator: 0,
                })
                .collect();
            if let Some(last) = segments.last_mut() {
                last.disambiguator = disamb;
            }
            let path = DefPath { segments };
            let hash = DefPathHash::new(scid, &path);
            self.map.def_paths.insert(def, path);
            self.map.def_path_hashes.insert(def, hash);
            self.map.hash_to_def.insert(hash, def);
        }
    }
}

/// Enumerate the leaf paths of a use-tree (each a full segment list), invoking
/// `f` per leaf. A glob contributes its prefix (the module to check).
fn use_leaves(tree: &UseTree, prefix: &[String], f: &mut impl FnMut(Vec<String>)) {
    match tree {
        UseTree::Path { segments, .. } => {
            let mut p = prefix.to_vec();
            p.extend(segments.iter().cloned());
            f(p);
        }
        UseTree::Group {
            prefix: gp,
            children,
        } => {
            let mut p = prefix.to_vec();
            p.extend(gp.iter().cloned());
            for child in children {
                use_leaves(child, &p, f);
            }
        }
        UseTree::Glob { prefix: gp } => {
            let mut p = prefix.to_vec();
            p.extend(gp.iter().cloned());
            f(p);
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
struct S = s { a: uint(8) }
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
struct S = s { a: uint(8) }
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

    // ----- Q2c: `use` imports + privacy -----

    #[test]
    fn use_imports_a_name_from_a_submodule() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod m { pub fn f () -> uint(8) { return 0; } }\nuse m::f;",
        );
        let map = crate_def_map(&db, krate);
        assert!(
            map.resolve_local(map.root(), "f", Namespace::Item)
                .is_some()
        );
        assert!(map.diagnostics().is_empty(), "{:?}", map.diagnostics());
    }

    #[test]
    fn use_glob_imports_only_accessible_names() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod m {\n  pub fn f () -> uint(8) { return 0; }\n  pub fn g () -> uint(8) { return 0; }\n  fn hidden () -> uint(8) { return 0; }\n}\nuse m::*;",
        );
        let map = crate_def_map(&db, krate);
        let root = map.root();
        assert!(map.resolve_local(root, "f", Namespace::Item).is_some());
        assert!(map.resolve_local(root, "g", Namespace::Item).is_some());
        // `hidden` is private to `m`, so the glob does not bring it in.
        assert!(map.resolve_local(root, "hidden", Namespace::Item).is_none());
    }

    #[test]
    fn use_group_and_alias() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod m {\n  pub fn f () -> uint(8) { return 0; }\n  pub fn g () -> uint(8) { return 0; }\n}\nuse m::{f, g as h};",
        );
        let map = crate_def_map(&db, krate);
        let root = map.root();
        assert!(map.resolve_local(root, "f", Namespace::Item).is_some());
        assert!(map.resolve_local(root, "h", Namespace::Item).is_some());
        // The alias renames: `g` is not bound, only `h`.
        assert!(map.resolve_local(root, "g", Namespace::Item).is_none());
    }

    #[test]
    fn use_imports_a_submodule_through_a_chained_path() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod a { pub mod b { pub fn f () -> uint(8) { return 0; } } }\nuse a::b;",
        );
        let map = crate_def_map(&db, krate);
        // `b` is imported into the root in the Module namespace.
        assert!(
            map.resolve_local(map.root(), "b", Namespace::Module)
                .is_some()
        );
        assert!(map.diagnostics().is_empty(), "{:?}", map.diagnostics());
    }

    #[test]
    fn a_local_def_beats_an_import() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod m { pub fn f () -> uint(8) { return 0; } }\nuse m::f;\nfn f () -> uint(8) { return 1; }",
        );
        let map = crate_def_map(&db, krate);
        let binding = map
            .binding_local(map.root(), "f", Namespace::Item)
            .expect("f bound");
        assert_eq!(
            binding.source,
            BindingSource::Def,
            "the local `f` must win over the import"
        );
    }

    #[test]
    fn importing_a_private_name_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod m { fn secret () -> uint(8) { return 0; } }\nuse m::secret;",
        );
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics()
                .iter()
                .any(|d| matches!(d, DefDiagnostic::PrivateImport { name } if name == "secret")),
            "{:?}",
            map.diagnostics()
        );
    }

    #[test]
    fn pub_crate_is_reachable_from_another_subtree() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod a { pub(crate) fn f () -> uint(8) { return 0; } }\nmod b { use crate::a::f; }",
        );
        let map = crate_def_map(&db, krate);
        let b = named_module(map, "b", Namespace::Module).expect("module b");
        assert!(map.resolve_local(b, "f", Namespace::Item).is_some());
        assert!(map.diagnostics().is_empty(), "{:?}", map.diagnostics());
    }

    #[test]
    fn an_unresolved_import_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "t.plr", "use nonexistent::thing;");
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics()
                .iter()
                .any(|d| matches!(d, DefDiagnostic::UnresolvedImport { .. })),
            "{:?}",
            map.diagnostics()
        );
    }

    // ----- Q2d: constructors, impl-method index, DefPath -----

    #[test]
    fn struct_mints_a_distinct_owned_constructor() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "struct Bus = bus { a: uint(8) }",
        );
        let map = crate_def_map(&db, krate);
        let root = map.root();
        let ty = map
            .resolve_local(root, "Bus", Namespace::Item)
            .expect("type");
        let ctor = map
            .resolve_local(root, "bus", Namespace::Item)
            .expect("ctor");
        assert!(ty != ctor, "ctor is a distinct def");
        assert_eq!(map.def_data(ty).unwrap().kind, DefKind::Struct);
        assert_eq!(map.def_data(ctor).unwrap().kind, DefKind::Ctor);
        assert!(map.def_data(ctor).unwrap().owner == Some(ty));
        assert!(map.diagnostics().is_empty(), "{:?}", map.diagnostics());
    }

    #[test]
    fn struct_with_type_named_constructor_collides() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "t.plr", "struct S = S { a: uint(8) }");
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics()
                .iter()
                .any(|d| matches!(d, DefDiagnostic::DuplicateDef { name } if name == "S")),
            "{:?}",
            map.diagnostics()
        );
    }

    #[test]
    fn impl_methods_are_indexed_by_owner() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "struct Widget = w { a: uint(8) }\nimpl Widget { fn m (self) -> uint(8) { return 0; } }",
        );
        let map = crate_def_map(&db, krate);
        let widget = map
            .resolve_local(map.root(), "Widget", Namespace::Item)
            .unwrap();
        let m = map.impl_method(widget, "m").expect("method m");
        assert_eq!(map.def_data(m).unwrap().kind, DefKind::Method);
        assert!(map.def_data(m).unwrap().owner == Some(widget));
        // A method is not a module-table name.
        assert!(
            map.resolve_local(map.root(), "m", Namespace::Item)
                .is_none()
        );
    }

    #[test]
    fn impl_on_an_unknown_owner_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "impl Ghost { fn m (self) -> uint(8) { return 0; } }",
        );
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics().iter().any(
                |d| matches!(d, DefDiagnostic::UnresolvedImplOwner { name } if name == "Ghost")
            ),
            "{:?}",
            map.diagnostics()
        );
    }

    #[test]
    fn def_path_is_module_qualified() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "mod m { pub fn f () -> uint(8) { return 0; } }",
        );
        let map = crate_def_map(&db, krate);
        let mmod = named_module(map, "m", Namespace::Module).unwrap();
        let f = map.resolve_local(mmod, "f", Namespace::Item).unwrap();
        let path = map.def_path(f).expect("def path");
        let names: Vec<&str> = path
            .segments
            .iter()
            .map(|s| match &s.kind {
                DefPathSegmentKind::Named(n) => n.as_str(),
                DefPathSegmentKind::AnonConst(_) => "<anon>",
            })
            .collect();
        assert_eq!(names, ["m", "f"]);
        assert!(map.def_path_hash(f).is_some());
    }

    #[test]
    fn def_path_hash_is_stable_across_a_body_edit() {
        // The identity payoff: a body edit leaves the DefPathHash unchanged.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "fn f () -> uint(8) { return 0; }",
        );
        let before = {
            let map = crate_def_map(&db, krate);
            let f = map.resolve_local(map.root(), "f", Namespace::Item).unwrap();
            map.def_path_hash(f).unwrap()
        };
        vfs.set_file_text(&mut db, "t.plr", "fn f () -> uint(8) { return 0 + 1 + 2; }");
        let after = {
            let map = crate_def_map(&db, krate);
            let f = map.resolve_local(map.root(), "f", Namespace::Item).unwrap();
            map.def_path_hash(f).unwrap()
        };
        assert_eq!(before, after);
    }

    #[test]
    fn same_named_sibling_methods_get_distinct_path_hashes() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.plr",
            "struct A = a { x: uint(8) }\nstruct B = b { x: uint(8) }\nimpl A { fn m (self) -> uint(8) { return 0; } }\nimpl B { fn m (self) -> uint(8) { return 0; } }",
        );
        let map = crate_def_map(&db, krate);
        let a = map.resolve_local(map.root(), "A", Namespace::Item).unwrap();
        let b = map.resolve_local(map.root(), "B", Namespace::Item).unwrap();
        let ma = map.impl_method(a, "m").unwrap();
        let mb = map.impl_method(b, "m").unwrap();
        assert_ne!(
            map.def_path_hash(ma).unwrap(),
            map.def_path_hash(mb).unwrap(),
            "the disambiguator must separate same-named sibling methods"
        );
    }
}
