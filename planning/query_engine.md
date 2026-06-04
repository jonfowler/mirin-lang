# Query-based compilation

Polar's compiler today is a staged pipeline of **monolithic whole-crate passes**
(`planning/ir_pipeline.md`): `resolve_file` resolves the entire crate, `typeck`
checks the whole file, and so on. This works for batch `.plr → .sv`, but it does
not give incremental recompilation or the demand-driven evaluation a responsive
language server needs.

This doc is the design for moving the compiler onto a **demand-driven,
incremental query engine** in the shape of rustc's query system / rust-analyzer's
salsa layer. It is the full treatment of the model `planning/modules.md` §8
("single-layer incremental compilation") and `planning/lsp.md` ("Diagnostics and
sharing work with a checker") already gesture at. The stable-identity substrate
those docs call for — `DefId`/`DefPath`/`DefPathHash`/`DefPathTable`,
`resolve.rs` — is the foundation this builds on, and it already exists.

The bet: **bake the query architecture in while the compiler is ~18k lines.**
rustc and rust-analyzer both retrofitted this across mature codebases at great
cost (rust-analyzer is a *second* frontend precisely because retrofitting rustc
was infeasible). We pay almost nothing to do it now, and it is what lets one
in-process engine serve both the batch compiler and the LSP.

## 1. The model

A *query* is a pure function `query(db, key) -> value`, memoized in a central
database. Compilation is a graph of queries over a small set of mutable
**inputs**. Two properties define the system:

- **Demand-driven (lazy).** Nothing computes until something asks for it.
  "Compile the crate" forces the `verilog(def)` query for each emittable item,
  which pulls only the queries on the path to those answers. An LSP request
  (`goto_def`, `hover`) forces only the sliver it needs — most bodies are never
  type-checked.
- **Dynamic dependency tracing.** While a query runs, the engine records every
  input and sub-query it reads. Dependencies are *discovered by execution*, not
  declared. This is what makes incremental invalidation exact.

### 1.1 Inputs

The leaves of the graph, set from outside, never computed. For Polar:

- **File text**, supplied through a **VFS** (`path → (text, revision)`), not
  direct `fs` reads (§5). The batch CLI fills it from disk once; the LSP overlays
  unsaved buffers and bumps the revision on `didChange`.
- **Crate graph / build config** — the set of files, clock-domain configuration.

### 1.2 Red-green and backdating

The engine is revision-based (rustc's "red-green", salsa's algorithm — same
lineage):

- A global **revision** counter; each input mutation bumps it.
- Each memoized value records `verified_at` (last revision we confirmed it
  current) and `changed_at` (last revision its *value* actually changed).
- On demand in a new revision: if every dependency's `changed_at` is no newer
  than this value's `verified_at`, the value is reused untouched. Otherwise it
  re-executes.
- **Backdating is the firewall.** When a query re-executes but produces a value
  *equal to before*, its `changed_at` is **not** advanced — only `verified_at`
  moves up. Dependents compare against `changed_at`, see no change, and do not
  re-run. This is what stops a keystroke in one body from cascading through the
  crate. Equality is the value's `==` (and/or a stable hash — the FNV utility in
  `resolve.rs` is the seed of this).

### 1.3 Durability, cycles, diagnostics

- **Durability** (optional, later): mark high-durability inputs (a future
  stdlib/prelude) so queries reading only those skip revalidation wholesale. Not
  needed for single-crate single-session; the substrate supports adding it.
