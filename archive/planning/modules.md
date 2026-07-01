# Module system

Status: **implemented (S1–S6).** This is the source of truth for the design;
`planning/syntax.md` carries the surface forms (`mod`, `use`, `pub`, paths) and
`planning/ir_pipeline.md` the passes (loader, two-phase resolver + import
fixpoint + privacy). Inline and file-based `mod`, `use` (groups/`as`/glob),
`crate`/`super`/`self` paths, and `pub`/`pub(crate)`/`pub(super)`/`pub(in …)`/
`pub use` visibility all work. Deferred (noted in §11 / syntax.md): lowering a
path written *directly* in expression/type position to hardware (use `use`),
and the on-disk incremental query cache (§8).

We follow Rust's module system closely. The user gave the directive plainly:
there is no reason to adopt a different format. Where we diverge it is a
deliberate, documented simplification (one case: the `mod.rs` duality, §4.2).

## 1. Goals and non-goals

Goals:

- `mod` (inline + file-based), `use` (groups, `as`, glob, `self`/`super`/`crate`),
  `pub` visibility with restricted forms and re-exports, 2018-style relative paths.
- Multi-file compilation rooted at a crate root.
- Keep the door open for incremental compilation — stable, path-based identity
  from day one, even though the full query cache is deferred.

Non-goals for this slice:

- The `mirin` build tool itself — manifest, dependency fetching, driving the
  compiler (§2). The compiler is, however, designed to accept multiple crate
  roots at once.
- Per-crate **separate compilation** and a crate-metadata format. This is
  explicitly *not* our model: we compile all crates monolithically (§2).
- Macros / conditional compilation. Mirin has none, which makes name resolution
  meaningfully simpler than rustc's (§7).
- Trait coherence / orphan rules.

## 2. Monolithic compilation, and `mirin-compiler` vs `mirin`

Decision: **one monolithic compile over all crates.** `mirin-compiler` is handed
the source roots of the whole dependency closure at once and compiles them in a
single pass into one `DefId`/`DefPath` space. There is no per-crate metadata
artifact, no serialized metadata format, no separate-compilation boundary to
remap identities across. We deliberately diverge from the rustc/cargo split here.

Rationale: RTL projects are unlikely to grow the large library ecosystems that
make separate compilation pay off. A monolithic model removes a whole stage — the
metadata format and its versioning, the `CStore`/`--extern` machinery, and the
`DefPathHash → DefId` remap on metadata load — and makes incremental compilation
*single-layer* (§8) instead of two layers (cargo crate-level + rustc
query-level) that must compose across a boundary.

We still expect a `mirin` build tool eventually — it owns the manifest, fetches
dependencies, and drives the compiler — but it hands `mirin-compiler` all the
crate roots together rather than orchestrating per-crate invocations that pass
metadata between them.

| Concern | `mirin-compiler` | `mirin` (build tool) |
|---|---|---|
| Unit of work | the whole dependency closure, one monolithic compile | the package graph + driving the compiler |
| Sees manifest / versions / features | no | yes |
| Knows where source came from | no — handed the crate-root file paths | yes (registry/git/path) |
| Resolves the dependency graph | no | yes |
| Dependencies | just more source pulled into the one tree | fetched and handed over as crate roots |
| Incrementality | a single fine-grained query layer over one `DefPath` space | — (no coarse crate-skip tier) |

What we give up by dropping separate compilation, and why it is acceptable here:

- The "free" coarse incrementality of skipping an unchanged crate by file
  timestamp. With one compile, *all* reuse comes from the fine-grained query
  layer (§8). For small RTL dependency closures the whole compile is cheap, so
  the coarse win matters little — and the stable-identity infrastructure that
  powers the fine-grained layer is the same substrate that would let us add
  separate compilation back later if a real ecosystem ever emerges.
- Coarse build parallelism and lower peak memory from compiling crates in
  separate processes. Unlikely to matter at RTL project sizes.

`mirin` is the user's main point of contact. We do **not** build it now; the
interim CLI simply accepts the crate root(s) directly.

## 3. Crates as namespace roots

There is **one** compilation over all crates. Within it:

