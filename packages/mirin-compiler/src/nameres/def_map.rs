//! `crate_def_map` — name resolution's firewall #2 (`planning/query_engine.md`
//! §3.1).
//!
//! Builds the crate's **module tree** and **name tables** from the per-file
//! [`item_tree`](crate::syntax::item_tree)s. Depends only on item-tree *names and
//! structure*, never on bodies or types, so a body edit cannot reach it: the
//! item_tree firewall absorbs the edit (its value is unchanged), this query
//! backdates, and every dependent survives. This is the boundary that keeps
//! goto-def / privacy / signature resolution cached across body edits.
//!
//! Ports the *name-resolution half* of `mirin-compiler`'s `resolve.rs`
//! (`collect_items` → the module + def tree). The body-resolution half
//! (`resolve_items`) is deliberately **not** here — it lands in Q3 behind the
//! `sig_of`/`body` split. The whole local repo is one crate (§3.5); this query
//! is keyed on the crate's [`SourceRoot`](crate::base::db::SourceRoot) (root file +
//! file set), which is what lets it resolve `mod foo;` to another file.
//!
//! **Scope so far:** the module tree — root, inline `mod`, and `mod foo;` file
//! modules (Q2b); name tables in the `{Module, Item}` namespaces with
//! constructors (`struct Bus = bus`) and the `struct S = S` collision check;
//! `use` imports to a fixpoint with privacy (Q2c); the impl-method index and the
//! stable `DefPath`/`DefPathHash` table (Q2d); the synthetic **prelude** module
//! plus `resolve_in_scope` (own table → prelude), the in-scope lookup body
//! resolution uses (Q3a). Still to come: `sig_of`/`body`/`infer` (Q3b–d).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::base::db::{SourceFile, SourceRoot};
use crate::nameres::ids::{
    DefId, DefKind, DefPath, DefPathHash, DefPathSegment, DefPathSegmentKind, DefRole, Namespace,
    StableCrateId,
};
use crate::syntax::ast_id::FileAstId;
use crate::syntax::item_tree::{
    ImplItem, Item, ModItem, ModKind, UseTree, Visibility as SurfaceVisibility, item_tree,
};

/// The language builtins seeded into the synthetic prelude module — types and
/// intrinsic fns. Order matters: each entry's index mints its synthetic
/// `FileAstId`, so append-only.
const BUILTINS: &[(&str, DefKind)] = &[
    ("uint", DefKind::BuiltinType),
    ("bool", DefKind::BuiltinType),
    ("Clock", DefKind::BuiltinType),
    ("Event", DefKind::BuiltinType),
    ("Reset", DefKind::BuiltinType),
    ("Type", DefKind::BuiltinType),
    ("integer", DefKind::BuiltinType),
    ("sint", DefKind::BuiltinType),
    ("bits", DefKind::BuiltinType),
    ("Vec", DefKind::BuiltinType),
    ("reg", DefKind::Fn),
    ("posedge", DefKind::Fn),
    ("range", DefKind::Fn),
];

/// The builtin *type* names. Exposed so tooling (the LSP's highlight query, the
/// VS Code TextMate fallback) can be tested against the language's actual
/// builtin set instead of drifting.
pub fn builtin_type_names() -> impl Iterator<Item = &'static str> {
    BUILTINS
        .iter()
        .filter(|(_, kind)| matches!(kind, DefKind::BuiltinType))
        .map(|(name, _)| *name)
}

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
    /// The synthetic prelude — the lowest-priority fallback scope holding the
    /// language builtins. Injected by `crate_def_map`, not from any source.
    Prelude,
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
/// A name-resolution diagnostic, optionally anchored at the offending item
/// (`file` + `ast_id`) so the renderer can resolve its source range — a stable
/// anchor that survives edits to other items (the item-tree firewall). All
/// current producers set an anchor; the `Option` stays for future crate-wide
/// diagnostics with no single item to point at.
// No `Debug`: `SourceFile` (in `anchor`) carries none (its fields need the db),
// like the other input-holding types here.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct DefDiagnostic {
    pub anchor: Option<(SourceFile, FileAstId)>,
    pub kind: DefDiagnosticKind,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::Update)]
pub enum DefDiagnosticKind {
    /// A `mod foo;` whose file was not found in the crate.
    UnresolvedModule {
        name: String,
    },
    /// A `use` path that resolved to nothing.
    UnresolvedImport {
        path: Vec<String>,
    },
    /// A `use` that names a binding not accessible from the importing module.
    PrivateImport {
        name: String,
    },
    /// Two defs collide on `(name, namespace)` in one module — e.g. a type and
    /// its constructor sharing a name (`struct S = S`).
    DuplicateDef {
        name: String,
    },
    /// An `impl T { … }` whose owner type `T` did not resolve.
    UnresolvedImplOwner {
        name: String,
    },
    UnresolvedTrait {
        name: String,
    },
    MissingTraitItem {
        trait_name: String,
        name: String,
    },
    ExtraTraitItem {
        trait_name: String,
        name: String,
    },
    OverlappingImpls {
        trait_name: String,
        ty: String,
    },
}

