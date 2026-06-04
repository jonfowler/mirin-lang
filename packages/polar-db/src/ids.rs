//! Stable definition identity — the interned `DefId` (`planning/query_engine.md`
//! §2).
//!
//! A `DefId` is minted by **interning a syntactic location** `(file, FileAstId)`,
//! exactly like rust-analyzer's name-resolution collector. Because the
//! [`FileAstId`] is a hash-of-identity (kind + name + parent), not a byte offset
//! or sibling index, the interned id is stable across edits that don't change the
//! item's identity — including edits to other items, reformatting, and edits
//! inside this item's own body. salsa hands back the *same* integer for the same
//! location across revisions, so every downstream memo key built on a `DefId`
//! survives those edits.
//!
//! This replaces `polar-compiler`'s counter-minted `DefId` (a running index over
//! the surface IR, which was not offset-stable). The `CrateNum`/`StableCrateId`
//! layering from `resolve.rs` will return in Q2d with the `DefPath` table; for
//! now the whole local repo is one crate (§3.5) so a bare interned location
//! suffices to identify a def.

use crate::ast_id::FileAstId;
use crate::db::SourceFile;

/// A definition's stable identity: the interned syntactic location of the item
/// that introduced it. `DefId::new(db, file, ast_id)` returns the same id for the
/// same location on every revision; the accessors recover the location.
///
/// Carries the `'db` lifetime that salsa threads through interned entities — the
/// ergonomic cost flagged in `planning/query_engine.md` §7. Everything that
/// stores a `DefId` (the [`crate::def_map`] tables) inherits it.
#[salsa::interned]
pub struct DefId<'db> {
    /// The file the defining item lives in.
    pub file: SourceFile,
    /// The item's stable id within that file.
    pub ast_id: FileAstId,
}

/// The two name namespaces (`modules.md` §5.1). Polar splits **modules** from
/// everything else, rather than Rust's type/value split: a type and its
/// constructor share the `Item` namespace (so `struct S = S` collides), while a
/// `mod` lives in its own namespace and may share a name with an item (the
/// common `mod df { port DF = df { … } }`). A module's name table is keyed by
/// `(name, Namespace)`; a path's non-final segments resolve in `Module`, a leaf
/// or bare name in `Item`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::Update)]
pub enum Namespace {
    /// `mod` names — referenced only in path-prefix position.
    Module,
    /// Types, functions, constructors, builtin types.
    Item,
}

/// The flavor of a definition. The Q2a subset: the named items that enter a
/// module's name table. Method/Ctor/BuiltinType (which carry an owner `DefId`)
/// arrive with the impl-method index in Q2d.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::Update)]
pub enum DefKind {
    Fn,
    Struct,
    Port,
    /// An inline `mod foo { … }`. Lives in the type namespace; keys into the
    /// module tree. Erased before HIR — modules are a name-resolution concern.
    Mod,
}

impl DefKind {
    /// Which namespace this def's *name* occupies. `Mod` is the lone `Module`-ns
    /// kind; types, fns, and (from Q2d) constructors all share the `Item`
    /// namespace. Nameless defs (impl blocks) and index-only defs (methods) will
    /// return `None` once they exist (Q2d).
    pub fn namespace(self) -> Namespace {
        match self {
            DefKind::Mod => Namespace::Module,
            DefKind::Fn | DefKind::Struct | DefKind::Port => Namespace::Item,
        }
    }
}