- **Cycles.** A genuine query cycle (`const A = B; const B = A;`, or a
  self-referential width) is a back-edge on the active query stack. The engine
  detects it and reports a diagnostic rather than diverging. For domain/width
  inference that legitimately needs a fixpoint, the engine can iterate to
  convergence (salsa's `cycle_initial`/`cycle_fn`); default is error-on-cycle.
- **Diagnostics as a side channel.** Errors are *accumulated* during query
  execution (salsa accumulators), not threaded through every return type. A
  `diagnostics(def)` query collects them for a def on demand — this is exactly
  what the LSP's `publishDiagnostics` consumes.

## 2. Stable identity

Incremental reuse is only possible if memo **keys survive edits**. If typing in
function `A` changed the identity of `B`, `B`'s cached results would be discarded
for no reason. The whole game: **identity comes from the program's logical
structure (paths), never its incidental textual layout (byte offsets/indices).**

### 2.1 What "stable" means — and against what

Three layers, stable against different things (all already in `resolve.rs`):

| Layer | What | Stable against |
|---|---|---|
| `DefId` (`CrateNum` + `DefIndex`) | session-local interned integer | nothing across sessions; fast within one |
| `DefPath` (disambiguated name segments) | `crate::util::add` | edits to unrelated siblings; reformatting |
| `DefPathHash` (128-bit) | hash of `(StableCrateId, DefPath)` | across sessions; cross-crate; the persistence key |

"Stable" specifically means: **stable against the edits that should not affect
this identity** — whitespace, comments, edits inside *other* items, edits inside
*this item's body*. It is *not* stable against reordering/inserting same-kind
siblings or moving the item to another module; those are rarer and pay a cost.

### 2.2 Names anchor; position is the fallback

- **Named items anchor to their name.** `crate::add` is robust because adding a
  sibling `sub` does not touch it. Most of a `DefPath` is name segments, which is
  why most identity is very stable. *Rule: name everything that can be named.*
- **Anonymous nodes** (a `uint(N)` width expression, a `when`-arm body, a closure
  analog) have no name, so identity = `(parent path, kind, disambiguator)` — a
  position among same-kind siblings under the same parent. This is the fragile
  case, and `DefPathSegment` already carries the `disambiguator` for it.

### 2.3 Anonymous consts and the body-local rule

A `uint(N)` width whose `N` is an expression (`uint(cfg.bits)`,
`uint(cfg.get_bits())`) is an **anonymous const**: a body that happens to sit
inside a type (see `planning/parametricity.md`). It is given its own `DefId` so
its evaluation can be a separate memoized `const_eval(def)` query; the signature
holds only a stable *reference* to it. Two consequences:

- **Editing the expression keeps the anon-const's identity.** Identity is the
  *slot* it fills ("the width of the return type"), not the text inside it.
  Changing `cfg.get_bits()` to `cfg.get_bits() + 1` re-runs `body`/`const_eval`
  for that anon-const — its `DefId` is unchanged. Any `let`-binding *inside* the
  expression is body-local (an arena `LocalId`, recomputed wholesale on each body
  edit), never a `DefId`.
- **Disambiguate anon-consts by role, not by a flat counter, where the grammar
  allows.** "return-type width" / "param[i] width" is strictly more stable than
  "the n-th anon-const in source order": editing one width never renumbers
  another. `DefPathSegment` needs an anon-const segment kind whose disambiguator
  prefers a structural role and falls back to a parent-scoped counter only for
  genuinely positional cases. **Bake this into the segment representation before
  the identity layer hardens** — retrofitting a new segment kind into
  already-persisted `DefPathHash`es is the migration to avoid.

### 2.4 Owner-relative body ids

Within a body, expressions/locals get arena-local ids meaningful only inside that
body's lowering (`HirId = owner DefId + LocalId`, `modules.md §8` item 3). They
reset per body and are recomputed every time the body is re-lowered — bodies are
atomic (§3.3), so intra-body ids need no cross-edit stability. This keeps volatile
churn out of the global id space entirely. **Status to confirm in code:** the
current loader uses a single crate-wide `NodeId` counter (`surface/loader.rs`),
which is *not* owner-relative; making body ids owner-relative is part of the
migration (§6).

## 3. The query graph for Polar

Data flows top→bottom; an edge `X ▶ Y` means *Y is computed from X*. A batch
compile or an LSP request enters at the **bottom** and pulls **upward** toward
inputs. "High" = near inputs = few dependencies, many dependents. `★` marks a
firewall (a stable projection that absorbs churn from below).

```
[INPUT] file_text(path) via VFS              [INPUT] crate_graph / domain_config
   │
   ▼ parse                                                          (per file)
CST(file)                tree-sitter; error-recovering; reparsed whole-file
   │
   ▼ lower (item summary)            ★ FIREWALL — PURELY SYNTACTIC (per file)
item_tree(file)
   • fn / port / struct signatures AS WRITTEN (type paths unresolved)
   • PORTS = the interface; NO bodies; NO resolved/evaluated types
   • uint(<expr>) widths stored as anon-const REFERENCES, unevaluated
   • stable DefId/DefPath minted for every item AND every anon-const   ◀ resolve.rs
   │
   ▼ name resolution                                ★ FIREWALL (per crate)
crate_def_map(crate)
   • module tree, name → DefId, namespaces, use-imports   ◀ resolve.rs phases 1–2
   • impl-method table  (AddCfg::get_bits → DefId)
   │
   ├──────────────────────┬────────────────────────┬──────────────────────┐
   ▼ (per def, on-demand)  ▼ (per def)              ▼ (per anon-const)      ▼ (features)
sig_of(def)           body(def)               const_eval(def)         goto_def / refs /
RESOLVED signature    lowered EQUATION SYSTEM  compile-time value      hover / completion
• resolves type paths • let / var / when / if  • pulls infer + eval    (compose queries;
• width uint(cfg.b):    • var FEEDBACK lives HERE  of OTHER defs:        refs adds a
   demands const_eval ─┐ • whole-body atomic       sig_of/infer of the   textual prefilter)
                       │                           method it calls
                       └─▶ const_eval(#k) ─▶ infer(get_bits) ─▶ sig_of(get_bits) ─▶ def_map
                          (a SIDEWAYS pull into another def — acyclic unless add's
                           width transitively needs add's own width → CYCLE diagnostic)
   ▼ (per def)
infer(def)   types + clock-domains per node, FUSED (T @clk couples them)
   depends on: body(self) · sig_of(self) · sig_of(instantiated submodules)
             · const_eval(width consts)         ◀ today's hirt/typeck.rs, per-fn
   │
   ▼ (per def)        [monomorphise / block-lowering / flatten sit here as
   │                   per-def or per-instantiation queries — see §6]
   ▼
verilog(def) / diagnostics(def)
   depends on infer(self) + sig_of(submodules)   ← submodule INTERFACE only,
   ◀ svir/lower.rs + emit.rs                        never their bodies
```

### 3.1 The three firewalls

1. **`item_tree(file)` — the master firewall.** Purely syntactic, a pure function
   of one file's text, crate-independent. Editing *inside* a body re-runs
   `parse(file)` but produces a value-equal `item_tree` → backdates → name
   resolution and every other def survive untouched. The invariant, in
   rust-analyzer's words: *"typing inside a function body never invalidates global
   derived data."* Nothing resolved or evaluated may leak into it, or the firewall
   leaks.
2. **`crate_def_map(crate)` — name resolution.** Depends only on the `item_tree`s
   (their *names/structure*), not on bodies or types. A body edit cannot reach it.
   This is *type-independent* resolution only: the module tree, name→`DefId`
   bindings, `use` imports, privacy, and the impl-method **index**
   (`(owner_def, name) → method_def`) and fully-qualified `Type::method` paths.
   Type-directed **method-call dispatch** (`x.foo()`) is *not* here — it needs the
   receiver's inferred type and lives in `infer(def)`, which reads the index.
3. **`sig_of(def)` vs `body(def)` — the signature/body split.** A caller's
   *inference* depends on a callee's *signature*, never its *body*. So editing
   inside `foo` never re-infers callers of `foo`. This is the single most
   important per-def boundary.

### 3.2 Per-def granularity

`sig_of`, `body`, `infer`, `const_eval`, `verilog`, `diagnostics` are all keyed
by `DefId`. That is what bounds invalidation to the thing you touched. `typeck`
is *already* per-fn (`hirt/typeck.rs` runs a per-fn `InferCtxt`), so it maps onto
`infer(def)` almost directly — the largest semantic pass is already shaped right.

### 3.3 The body is atomic

There is no intra-body incremental inference: change anything in a body and the
whole body re-infers. This is forced, not lazy — type/domain inference within a
body is a coupled constraint system (inference variables thread bidirectionally),
and for Polar a body is literally a `var` **equation system** that must be solved
as a unit (`planning/cycles_and_scoping.md`). The body is the smallest
independently-solvable sub-graph, hence the atom. Cost scales with body size, so
keep module bodies decomposable.

### 3.4 Dependent widths and cycles

`uint(cfg.get_bits())` makes `sig_of(add)` depend on `const_eval` → `infer` →
`sig_of` of *another* def. This is a sideways pull, fine because every node is
memoized and the well-formed case is acyclic; the syntactic `item_tree` stays
stable and value-free regardless. A genuine cycle is caught by the query-stack
back-edge check (§1.3). See `planning/parametricity.md` for the const-eval split.

### 3.5 One local crate, not a crate graph

`crate_def_map` is keyed on the **root `SourceFile`**: the whole local repo is a
single crate (the crate is just the first path segment), and `mod foo;` loads a
sibling file *within* it. We deliberately do **not** split the local tree into a
crate graph. This costs no within-session incrementality, because the crate is
not what provides it:

- **Per-def reuse comes from the query DAG, not the crate.** A body edit backdates
  `item_tree`, so `crate_def_map` and every other def's `sig_of`/`infer` are
  known-unchanged. A def in an untouched region is reused exactly as if it lived
  in its own crate — splitting could not make this finer than per-def.
- **"Skip the whole untouched world" is salsa _durability_, an _input_ property —
  not a crate boundary.** Mark local files low-durability and any precompiled deps
  loaded at startup high-durability; a revision that bumps only low-durability
  inputs proves higher-durability queries unchanged *without walking their edges*.
  That is the "side-table checked first" optimisation, keyed per-input (strictly
  more flexible than per-crate). Parked in Q7; salsa gives it for free.

What a crate boundary *would* add is unrelated to within-session reuse: a hard
metadata firewall for consuming **source-less** compiled artifacts, and
cross-session / cross-crate identity (`StableCrateId` → `DefPathHash`). We keep
`StableCrateId` in the identity layer so adding real crates later is
non-breaking, and defer the boundary until we actually load precompiled crates
into the db.

**The one real cost** is item-structure granularity, not bodies: `crate_def_map`
is one query over all files' item-trees, so renaming/adding/removing an *item* in
any file re-runs name resolution for the whole crate (a body edit cannot — the
item_tree firewall absorbs it). This is cheap in practice. If it ever gets hot,
the fix is rust-analyzer's **per-module / block-level def maps** (resolve
sub-trees independently) — a refinement *inside* the one-crate model, preferred
over introducing crate splits.