- A **crate** is a namespace / dependency root — the thing `crate::` anchors to
  and the thing `use other_lib::…` names — *not* a separate compilation unit. The
  crate name is the first segment of every `DefPath`.
- There is one `DefId`/`DefPath` space spanning all crates. `CrateNum` (§6.1)
  partitions that space by crate; with monolithic compilation every crate is
  present in the same session, so there is no local-vs-external distinction.
- The **root crate's root** is the `.mrn` file passed on the CLI — today's single
  input file simply becomes the crate root module, keeping the CLI unchanged.
  Dependency crate roots are handed to the compiler too (by `mirin` eventually;
  by a CLI flag in the interim).
- Each crate root is an unnamed top-level module. `crate::` resolves to the root
  of the crate the code lives in (each crate has its own root); `use dep::…`
  crosses into a dependency crate's tree. Both are ordinary module-tree
  navigation now that everything lives in one tree.

## 4. Surface syntax

### 4.1 `mod`

```rust
mod foo { /* items */ }   // inline
mod foo;                  // file-based: body loaded from foo.mrn
pub mod foo;              // with visibility
```

A `mod foo;` declaration *pulls a file into the module tree*. A `.mrn` file on
disk is **not** part of the crate just by existing — some ancestor must declare
it with `mod`. The filesystem does not define the graph; `mod` statements do.
File layout only says where the compiler *looks* for the body. (Same rule as
Rust, and the one newcomers most often misread.)

### 4.2 File mapping

- Extension: `.mrn`.
- A module `foo` declared `mod foo;` inside the file `dir/X.mrn` loads from
  `dir/foo.mrn`. Its own children live under `dir/foo/`.
- Worked example: root `main.mrn` contains `mod util;` → `util.mrn`; `util.mrn`
  contains `mod cfg;` → `util/cfg.mrn`. The directory is named after the module
  that *owns* the children; that module's body is the sibling `<name>.mrn`.
- **Decision: drop the `mod.rs` duality.** Rust allows both `foo.rs` and
  `foo/mod.rs`; we allow only `foo.mrn` + a `foo/` directory for its children,
  and reject a `mod.mrn`. Rationale: the duality is a back-compat wart Rust
  carries; a greenfield language has none to preserve. This is the single
  deliberate divergence and it is trivially reversible if we change our minds.
- Having an ambiguous or missing backing file is a hard error.
- A `#[path = "..."]` override analog is deferred.

### 4.3 `use`

```rust
use crate::a::b;            // absolute from crate root
use super::x;              // from parent module
use self::y;               // from current module
use a::{b, c::{d, e}};     // groups / nesting
use a::{self, b};          // `self` brings `a` itself into scope alongside children
use a::b as c;             // rename
use a::*;                  // glob (lower priority than explicit names)
pub use a::b;              // re-export: becomes part of this module's public surface
```

### 4.4 Paths

2018-style **relative** resolution: a path resolves relative to the current
module unless it starts with an anchor — `crate::` (root), `super::` (parent,
chainable), `self::` (current). This applies uniformly to `use` paths and to
paths in expression/type position (`foo::Bar`, `Type::method`). Mirin already has
a `PathExpression` node for the `Type::member` shape; multi-segment module paths
extend it.

### 4.5 Visibility

- Default **private**: visible in the defining module and its descendants.
- `pub` (fully public), `pub(crate)`, `pub(super)`, `pub(in <path>)` (the path
  must resolve to an ancestor module). Each form *narrows* a restriction.
- `pub use` re-export: the re-exported name becomes part of the re-exporting
  module's public surface even if the original path stays private.

In an HDL, visibility governs *nameability and library-API encapsulation* — it
does not decide which Verilog gets generated. (All reachable components still
lower; `pub` is about who may *name* a definition, not whether it is emitted.)

## 5. Semantics

### 5.1 Namespaces

Two namespaces, splitting **modules** from everything else:

- **Module namespace**: `mod` names.
- **Item namespace**: `struct`/`port` type names, `fn`, constructors
  (`DefKind::Ctor`), and builtin types (`uint`, `bool`, `Clock`, …).
  (Generic params and locals are body/decl-local scopes; `Type::method` paths
  resolve through the impl-method index, not a module table.)

