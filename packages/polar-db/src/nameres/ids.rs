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
//! the surface IR, which was not offset-stable). [`StableCrateId`] +
//! [`DefPath`]/[`DefPathHash`] (the cross-session, cross-crate identity layer)
//! are built over this in `crate_def_map` (Q2d); for now the whole local repo is
//! one crate (§3.5).

use crate::base::db::SourceFile;
use crate::syntax::ast_id::FileAstId;

/// A definition's stable identity: the interned syntactic location of the item
/// that introduced it, plus a [`DefRole`]. `DefId::new(db, file, ast_id, role)`
/// returns the same id for the same location+role on every revision; the
/// accessors recover them.
///
/// The `role` lets several defs share one [`FileAstId`]: a `struct`/`port`
/// introduces both a type (`role = Item`) and a constructor (`role = Ctor`) from
/// one syntactic item. This is the same mechanism §2.3 calls for to give
/// anonymous consts their own ids under a parent.
///
/// Carries the `'db` lifetime that salsa threads through interned entities — the
/// ergonomic cost flagged in `planning/query_engine.md` §7. Everything that
/// stores a `DefId` (the [`crate::nameres::def_map`] tables) inherits it.
#[salsa::interned]
pub struct DefId<'db> {
    /// The file the defining item lives in.
    pub file: SourceFile,
    /// The item's stable id within that file.
    pub ast_id: FileAstId,
    /// Which def *of* that syntactic item this is.
    pub role: DefRole,
}

/// Which definition a [`DefId`] denotes among those sharing one [`FileAstId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::Update)]
pub enum DefRole {
    /// The item itself — a fn, struct/port type, mod, or impl method.
    Item,
    /// The term-level constructor a struct/port introduces (`struct Bus = bus`),
    /// sharing the type's `FileAstId` but distinguished by this role.
    Ctor,
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

/// The flavor of a definition. The owner of a `Ctor`/`Method` (the type it
/// belongs to) is carried out-of-band in `DefData::owner`, so `DefKind` stays
/// free of the `'db` lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::Update)]
pub enum DefKind {
    Fn,
    Struct,
    Port,
    /// An inline or file `mod`. Lives in the Module namespace; keys into the
    /// module tree. Erased before HIR — modules are a name-resolution concern.
    Mod,
    /// A term-level constructor (`struct Bus = bus` → `bus`). Item namespace.
    Ctor,
    /// A function in an `impl` block. Reached through the impl-method index, not
    /// a module name table.
    Method,
    /// An `impl T { … }` block. Introduces no name of its own.
    Impl,
    /// A primitive type baked into the language (`uint`, `bool`, `Clock`, …),
    /// carried by the synthetic prelude. Its `DefId` is the owner key in the
    /// impl-method index so prelude methods (`uint.reg`) dispatch like user ones.
    BuiltinType,
}

impl DefKind {
    /// Which namespace this def's *name* occupies. `Mod` is the lone `Module`-ns
    /// kind; types, fns, and constructors share the `Item` namespace. `Method`
    /// and `Impl` are never entered in a module name table (methods live in the
    /// impl-method index, impls are nameless); the value is unused for them.
    pub fn namespace(self) -> Namespace {
        match self {
            DefKind::Mod => Namespace::Module,
            DefKind::Fn
            | DefKind::Struct
            | DefKind::Port
            | DefKind::Ctor
            | DefKind::Method
            | DefKind::Impl
            | DefKind::BuiltinType => Namespace::Item,
        }
    }
}

/// A crate's stable, content-independent identity — the high half of a
/// [`DefPathHash`], so paths in different crates never collide. One local crate
/// for now (§3.5); `root()` is its id. Mirrors rustc's `StableCrateId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::Update)]
pub struct StableCrateId(pub u64);

impl StableCrateId {
    pub fn from_crate_name(name: &str) -> Self {
        StableCrateId(stable_hash_bytes(name.as_bytes()))
    }

    /// The single local crate's id, until external crates are loaded.
    pub fn root() -> Self {
        StableCrateId::from_crate_name("crate")
    }
}

/// One segment of a [`DefPath`]: what it names plus a disambiguator that
/// separates defs which would otherwise share a path (rustc's `DefPathData` +
/// disambiguator).
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub struct DefPathSegment {
    pub kind: DefPathSegmentKind,
    pub disambiguator: u32,
}

/// What a [`DefPathSegment`] denotes. `Named` covers every def that has a name
/// (items, constructors, modules). `AnonConst` is the §2.3 hook: a `uint(<expr>)`
/// width body has no name, so its identity is its **structural role** under the
/// parent. No anon-consts are minted yet (Q4), but the representation is baked in
/// now so the persisted `DefPathHash`es never need migrating.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub enum DefPathSegmentKind {
    Named(String),
    AnonConst(AnonConstRole),
}

/// The structural role of an anonymous const, preferred over a flat positional
/// counter so editing one width never renumbers another (§2.3). Extended as
/// anon-const sites are added; the disambiguator on [`DefPathSegment`] is the
/// fallback for genuinely positional cases.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub enum AnonConstRole {
    /// The width of the return type.
    ReturnTypeWidth,
    /// The width of the i-th parameter's type.
    ParamWidth(u32),
    /// The width of the i-th field's type.
    FieldWidth(u32),
}

/// The **stable** identity of a definition: the disambiguated segment path from
/// the crate root (`crate::util::cfg::parse`). Survives edits to unrelated
/// siblings the way an integer index does not. Mirrors `resolve.rs::DefPath`
/// (the `CrateNum` is implicit — one local crate).
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub struct DefPath {
    pub segments: Vec<DefPathSegment>,
}

/// Hash of `(StableCrateId, DefPath)` — the serializable, cross-session-stable
/// id (and the basis for cross-crate references). High 64 bits are the
/// `StableCrateId`; low 64 a stable hash of the segments. Mirrors rustc's 128-bit
/// `DefPathHash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, salsa::Update)]
pub struct DefPathHash(pub u128);

impl DefPathHash {
    pub fn new(krate: StableCrateId, path: &DefPath) -> Self {
        DefPathHash(((krate.0 as u128) << 64) | hash_def_path(path) as u128)
    }

    pub fn stable_crate_id(self) -> StableCrateId {
        StableCrateId((self.0 >> 64) as u64)
    }
}

/// A small, dependency-free, stable hash (FNV-1a, 64-bit) — stable across runs
/// and builds, unlike `std`'s `DefaultHasher`. Mirrors `resolve.rs`.
fn stable_hash_bytes(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Stable hash of a crate-relative segment list (the crate is folded into the
/// high half of `DefPathHash`, so it is not rehashed here).
fn hash_def_path(path: &DefPath) -> u64 {
    let mut buf: Vec<u8> = Vec::new();
    for seg in &path.segments {
        match &seg.kind {
            DefPathSegmentKind::Named(name) => buf.extend_from_slice(name.as_bytes()),
            DefPathSegmentKind::AnonConst(role) => {
                buf.push(0xa0);
                match role {
                    AnonConstRole::ReturnTypeWidth => buf.push(0),
                    AnonConstRole::ParamWidth(i) => {
                        buf.push(1);
                        buf.extend_from_slice(&i.to_le_bytes());
                    }
                    AnonConstRole::FieldWidth(i) => {
                        buf.push(2);
                        buf.extend_from_slice(&i.to_le_bytes());
                    }
                }
            }
        }
        buf.push(0);
        buf.extend_from_slice(&seg.disambiguator.to_le_bytes());
        buf.push(0xff);
    }
    stable_hash_bytes(&buf)
}