## 4. Relationship to existing passes

The query graph is *not* a rewrite of the semantics — it is a re-wiring of how
the existing passes are invoked. Each current whole-crate pass becomes a per-def
(or per-file) query whose *body is largely the code that exists today*, narrowed
from "walk the whole crate" to "compute this one key."

| Current pass (`ir_pipeline.md`) | Query | Key | Notes |
|---|---|---|---|
| tree-sitter parse | *(no query)* | — | **transient**: a `tree_sitter::Tree` is not `salsa::Update`, so it can't be a tracked value. Parsing happens *inside* the queries that need the tree (§7). This is where Polar diverges from rust-analyzer, whose rowan trees back a real `parse` query. |
| (new) stable identity | `ast_id_map` | file | per-file `FileAstId`s by hash-of-identity (§2.2). Done (Q1b). |
| (new) item summary | `item_tree` | file | the firewall: lean (name+vis+id, mod nesting, impl method index), no types/bodies. Done (Q1c). |
| `resolve_file` ph.1–2 | `crate_def_map` | crate | already rustc-shaped |
| `lower_to_hir` (per item) | `body` | def | narrow from whole-file to per-def |
| `typeck::check_file` | `infer` | def | **already per-fn** |
| `check_width_obligations` | folded into `infer` / `const_eval` | def | |
| `monomorphise` | `mono_instance` | (def, type-args) | per-instantiation query |
| block-lower / method-lower / out-args / flatten | per-def lowering queries | def | mechanical |
| `lower_to_sv` + `emit_sv` | `verilog` | def / crate | back end, convert last |