This deliberately differs from Rust's type/value split. Rust separates types
from values mainly so a unit/tuple struct's *type* and *constructor* can share
one name; Mirin gives constructors **distinct** names (`struct Bus = bus`), so it
needs no such split. Instead Mirin keeps a **single item namespace** — a type and
its constructor both live in it and therefore must differ (`struct S = S` is a
name collision) — and splits out only **modules**, because a module name appears
solely in path-prefix position (`df::X`) and so can coexist with an item of the
same name (the common `mod df { port DF = df { … } }`). Each module's name table
is keyed by `(Symbol, Namespace)` with `Namespace ∈ {Module, Item}`: a path's
non-final segments resolve in the Module namespace, a leaf or bare name in the
Item namespace.

### 5.2 Name lookup order (inside a module body)

For a bare name or a path's first segment:

1. Local ribs — `let`/`var`/param scopes (today's `BlockCtx` logic, unchanged).
2. The current module's own items and its explicit `use` imports.
3. Glob imports into the current module (lower priority; collisions are resolved
   lazily — see §7.3).
4. The prelude (§5.3), the lowest-priority layer.

`crate::`/`super::`/`self::` bypass steps 1–4 to an explicit anchor; later
segments resolve into the named module's table, subject to visibility (§7.4).

### 5.3 Prelude

The current prelude (`reg`, `+`, `*`, `posedge`, `uint`, `bool`, `Clock`,
`Event`, `Type`) becomes a synthetic prelude module whose public names are
injected into every module's scope at lowest priority — replacing today's "dump
everything into one global table." A user-defined name shadows the prelude, as in
Rust. A future "no prelude" opt-out is deferred.

## 6. Compiler integration — data structures

Today's resolver (`resolve.rs`) is flat, single-file, and string-keyed
(`Ctx::global_defs: HashMap<String, (DefKind, DefId)>`, `DefId(u32)` indexing one
`ResolveResult::defs` vector). The module system replaces the single global table
with a module tree and upgrades identity.

### 6.1 `DefId` / `DefPath` / `DefPathHash`

```rust
struct DefId  { krate: CrateNum, index: DefIndex }
struct DefIndex(u32);                                 // == today's DefId(u32)
struct CrateNum(u32);                                 // namespace-root partition
```

- `DefId` stays the fast, in-session currency (an index into the def table).
  `CrateNum` partitions that space by crate (a namespace root); with monolithic
  compilation every crate is in the same session, so there is no
  local-vs-external split — `CrateNum` is about naming and the `crate::` anchor,
  not a compilation boundary.
- `DefPath`: the **stable** identity — the disambiguated name-segment path from
  the crate root (`crate::util::cfg::parse`). Survives edits to unrelated
  siblings (an integer index does not).
- `DefPathHash(u128)`: hash of `(StableCrateId, DefPath)`. The serializable,
  cross-session-stable id. The keystone for incremental and for any future
  cross-crate reference.
- Maintain a `DefPathTable` (`DefId ↔ DefPath`) and, once we load cached/external
  data, a `DefPathHash → DefId` remap built on load.

### 6.2 Module tree

```rust
struct ModuleData {
    kind: ModuleKind,                              // Root | Named(DefId)
    parent: Option<DefId>,
    items: HashMap<(Symbol, Namespace), Binding>,
}
struct Binding { res: Res, vis: Visibility, source: BindingSource }
enum BindingSource { Def, Import, Glob, Prelude }   // drives glob priority + re-export
```

Block-scoped anonymous modules (rustc's `ModuleKind::Block`) are not needed yet;
defer.

### 6.3 `Res` additions

- `Res::Def(DefKind::Mod, DefId)` for modules.
- Imports resolve through to the target's `Res`; an as-yet-unresolved import is
  tracked internally during phase 2 (§7.3).

### 6.4 Crate / definitions container

A `Crate { root: DefId, modules, defs (by DefIndex), def_paths }` becomes the home
of the def tree, replacing the flat `ResolveResult::defs` list. `ResolveResult`
keeps its use-site `resolutions`/`locals` maps.

**Out-of-band item storage**: children are referenced from their parent module by
`DefId`; item bodies live in a `DefId`-keyed map (rustc's HIR shape) rather than
nested inside parents. This keeps "what a module contains" a separate dependency
from "what an item's body is" — which is what makes fine-grained incremental
invalidation possible later.

## 7. Compiler integration — passes

Slots between parse and `lower_to_hir`, replacing the single `resolve_file` call
with: load → build tree → resolve imports → late resolve → privacy. HIR lowering
and everything downstream are unchanged except they consume `DefId { krate, index }`
and the def tree instead of the flat list.

### 7.1 File loading (new, driver level)

Start at the crate root file. Walk items; for each `mod foo;` resolve the path,
read + parse into a `SourceFile`, and recurse; for inline `mod foo { }` recurse
directly. Output: the set of parsed `SourceFile`s plus the module-tree skeleton.

`NodeId` must become crate-unique (today it is per-`SourceFile`). Target shape:
**owner-relative ids** — `(owner: DefId, local_id: u32)`, rustc's `HirId` — so
editing one item's body does not renumber another's (incremental). Minimum viable
first cut: a single crate-wide `NodeId` counter during lowering.

### 7.2 Phase 1 — build module + def tree (extends the collect pass)

Walk every module's items, allocate `DefId`s, populate each `ModuleData` name
table and the `defs`/`def_paths` tables, record visibility. No imports yet.
Because Mirin has no macros, there is **no expansion to interleave and no
fixpoint** here — strictly simpler than rustc's early resolver.

### 7.3 Phase 2 — resolve imports (new)

Resolve each `use` against the built tree. Explicit imports first. Glob imports
need a small **fixpoint** (a glob's imported set depends on the target module's
contents, which may itself contain globs); without macros it converges in a few
passes. Two globs importing the same name is an **ambiguity** recorded and
reported lazily — only if the name is actually used (rustc behavior). Populate
import bindings into the module tables.

### 7.4 Phase 3 — late / intra-body resolution (extends today's `BlockCtx`)

The existing block/expr/type resolution, now: (a) multi-segment paths resolve
into module tables; (b) bare names follow the §5.2 lookup order; (c)
`crate`/`super`/`self` anchors. Local ribs and the `let`/`var`/`=>` machinery are
unchanged.

### 7.5 Phase 4 — privacy check (new, mirrors `rustc_privacy`)

After resolution, for each resolved path to a `Def`, verify the target is
accessible from the use-site module: the ancestor-chain rule, plus any `pub(...)`
restriction, with `pub use` short-circuiting the chain. Kept separate from
resolution on purpose — resolution answers "what does this name bind to," privacy
answers "are you allowed to name it."

## 8. Incremental compilation

> The full design for the query engine this section sketches lives in
> `planning/query_engine.md`. This section states the single-layer *strategy*;
> that doc covers the query graph, firewalls, stable identity, and the migration
> plan.

With one monolithic compile, incremental compilation is **single-layer**: there
is no coarse crate-skip tier, so all reuse comes from fine-grained, query-style
memoization over the single `DefPath` space. This is *simpler* than rustc's
two-tier scheme (cargo crate-level + rustc query-level) precisely because there is
no metadata boundary — the painful `DefPathHash → DefId`-on-load remap does not
exist when every definition is in the same session.

The stable-identity infrastructure is therefore *more* central, not less: it is
the only thing standing between us and incrementality. The throughline:
**identity must come from the program's logical structure (paths), not its
incidental textual layout (indices/offsets).** rustc retrofitted this across the
whole compiler at great cost; we bake it in now at near-zero cost.

Adopt now (cheap, painful to retrofit):

1. `CrateNum` in `DefId` as the crate partition of one `DefPath` space (§6.1).
2. `DefPath` + `DefPathHash`, plus a stable-hash utility that substitutes
   `DefPathHash` for `DefId` and ignores spans. This single utility is what makes
   *any* later fingerprint-based skipping possible.
3. Owner-relative body ids (`HirId = owner + local_id`) so per-item edits do not
   renumber siblings.
4. Out-of-band item storage keyed by `DefId` (§6.4).
5. (Directional, optional) shape passes as `query(key) -> value` with
   memoization — even single-session — to force clean boundaries.
6. Feed compiler inputs through a **VFS** (`path → (text, revision)` overlay)
   rather than direct `fs` reads. Batch CLI fills it from disk once; the LSP
   overlays unsaved editor buffers and bumps revisions. This lets the *same*
   in-memory query engine serve both batch compiles and the language server —
   navigation, highlighting, and live type errors all share work in-process, with
   no cross-process cache, serialization, or locking. See `planning/lsp.md`
   ("Diagnostics and sharing work with a checker").

Defer until there is real demand:

- An on-disk persistent red-green query cache (`rustc_query_system`). The
  in-memory query shape (item 5) is the stepping stone to it.

Not part of this model (would only return if a real library ecosystem ever made
per-crate skipping worthwhile — the `DefPathHash` substrate above would support
adding it then): a crate-metadata format, `StableCrateId`/SVH cross-crate
fingerprints, and pipelined metadata emission. All three are artifacts of
separate compilation, which we have dropped (§2).

## 9. Staging / implementation plan

Each slice is independently committable and leaves the build green.

- **S1** — `DefId` carries `CrateNum` (`LOCAL_CRATE`); add `DefPath`/`DefPathHash`
  + def-path table + stable-hash utility. No behavior change; single-file still
  works.
- **S2** — Module tree + namespaces; inline `mod foo { }`; bare-name lookup
  through module tables; prelude becomes a module. No files, no `use` yet.
- **S3** — File-based `mod foo;` + the loader/driver; crate-unique (ideally
  owner-relative) `NodeId`s.
- **S4** — `use` (explicit, groups, `as`), then glob with the fixpoint; paths with
  `crate`/`super`/`self`.
- **S5** — `pub` vs private + the privacy pass (minimal visibility).
- **S6** — `pub(crate)`/`pub(super)`/`pub(in …)` + `pub use` re-exports (full
  visibility).

Visibility is *designed* in full (§4.5, §7.5) but *implemented* across S5→S6, so
the first working multi-file builds do not block on the restricted forms.

## 10. Backward compatibility

A crate root with no `mod`/`use` and everything public behaves exactly as today.
Existing single-file examples and tests pass unchanged — the crate root module
*is* the old single global scope.

## 11. Open questions

- Crate-root filename convention once `mirin` exists (`main.mrn`/`lib.mrn`?). For
  now the CLI input file is the root.
- `#[path = "…"]` override analog — defer.
- Block-scoped `mod` — defer (rare).
- `Type::method` path addressing: keep `impl_methods` as the dispatch table and
  add `Type::method` as a thin lookup into it, rather than routing methods through
  the module name tables. (Recommended.)
- Cross-module `impl` coherence / orphan rules — defer.
- Prelude opt-out (`#![no_prelude]` analog) — defer.

## 12. Prior art

rustc: `rustc_resolve` (two-phase — build the module graph, then resolve imports
to a fixpoint; late resolution via `Rib` scopes); `rustc_hir::def_id`
(`DefId`/`DefIndex`/`CrateNum`); `rustc_hir::definitions`
(`DefPath`/`DefPathHash`/`DefPathTable`); `rustc_privacy` (separate visibility
pass); `rustc_metadata` (`CStore`, cross-crate metadata — deferred);
`rustc_query_system` + ICH (incremental — partially adopted, single-layer).
`rustc_metadata` / `CStore` and cargo's per-crate orchestration are **not**
mirrored: we compile monolithically (§2), so there is no metadata boundary and
the future `mirin` tool drives one whole-program compile rather than orchestrating
separate ones. Mirin's simplifications over rustc: monolithic (single-layer)
compilation instead of separate compilation + a two-tier incremental scheme; no
macros (so phase 1 needs no expansion/fixpoint); two namespaces instead of three;
and no `mod.rs` duality. Monolithic whole-program compilation also matches how HDL
toolchains tend to elaborate a whole design at once.