impl DefDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            DefDiagnosticKind::UnresolvedModule { name } => {
                format!("unresolved module `{name}` (no matching `.mrn` file)")
            }
            DefDiagnosticKind::UnresolvedImport { path } => {
                format!("unresolved import `{}`", path.join("::"))
            }
            DefDiagnosticKind::PrivateImport { name } => format!("`{name}` is private"),
            DefDiagnosticKind::DuplicateDef { name } => {
                format!("the name `{name}` is defined more than once in this module")
            }
            DefDiagnosticKind::UnresolvedTrait { name } => {
                format!("`{name}` is not a trait")
            }
            DefDiagnosticKind::MissingTraitItem { trait_name, name } => {
                format!("missing `{name}` in implementation of `{trait_name}`")
            }
            DefDiagnosticKind::ExtraTraitItem { trait_name, name } => {
                format!("`{name}` is not a member of trait `{trait_name}`")
            }
            DefDiagnosticKind::OverlappingImpls { trait_name, ty } => {
                format!("conflicting implementations of `{trait_name}` for `{ty}`")
            }
            DefDiagnosticKind::UnresolvedImplOwner { name } => {
                format!("cannot find type `{name}` for this `impl`")
            }
        }
    }
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

/// One `impl Trait for SelfType { … }` block, as the def map records it.
/// `self_def` is the implementing type's head def; full self-type matching
/// (generic args) happens in the solver against the lowered impl header.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct TraitImplData<'db> {
    /// The impl block's own def. `sig_of(impl_def)` is the lowered HEADER:
    /// `generic_params` = the impl binder (a generic owner is applied, so its
    /// params are declared there), `return_type` = Some(self type),
    /// `predicates` = the binder's bounds.
    pub impl_def: DefId<'db>,
    pub self_def: DefId<'db>,
    pub self_has_args: bool,
    pub methods: Vec<(String, DefId<'db>)>,
    /// The impl's associated consts: `(name, def)` — `sig_of(def)` lowers
    /// the value into `const_value`.
    pub consts: Vec<(String, DefId<'db>)>,
    pub file: SourceFile,
    pub ast_id: crate::syntax::ast_id::FileAstId,
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
    /// Carries `#[inline]` (planning/attributes.md): the backend splices this
    /// fn/method's body at call sites instead of instantiating a module. Only
    /// ever true for `Fn`/`Method` defs.
    pub inline: bool,
}

/// The crate's name-resolution map: the module tree, per-def metadata, and the
/// diagnostics produced while resolving it. The return value of [`crate_def_map`].
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct CrateDefMap<'db> {
    modules: Vec<ModuleData<'db>>,
    root: ModuleId,
    /// The synthetic prelude module (lowest-priority lookup fallback).
    prelude: ModuleId,
    defs: HashMap<DefId<'db>, DefData<'db>>,
    def_to_module: HashMap<DefId<'db>, ModuleId>,
    /// The impl-method index: `(owner type, method name) → method def`. Built
    /// from `impl T { … }` blocks; type-directed dispatch (Q3 `infer`) reads it.
    impl_methods: HashMap<(DefId<'db>, String), DefId<'db>>,
    /// `(trait def, method name) → method-DECL def` — the trait's own method
    /// signatures.
    trait_methods: HashMap<(DefId<'db>, String), DefId<'db>>,
    /// Per-trait impls (`impl Trait for SelfType`), in source order. The
    /// solver's impl-candidate source (planning/traits.md).
    trait_impls: HashMap<DefId<'db>, Vec<TraitImplData<'db>>>,
    /// `(self-type head def, method name) → [(trait, impl-method def)]` —
    /// method dispatch's trait-candidate index (the self-type-fingerprint
    /// lookup; rust-analyzer's TyFingerprint shape).
    trait_dispatch: HashMap<(DefId<'db>, String), Vec<(DefId<'db>, DefId<'db>)>>,
    /// `(trait def, const name) → const-DECL def`.
    trait_consts: HashMap<(DefId<'db>, String), DefId<'db>>,
    /// trait-impl method → its trait (the backend qualifies module names
    /// with it: `Owner__Trait__method`).
    method_trait: HashMap<DefId<'db>, DefId<'db>>,
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

    /// The synthetic prelude module — the lowest-priority lookup fallback.
    pub fn prelude(&self) -> ModuleId {
        self.prelude
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

    /// Every def in the crate, in no particular order (fns, structs, ports,
    /// ctors, mods, impl methods, prelude builtins).
    pub fn defs(&self) -> impl Iterator<Item = DefId<'db>> + '_ {
        self.defs.keys().copied()
    }

    pub fn diagnostics(&self) -> &[DefDiagnostic] {
        &self.diagnostics
    }

    /// The method `name` on owner type `owner`, via the impl-method index.
    pub fn impl_method(&self, owner: DefId<'db>, name: &str) -> Option<DefId<'db>> {
        self.impl_methods.get(&(owner, name.to_owned())).copied()
    }

    /// A trait's own method declaration, by name.
    pub fn trait_method(&self, trait_def: DefId<'db>, name: &str) -> Option<DefId<'db>> {
        self.trait_methods
            .get(&(trait_def, name.to_owned()))
            .copied()
    }

    /// A trait's associated-const declaration, by name.
    pub fn trait_const(&self, trait_def: DefId<'db>, name: &str) -> Option<DefId<'db>> {
        self.trait_consts
            .get(&(trait_def, name.to_owned()))
            .copied()
    }

    /// Every trait's impl list (the assoc-const evaluator scans for the
    /// impl owning a const def).
    pub fn all_trait_impls(&self) -> impl Iterator<Item = (&DefId<'db>, &Vec<TraitImplData<'db>>)> {
        self.trait_impls.iter()
    }

    /// The `impl Trait for …` blocks for a trait, in source order.
    pub fn trait_impls(&self, trait_def: DefId<'db>) -> &[TraitImplData<'db>] {
        self.trait_impls
            .get(&trait_def)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Trait-impl methods named `name` implemented for the type headed by
    /// `owner`: `[(trait def, impl-method def)]`. Dispatch's trait candidates.
    pub fn trait_dispatch(&self, owner: DefId<'db>, name: &str) -> &[(DefId<'db>, DefId<'db>)] {
        self.trait_dispatch
            .get(&(owner, name.to_owned()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The trait a trait-impl method belongs to (`None` for inherent methods
    /// and trait method DECLS).
    pub fn trait_of_method(&self, method: DefId<'db>) -> Option<DefId<'db>> {
        self.method_trait.get(&method).copied()
    }

    /// Is this def a trait's method DECLARATION (no body)?
    pub fn is_trait_method_decl(&self, def: DefId<'db>) -> bool {
        self.def_data(def)
            .and_then(|d| d.owner)
            .and_then(|o| self.def_data(o))
            .is_some_and(|o| o.kind == DefKind::Trait)
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

    /// Resolve a bare name in one module's own table (defs + imports), with no
    /// fallback. Used for path segments, where each step is explicit.
    pub fn resolve_local(&self, module: ModuleId, name: &str, ns: Namespace) -> Option<DefId<'db>> {
        self.binding_local(module, name, ns).map(|b| b.def)
    }

    /// Resolve a bare name as a body in `module` sees it: the module's own table,
    /// then the prelude (lowest priority, so a user name shadows a builtin). This
    /// is the in-scope lookup body resolution (Q3c) uses. No ancestor walk — a
    /// bare name resolves in its own module or the prelude, matching the old
    /// resolver (`modules.md` §5.2).
    pub fn resolve_in_scope(
        &self,
        module: ModuleId,
        name: &str,
        ns: Namespace,
    ) -> Option<DefId<'db>> {
        self.resolve_local(module, name, ns)
            .or_else(|| self.resolve_local(self.prelude, name, ns))
    }

    /// Resolve `crate`/`super`/`self` anchors at the start of a path. Returns the
    /// module the first *named* segment is looked up in, and that segment's index.
    pub(crate) fn path_anchor(&self, segments: &[&str], from: ModuleId) -> (ModuleId, usize) {
        match segments.first().copied() {
            Some("crate") => (self.root, 1),
            Some("self") => (from, 1),
            Some("super") => {
                let mut module = from;
                let mut i = 0;
                while segments.get(i).copied() == Some("super") {
                    module = self.modules[module.0 as usize].parent.unwrap_or(module);
                    i += 1;
                }
                (module, i)
            }
            _ => (from, 0),
        }
    }

    /// Resolve a multi-segment path's final segment to a def in `final_ns`,
    /// starting from module `from`. Intermediate segments must be modules (the
    /// `{Module, Item}` split makes this unambiguous — they resolve in the
    /// `Module` namespace). Bare names should use [`Self::resolve_in_scope`].
    pub fn resolve_path(
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
            let def = self.resolve_local(module, segments[i], Namespace::Module)?;
            if self.def_data(def).map(|d| d.kind) != Some(DefKind::Mod) {
                return None;
            }
            module = self.module_of_def(def)?;
            i += 1;
        }
        self.resolve_local(module, segments[i], final_ns)
    }
}

/// QUERY: the crate's name-resolution map, built from the root file's
/// `item_tree` and the file modules it pulls in (`mod foo;`).
#[salsa::tracked(returns(ref))]
pub fn crate_def_map<'db>(db: &'db dyn salsa::Database, krate: SourceRoot) -> CrateDefMap<'db> {
    let mut collector = Collector::new(db, krate);
    let root_module = collector.new_module(ModuleKind::Root, None, Vec::new());
    debug_assert_eq!(root_module, collector.map.root);
    // The prelude is its own module (id 1), the lowest-priority lookup fallback.
    let prelude = collector.new_module(ModuleKind::Prelude, None, vec!["$prelude".to_owned()]);
    collector.map.prelude = prelude;
    collector.populate_prelude(prelude);
    // The prelude SOURCE (vfs injects `$prelude.mrn` into every crate):
    // operator traits and builtin impls collected into the prelude module,
    // resolved by the same phases as user code.
    if let Some(pf) = krate
        .files(db)
        .iter()
        .find(|f| f.path(db).as_os_str() == crate::base::vfs::Vfs::PRELUDE_PATH)
        .copied()
    {
        let ptree = item_tree(db, pf);
        collector.collect_items(&ptree.top_level, pf, prelude, Path::new(""));
    }
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
    /// The crate root file — also the nominal `file` for synthetic prelude defs
    /// (with reserved synthetic `FileAstId`s, so they never collide with it).
    root_file: SourceFile,
    /// Path → file, for resolving `mod foo;` to another file in the crate.
    files: HashMap<PathBuf, SourceFile>,
    /// `(module, file, use-item ast-id, vis, use-tree)` for every `use`,
    /// recorded during collection and consumed by the import fixpoint and the
    /// privacy check. The file + ast-id anchor the check's diagnostics.
    uses: Vec<(ModuleId, SourceFile, FileAstId, SurfaceVisibility, UseTree)>,
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
            root_file: krate.root_file(db),
            map: CrateDefMap {
                modules: Vec::new(),
                root: ModuleId(0),
                prelude: ModuleId(1),
                defs: HashMap::new(),
                def_to_module: HashMap::new(),
                impl_methods: HashMap::new(),
                trait_methods: HashMap::new(),
                trait_impls: HashMap::new(),
                trait_consts: HashMap::new(),
                trait_dispatch: HashMap::new(),
                method_trait: HashMap::new(),
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

    /// Fill the prelude with the language builtins — types and intrinsic fns,
    /// all `Public` and in the `Item` namespace. Each gets a `DefId` minted from
    /// a synthetic `FileAstId` (so prelude defs have stable ids and can be
    /// method-dispatch owners) but is **not** given a `DefPath` (they are
    /// rebuilt identically each session and need no cross-session identity).
    /// Signatures for the fns are synthesised later by `sig_of` (Q3b).
    fn populate_prelude(&mut self, prelude: ModuleId) {
        for (i, (name, kind)) in BUILTINS.iter().enumerate() {
            let ast_id = crate::syntax::ast_id::FileAstId::synthetic(i as u16);
            let def = DefId::new(self.db, self.root_file, ast_id, DefRole::Item);
            self.map.defs.insert(
                def,
                DefData {
                    kind: *kind,
                    name: (*name).to_owned(),
                    module: prelude,
                    visibility: Visibility::Public,
                    owner: None,
                    inline: false,
                },
            );
            self.map.modules[prelude.0 as usize].items.insert(
                ((*name).to_owned(), kind.namespace()),
                Binding {
                    def,
                    source: BindingSource::Def,
                    vis: Visibility::Public,
                },
            );
        }
    }

    /// Collect the items declared in one module. `file` is the file they live in;
    /// `dir` is the directory in which a `mod foo;` among them resolves to
    /// `dir/foo.mrn`.
    fn collect_items(&mut self, items: &[Item], file: SourceFile, module: ModuleId, dir: &Path) {
        for item in items {
            match item {
                Item::Fn(f) => {
                    let def = self.declare(
                        file,
                        f.ast_id,
                        DefRole::Item,
                        &f.name,
                        DefKind::Fn,
                        module,
                        &f.visibility,
                        None,
                    );
                    if f.inline
                        && let Some(d) = self.map.defs.get_mut(&def)
                    {
                        d.inline = true;
                    }
                }
                Item::Struct(s) => self.declare_adt(file, s, DefKind::Struct, module),
                Item::Port(p) => self.declare_adt(file, p, DefKind::Port, module),
                Item::Mod(m) => self.collect_mod(m, file, module, dir),
                Item::Use(u) => {
                    self.uses
                        .push((module, file, u.ast_id, u.visibility.clone(), u.tree.clone()))
                }
                Item::Trait(t) => self.declare_trait(file, t, module),
                Item::Impl(i) => self.impls.push((module, file, i.clone())),
            }
        }
    }

    /// Declare a trait: the trait def (Item namespace) plus a `Method` def per
    /// method declaration, owned by the trait and indexed in `trait_methods`.
    /// Associated-const declarations get their defs with the assoc-const work
    /// (planning/traits.md T4).
    fn declare_trait(
        &mut self,
        file: SourceFile,
        item: &crate::syntax::item_tree::TraitItem,
        module: ModuleId,
    ) {
        let trait_def = self.declare(
            file,
            item.ast_id,
            DefRole::Item,
            &item.name,
            DefKind::Trait,
            module,
            &item.visibility,
            None,
        );
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
                    owner: Some(trait_def),
                    inline: false,
                },
            );
            self.def_order.push(def);
            self.map
                .trait_methods
                .insert((trait_def, method.name.clone()), def);
        }
        for c in &item.consts {
            let def = DefId::new(self.db, file, c.ast_id, DefRole::Item);
            self.map.defs.insert(
                def,
                DefData {
                    kind: DefKind::AssocConst,
                    name: c.name.clone(),
                    module,
                    visibility: Visibility::Public,
                    owner: Some(trait_def),
                    inline: false,
                },
            );
            self.def_order.push(def);
            self.map
                .trait_consts
                .insert((trait_def, c.name.clone()), def);
        }
    }

    /// Declare a struct/port: the type def plus its term-level constructor (a
    /// distinct `DefKind::Ctor` sharing the type's `FileAstId` via `DefRole::Ctor`,
    /// owned by the type). Both names land in the `Item` namespace, so a type and
    /// a constructor that share a name (`struct S = S`) collide.
    fn declare_adt(
        &mut self,
        file: SourceFile,
        item: &crate::syntax::item_tree::NamedItem,
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
        ast_id: crate::syntax::ast_id::FileAstId,
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
                inline: false,
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
            self.map.diagnostics.push(DefDiagnostic {
                anchor: Some((file, ast_id)),
                kind: DefDiagnosticKind::DuplicateDef {
                    name: name.to_owned(),
                },
            });
        }
        def
    }

    /// Collect a `mod m`. Its declaration is recorded in `parent`; its body comes
    /// either from the inline `{ … }` (same file) or, for `mod m;`, from the file
    /// `dir/m.mrn`. Either way the module's *own* children resolve their file
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
                let mod_path = dir.join(format!("{}.mrn", m.name));
                if let Some(&mod_file) = self.files.get(&mod_path) {
                    let tree = item_tree(self.db, mod_file);
                    self.collect_items(&tree.top_level, mod_file, sub, &child_dir);
                } else {
                    self.map.diagnostics.push(DefDiagnostic {
                        anchor: Some((file, m.ast_id)),
                        kind: DefDiagnosticKind::UnresolvedModule {
                            name: m.name.clone(),
                        },
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
    /// grow as other imports land. Mirin has no macros, so this is the only
    /// fixpoint resolution needs.
    fn resolve_imports(&mut self) {
        let uses = std::mem::take(&mut self.uses);
        loop {
            let mut changed = false;
            for (module, _file, _ast_id, use_vis, tree) in &uses {
                // The binding's visibility is the re-export visibility: a plain
                // `use` (Inherited) → module-private; a `pub use` → its declared
                // visibility. `resolve_visibility` maps both correctly.
                let vis = self.resolve_visibility(use_vis, *module);
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
            let Some(def) = self.map.resolve_path(prefix, module, Namespace::Module) else {
                return false;
            };
            let name = alias.unwrap_or(modname);
            return self.import_binding(module, name, Namespace::Module, def, vis);
        }
        let name = alias.unwrap_or(last);
        let mut changed = false;
        // Import the name in whichever namespace(s) it resolves to.
        for ns in [Namespace::Module, Namespace::Item] {
            if let Some(def) = self.map.resolve_path(segments, module, ns) {
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

    // ----- path resolution ----- (the primitives live on `CrateDefMap`)

    /// Resolve a path whose final segment must itself be a module.
    fn resolve_path_to_module(&self, segments: &[&str], from: ModuleId) -> Option<ModuleId> {
        if segments.is_empty() {
            // Empty prefix (`{ … }` / `*` at the current module).
            return Some(from);
        }
        let def = self.map.resolve_path(segments, from, Namespace::Module)?;
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
        // Each leaf carries its `use` item's anchor, for located diagnostics.
        let mut leaves: Vec<(ModuleId, (SourceFile, FileAstId), Vec<String>)> = Vec::new();
        for (module, file, ast_id, _vis, tree) in &uses {
            use_leaves(tree, &[], &mut |segs| {
                leaves.push((*module, (*file, *ast_id), segs))
            });
        }
        for (module, anchor, mut segs) in leaves {
            // A trailing `self` names the prefix module.
            if segs.last().map(String::as_str) == Some("self") {
                segs.pop();
            }
            if segs.is_empty() {
                continue;
            }
            let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
            self.check_path_access(&refs, module, anchor);
        }
        self.uses = uses;
    }

    /// Walk a path's bindings (intermediate modules, then the final name) from
    /// `from`, recording the first failure as a diagnostic located at `anchor`
    /// (the `use` item).
    fn check_path_access(
        &mut self,
        segments: &[&str],
        from: ModuleId,
        anchor: (SourceFile, FileAstId),
    ) {
        let (mut module, start) = self.map.path_anchor(segments, from);
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
                self.map.diagnostics.push(DefDiagnostic {
                    anchor: Some(anchor),
                    kind: DefDiagnosticKind::UnresolvedImport {
                        path: segments.iter().map(|s| s.to_string()).collect(),
                    },
                });
                return;
            };
            if !own_scope && !self.vis_accessible(binding.vis, from) {
                self.push_private(binding.def, anchor);
                return;
            }
            let Some(next) = self.map.module_of_def(binding.def) else {
                return;
            };
            module = next;
            i += 1;
        }
        // Final segment — a name may exist in several namespaces (a `mod x`
        // and an item `x`); the import takes whichever are accessible, and
        // privacy is an error only when NO namespace yields an accessible
        // binding (Rust's per-namespace rule).
        let own_scope = i == start && start == 0;
        let mut private_def = None;
        let mut found = false;
        for ns in [Namespace::Module, Namespace::Item] {
            if let Some(binding) = self.map.binding_local(module, segments[i], ns) {
                found = true;
                if own_scope || self.vis_accessible(binding.vis, from) {
                    return; // at least one namespace is accessible
                }
                private_def.get_or_insert(binding.def);
            }
        }
        if found {
            if let Some(def) = private_def {
                self.push_private(def, anchor);
            }
            return;
        }
        self.map.diagnostics.push(DefDiagnostic {
            anchor: Some(anchor),
            kind: DefDiagnosticKind::UnresolvedImport {
                path: segments.iter().map(|s| s.to_string()).collect(),
            },
        });
    }

    fn push_private(&mut self, def: DefId<'db>, anchor: (SourceFile, FileAstId)) {
        if let Some(data) = self.map.def_data(def) {
            self.map.diagnostics.push(DefDiagnostic {
                anchor: Some(anchor),
                kind: DefDiagnosticKind::PrivateImport {
                    name: data.name.clone(),
                },
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
            let is_trait_impl = item.trait_.is_some();
            let owner = self
                .map
                .resolve_in_scope(module, &item.owner, Namespace::Item);
            let owner = match owner.and_then(|o| self.map.def_data(o).map(|d| (o, d.kind))) {
                Some((o, DefKind::Struct | DefKind::Port)) => o,
                // A trait impl may implement for a builtin (`impl Bits for bool`).
                Some((o, DefKind::BuiltinType)) if is_trait_impl => o,
                _ => {
                    self.map.diagnostics.push(DefDiagnostic {
                        anchor: Some((file, item.ast_id)),
                        kind: DefDiagnosticKind::UnresolvedImplOwner {
                            name: item.owner.clone(),
                        },
                    });
                    continue;
                }
            };
            // A trait impl's trait must resolve to a trait def.
            let trait_def = match &item.trait_ {
                None => None,
                Some(tname) => {
                    let t = self.map.resolve_in_scope(module, tname, Namespace::Item);
                    match t.and_then(|t| self.map.def_data(t).map(|d| (t, d.kind))) {
                        Some((t, DefKind::Trait)) => Some(t),
                        _ => {
                            self.map.diagnostics.push(DefDiagnostic {
                                anchor: Some((file, item.ast_id)),
                                kind: DefDiagnosticKind::UnresolvedTrait {
                                    name: tname.clone(),
                                },
                            });
                            continue;
                        }
                    }
                }
            };
            let mut impl_consts: Vec<(String, DefId<'db>)> = Vec::new();
            if let Some(t) = trait_def {
                for c in &item.consts {
                    let def = DefId::new(self.db, file, c.ast_id, DefRole::Item);
                    self.map.defs.insert(
                        def,
                        DefData {
                            kind: DefKind::AssocConst,
                            name: c.name.clone(),
                            module,
                            visibility: Visibility::Public,
                            owner: Some(owner),
                            inline: false,
                        },
                    );
                    self.def_order.push(def);
                    let _ = t;
                    impl_consts.push((c.name.clone(), def));
                }
            }
            let mut impl_methods: Vec<(String, DefId<'db>)> = Vec::new();
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
                        inline: method.inline,
                    },
                );
                self.def_order.push(def);
                match trait_def {
                    // Trait-impl methods are reached through trait selection,
                    // never the inherent index (an inherent method and a trait
                    // method may share a name; inherent wins at dispatch).
                    Some(_) => impl_methods.push((method.name.clone(), def)),
                    // Last writer wins on a duplicate method name (lenient; the
                    // duplicate-method diagnostic lands with the error surface, Q6).
                    None => {
                        self.map
                            .impl_methods
                            .insert((owner, method.name.clone()), def);
                    }
                }
            }
            // The impl block's own def — for BOTH inherent and trait impls. Its
            // sig is the impl HEADER (binder generics + self type + binder
            // bounds), which carries header diagnostics like a generic owner
            // written un-applied (`impl Bus` on `struct Bus(A: Type)`).
            let impl_def = DefId::new(self.db, file, item.ast_id, DefRole::Item);
            self.map.defs.insert(
                impl_def,
                DefData {
                    kind: DefKind::Impl,
                    name: format!(
                        "impl_{}_{}",
                        item.trait_.clone().unwrap_or_default(),
                        item.owner
                    ),
                    module,
                    visibility: Visibility::Restricted(module),
                    owner: Some(owner),
                    inline: false,
                },
            );
            self.def_order.push(impl_def);
            if let Some(t) = trait_def {
                // Conformance, name level: every trait method implemented,
                // nothing extra. (Signature-level conformance arrives with
                // the solver slice — planning/traits.md T3.)
                let declared_consts: Vec<&String> = self
                    .map
                    .trait_consts
                    .keys()
                    .filter(|(td, _)| *td == t)
                    .map(|(_, n)| n)
                    .collect();
                for d in &declared_consts {
                    if impl_consts.iter().all(|(n, _)| n != *d) {
                        self.map.diagnostics.push(DefDiagnostic {
                            anchor: Some((file, item.ast_id)),
                            kind: DefDiagnosticKind::MissingTraitItem {
                                trait_name: item.trait_.clone().unwrap_or_default(),
                                name: (*d).clone(),
                            },
                        });
                    }
                }
                for (n, _) in &impl_consts {
                    if self.map.trait_consts.get(&(t, n.clone())).is_none() {
                        self.map.diagnostics.push(DefDiagnostic {
                            anchor: Some((file, item.ast_id)),
                            kind: DefDiagnosticKind::ExtraTraitItem {
                                trait_name: item.trait_.clone().unwrap_or_default(),
                                name: n.clone(),
                            },
                        });
                    }
                }
                let declared: Vec<&String> = self
                    .map
                    .trait_methods
                    .keys()
                    .filter(|(td, _)| *td == t)
                    .map(|(_, n)| n)
                    .collect();
                for d in &declared {
                    if impl_methods.iter().all(|(n, _)| n != *d) {
                        self.map.diagnostics.push(DefDiagnostic {
                            anchor: Some((file, item.ast_id)),
                            kind: DefDiagnosticKind::MissingTraitItem {
                                trait_name: item.trait_.clone().unwrap_or_default(),
                                name: (*d).clone(),
                            },
                        });
                    }
                }
                for (n, _) in &impl_methods {
                    if self.map.trait_methods.get(&(t, n.clone())).is_none() {
                        self.map.diagnostics.push(DefDiagnostic {
                            anchor: Some((file, item.ast_id)),
                            kind: DefDiagnosticKind::ExtraTraitItem {
                                trait_name: item.trait_.clone().unwrap_or_default(),
                                name: n.clone(),
                            },
                        });
                    }
                }
                // Coherence, cheap first cut: two impls of one trait for the
                // same arg-less head always overlap. Header unification for
                // parameterised self types lands with the solver (T3).
                let dup = self.map.trait_impls.get(&t).is_some_and(|impls| {
                    impls
                        .iter()
                        .any(|i| i.self_def == owner && !i.self_has_args)
                });
                if dup && !item.self_has_args {
                    self.map.diagnostics.push(DefDiagnostic {
                        anchor: Some((file, item.ast_id)),
                        kind: DefDiagnosticKind::OverlappingImpls {
                            trait_name: item.trait_.clone().unwrap_or_default(),
                            ty: item.owner.clone(),
                        },
                    });
                }
                for (n, def) in &impl_methods {
                    self.map.method_trait.insert(*def, t);
                    self.map
                        .trait_dispatch
                        .entry((owner, n.clone()))
                        .or_default()
                        .push((t, *def));
                }
                self.map
                    .trait_impls
                    .entry(t)
                    .or_default()
                    .push(TraitImplData {
                        impl_def,
                        self_def: owner,
                        self_has_args: item.self_has_args,
                        methods: impl_methods,
                        consts: impl_consts,
                        file,
                        ast_id: item.ast_id,
                    });
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
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;

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
        let krate = single(&mut db, &mut vfs, "t.mrn", SAMPLE);
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
        let krate = single(&mut db, &mut vfs, "t.mrn", SAMPLE);
        let map = crate_def_map(&db, krate);

        // root + prelude + inner = 3 modules.
        assert_eq!(map.modules().len(), 3);
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
        let krate = single(&mut db, &mut vfs, "t.mrn", SAMPLE);
        let before = summary(crate_def_map(&db, krate));

        vfs.set_file_text(
            &mut db,
            "t.mrn",
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
            "top.mrn",
            "mod child;\nfn root_fn () -> uint(8) { return 0; }",
        );
        vfs.set_file_text(
            &mut db,
            "child.mrn",
            "fn helper () -> uint(8) { return 0; }",
        );
        let krate = vfs.source_root(&mut db, "top.mrn");
        let map = crate_def_map(&db, krate);

        // The root holds `child` (Module ns) and `root_fn` (Item ns).
        assert!(
            map.resolve_local(map.root(), "root_fn", Namespace::Item)
                .is_some()
        );
        let child = named_module(map, "child", Namespace::Module).expect("child module");
        assert_eq!(map.module(child).parent(), Some(map.root()));
        // `helper` from child.mrn lives in the child module, not the root.
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
        // `mod a;` at the root → a.mrn; `mod b;` inside a → a/b.mrn.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "top.mrn", "mod a;");
        vfs.set_file_text(
            &mut db,
            "a.mrn",
            "mod b;\nfn in_a () -> uint(8) { return 0; }",
        );
        vfs.set_file_text(&mut db, "a/b.mrn", "fn in_b () -> uint(8) { return 0; }");
        let krate = vfs.source_root(&mut db, "top.mrn");
        let map = crate_def_map(&db, krate);

        let a = named_module(map, "a", Namespace::Module).expect("module a");
        assert!(map.resolve_local(a, "in_a", Namespace::Item).is_some());
        let b = named_module(map, "b", Namespace::Module).expect("module b");
        assert_eq!(map.module(b).parent(), Some(a));
        assert!(map.resolve_local(b, "in_b", Namespace::Item).is_some());
    }

    #[test]
    fn missing_module_file_yields_an_empty_module() {
        // `mod ghost;` with no ghost.mrn: the name still resolves, body is empty.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "top.mrn", "mod ghost;");
        let map = crate_def_map(&db, krate);

        let ghost = named_module(map, "ghost", Namespace::Module).expect("ghost module");
        assert_eq!(map.module(ghost).items().count(), 0);
    }

    #[test]
    fn editing_a_file_modules_body_does_not_change_the_def_map() {
        // The firewall across files: a body edit in child.mrn backdates its
        // item_tree, so the crate-wide def map is unchanged.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        vfs.set_file_text(&mut db, "top.mrn", "mod child;");
        vfs.set_file_text(
            &mut db,
            "child.mrn",
            "fn helper () -> uint(8) { return 0; }",
        );
        let krate = vfs.source_root(&mut db, "top.mrn");
        let before = summary(crate_def_map(&db, krate));

        vfs.set_file_text(
            &mut db,
            "child.mrn",
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
            "t.mrn",
            "mod m { pub fn f () -> uint(8) { return 0; } }\nuse m::f;",
        );
        let map = crate_def_map(&db, krate);
        assert!(
            map.resolve_local(map.root(), "f", Namespace::Item)
                .is_some()
        );
        assert!(
            map.diagnostics().is_empty(),
            "{:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn use_glob_imports_only_accessible_names() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
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
            "t.mrn",
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
            "t.mrn",
            "mod a { pub mod b { pub fn f () -> uint(8) { return 0; } } }\nuse a::b;",
        );
        let map = crate_def_map(&db, krate);
        // `b` is imported into the root in the Module namespace.
        assert!(
            map.resolve_local(map.root(), "b", Namespace::Module)
                .is_some()
        );
        assert!(
            map.diagnostics().is_empty(),
            "{:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_local_def_beats_an_import() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
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
    fn dual_namespace_import_takes_the_accessible_one() {
        // `offset` exists as a PRIVATE module and (via `pub use`) a public
        // fn. Rust's per-namespace rule: the import takes the accessible
        // binding; privacy errors only when no namespace is accessible.
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
            "mod s { mod offset { pub fn offset (x: uint(8)) -> uint(8) { return x; } }\n\
             pub use crate::s::offset::offset; }\n\
             use crate::s::offset;\n\
             fn top (x: uint(8)) -> uint(8) { return offset(x); }",
        );
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics().is_empty(),
            "accessible Item-namespace binding must satisfy the import: {:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
        );
        assert!(
            map.resolve_local(map.root(), "offset", Namespace::Item)
                .is_some(),
            "the fn must be imported"
        );
    }

    #[test]
    fn importing_a_private_name_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
            "mod m { fn secret () -> uint(8) { return 0; } }\nuse m::secret;",
        );
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics().iter().any(
                |d| matches!(&d.kind, DefDiagnosticKind::PrivateImport { name } if name == "secret")
            ),
            "{:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pub_crate_is_reachable_from_another_subtree() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
            "mod a { pub(crate) fn f () -> uint(8) { return 0; } }\nmod b { use crate::a::f; }",
        );
        let map = crate_def_map(&db, krate);
        let b = named_module(map, "b", Namespace::Module).expect("module b");
        assert!(map.resolve_local(b, "f", Namespace::Item).is_some());
        assert!(
            map.diagnostics().is_empty(),
            "{:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_unresolved_import_is_flagged() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "t.mrn", "use nonexistent::thing;");
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, DefDiagnosticKind::UnresolvedImport { .. })),
            "{:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
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
            "t.mrn",
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
        assert!(
            map.diagnostics().is_empty(),
            "{:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn struct_with_type_named_constructor_collides() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(&mut db, &mut vfs, "t.mrn", "struct S = S { a: uint(8) }");
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics().iter().any(
                |d| matches!(&d.kind, DefDiagnosticKind::DuplicateDef { name } if name == "S")
            ),
            "{:?}",
            map.diagnostics()
                .iter()
                .map(|d| &d.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn impl_methods_are_indexed_by_owner() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
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
            "t.mrn",
            "impl Ghost { fn m (self) -> uint(8) { return 0; } }",
        );
        let map = crate_def_map(&db, krate);
        assert!(
            map.diagnostics().iter().any(
                |d| matches!(&d.kind, DefDiagnosticKind::UnresolvedImplOwner { name } if name == "Ghost")
            ),
            "{:?}",
            map.diagnostics().iter().map(|d| &d.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn def_path_is_module_qualified() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
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
            "t.mrn",
            "fn f () -> uint(8) { return 0; }",
        );
        let before = {
            let map = crate_def_map(&db, krate);
            let f = map.resolve_local(map.root(), "f", Namespace::Item).unwrap();
            map.def_path_hash(f).unwrap()
        };
        vfs.set_file_text(&mut db, "t.mrn", "fn f () -> uint(8) { return 0 + 1 + 2; }");
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
            "t.mrn",
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

    // ----- Q3a: prelude + in-scope resolution -----

    #[test]
    fn prelude_builtins_resolve_in_scope() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
            "fn f () -> uint(8) { return 0; }",
        );
        let map = crate_def_map(&db, krate);
        let root = map.root();
        // Builtin type and intrinsic fn both reachable from the root via scope.
        let uint = map
            .resolve_in_scope(root, "uint", Namespace::Item)
            .expect("uint in scope");
        assert_eq!(map.def_data(uint).unwrap().kind, DefKind::BuiltinType);
        assert_eq!(
            map.resolve_in_scope(root, "reg", Namespace::Item)
                .map(|d| map.def_data(d).unwrap().kind),
            Some(DefKind::Fn)
        );
    }

    #[test]
    fn prelude_is_a_separate_lowest_priority_module() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = single(
            &mut db,
            &mut vfs,
            "t.mrn",
            "fn f () -> uint(8) { return 0; }",
        );
        let map = crate_def_map(&db, krate);
        let root = map.root();
        assert!(root != map.prelude(), "prelude is a distinct module");
        // Builtins live in the prelude, not the root's own table.
        assert!(map.resolve_local(root, "uint", Namespace::Item).is_none());
        assert!(
            map.resolve_in_scope(root, "uint", Namespace::Item)
                .is_some()
        );
    }

    #[test]
    fn a_user_def_shadows_a_prelude_builtin() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // A user type named `uint` shadows the prelude builtin of the same name.
        let krate = single(&mut db, &mut vfs, "t.mrn", "struct uint = u { a: uint(8) }");
        let map = crate_def_map(&db, krate);
        let resolved = map
            .resolve_in_scope(map.root(), "uint", Namespace::Item)
            .unwrap();
        assert_eq!(
            map.def_data(resolved).unwrap().kind,
            DefKind::Struct,
            "the user `struct uint` must win over the prelude builtin"
        );
    }
}