## 5. The VFS input boundary

The one new requirement on the compiler regardless of engine choice: inputs
arrive through a `path → (text, revision)` overlay, not `fs` reads. The
`SourceProvider` seam in `surface/loader.rs` is already this boundary in embryo
("the seam a future VFS overlays editor buffers onto"). Concretely the VFS *is*
the set of file-text inputs; bumping a file's revision is what mutates an input
and advances the engine's global revision. This is what lets the batch CLI and
`polar-lsp` share **one in-process engine** with zero serialization or locking
(`planning/lsp.md`).

## 6. Migration tensions in the current code

Three things must change to fit the model; the rest is reuse:

1. **Combined `SourceFile`.** The loader splices every `mod` into one combined
   buffer with a crate-wide `NodeId` counter. The query model wants **per-file**
   `parse`/`item_tree` so a one-file edit invalidates one file. This is the
   biggest structural change: `parse` and `item_tree` become per-file, and the
   crate-wide splice is replaced by `crate_def_map` stitching per-file item-trees
   through the module tree.
2. **Owner-relative ids.** The crate-wide `NodeId` counter must become
   owner-relative (`HirId = DefId + LocalId`, §2.4) so a body edit does not
   renumber siblings.
3. **Whole-crate pass signatures.** Passes take `&SourceFile`/`&Hir` and return
   crate-wide results; they become functions of a single key reading the db.
   `typeck`'s per-fn `InferCtxt` is the template — most passes already have a
   per-item core to lift out.

## 7. Engine choice (decision)

Two viable engines, and one key insight that de-risks the choice:

- **Thin hand-rolled memoization** — a `HashMap<Key, Value>` keyed by `DefId`/file
  with revision-based invalidation, no backdating at first. Minimal, no
  dependency, full control. Enough to establish boundaries and serve a first LSP.
- **The `salsa` crate** (salsa 3.0, what rust-analyzer uses) — gives red-green,
  backdating, durability, cycle fixpoint, interning, and accumulators for free,
  at the cost of macro-heavy `#[salsa::tracked]` plumbing, `'db` lifetimes, and
  tracking an evolving dependency.

**The insight:** the retrofit-expensive, hard-to-reverse work is the **query
boundaries + stable identity + VFS** — *not* the engine. The engine is swappable
behind those boundaries.

**Decision.** Start on a **thin hand-rolled store** (a `HashMap<Key, Value>` per
query + a global revision; coarse invalidation, no backdating yet), but keep all
query **signatures engine-agnostic** so the store underneath is swappable. The
pure-function discipline — every query a function of the db, no hidden mutable
state — is adopted from day one regardless; that is the part worth having
immediately, and it is what makes the later swap mechanical.

**`salsa` was validated by spike (Q0) — VERDICT: adopt it.** The Q0 spike
(`polar-db/src/salsa_spike.rs`, feature `spike-salsa`) reimplemented `parse` on
salsa 0.26 and compiled as written save for one trivial `use salsa::Setter`; the
test confirms automatic memoization (one execution for two calls) and
invalidation on input change. The macro API (`#[salsa::input]` /
`#[salsa::tracked]` / `#[returns(ref)]`) is clean and materially less code than a
hand-written memo table, and using it maximises how directly we can crib from
rust-analyzer (which *is* salsa-based). The spike also **confirmed the CST
wrinkle**: a tracked return value must be `'static + salsa::Update`, which
`tree_sitter::Tree` is not — so `parse` must lower the CST to an owned value and
let the tree stay a transient (the design we wanted regardless). Therefore: the
thin store (`db.rs`) stays only as the Q0 reference/fallback; **Q1 onward builds
on salsa.** The `'db` lifetime (felt first at `item_tree`'s tracked struct) is the
one ergonomic cost to watch.

## 8. Implementation plan

**Strategy: a separate crate, replacing `polar-compiler` when done.** Rationale:
converting the monolithic whole-crate passes is real work that touches the
foundations (per-file `item_tree`, owner-relative ids, splitting the combined
`SourceFile`); doing that *in place* means fighting the existing structures under
a green-build constraint, whereas a fresh crate builds the foundation right and
ports pass *logic* incrementally. The compiler is not in production use, so
letting tests go dark during the rebuild is acceptable. This mirrors the
rust-analyzer↔rustc relationship — a clean reimplementation with the original as
reference — with the difference that we **replace** rather than coexist, so the
old crate is scaffolding we delete at the end, never a permanent second frontend.

Working method:

- **The old `polar-compiler` is a reference oracle.** It stays runnable
  throughout: copy pass logic from it, and run it to clarify expected behaviour on
  any example while building the new system.
- **Use rust-analyzer as the per-stage reference.** Each slice has a direct RA
  analogue (`base-db` VFS, `hir-def` `ItemTree`/`DefMap`, `hir-ty` `infer`); read
  the corresponding RA crate before building ours.
- New crate (name TBD, e.g. `polar-cc`); split into sub-crates later the way RA
  does (`vfs`, `base-db`, `hir-def`, `hir-ty`) if it earns its keep.

Front-to-back slices, each a self-contained chunk that leaves the new crate
building (even while overall behaviour is incomplete):

- **Q0** — new crate skeleton + **VFS** input (`path → (text, revision)`) + thin
  `Db`. Port the tree-sitter parser wrapper. Decide salsa-vs-thin here by spike.
- **Q1** — per-file `parse(file)` + `item_tree(file)` (the syntactic firewall);
  owner-relative ids (`HirId = DefId + LocalId`). No combined `SourceFile`.
- **Q2** — `crate_def_map(crate)` over per-file item-trees: port `resolve.rs`
  (already rustc-shaped) — module tree, namespaces, `use`, privacy.
- **Q3** — `sig_of(def)` / `body(def)` / `infer(def)` per-def (port
  `lower_to_hir` + the per-fn `typeck` core). Anon-const role-based identity
  (§2.3).
- **Q4** — `const_eval(def)` node; dependent widths (`uint(cfg.bits)`) route
  through it; cycle detection.
- **Q5** — back end as per-def / per-instantiation queries (`monomorphise`,
  block/method/out-arg lowering, `flatten`, `verilog`). Driver = "force
  `verilog` for each item". **At this point the new crate is at parity** — switch
  the CLI over and retire `polar-compiler`.
- **Q6** — `diagnostics(def)` accumulator; point `polar-lsp` M2 at the engine.
- **Q7** (deferred) — if still on the thin store: real red-green/backdating,
  durability, optional on-disk cache. If on salsa, these largely come for free.

## 9. Prior art

rustc: `rustc_query_system` (red-green, the query DAG), `rustc_hir::definitions`
(`DefPath`/`DefPathHash` — already mirrored in `resolve.rs`), `type_of`/`fn_sig`
as on-demand queries feeding from HIR lowering, anon-consts for array lengths.
rust-analyzer: `salsa`, the per-file `ItemTree` "invalidation barrier", the
`AstIdMap` for edit-stable syntax-node identity, per-def `Body`/`infer`. salsa:
the red-green algorithm, tracked structs/functions, interning, accumulators,
durability, cycle fixpoint. See also `planning/modules.md` §8,
`planning/lsp.md`, `planning/parametricity.md`, `planning/type_inference.md`.
