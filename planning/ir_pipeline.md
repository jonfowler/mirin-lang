# IR pipeline

Polar's compiler is a staged pipeline. Each IR has a defined role and a fixed set
of passes against it. This doc is the map; the code in
`packages/polar-compiler/src/` is the source of truth.

## Overview

```
crate root .plr
  ─► load_crate                     surface/loader.rs
       (per file: tree-sitter parse → lower_cst, splicing `mod foo;` bodies
        into one combined Surface IR over one combined source buffer)
Surface IR
  ─► resolve_file                   resolve.rs
  ─► check_directions               surface/direction.rs
  ─► lower_to_hir                   hir/lower.rs
HIR (untyped)
  ─► check_drivers                  hir/check_drivers.rs
  ─► typeck::check_file             hirt/typeck.rs
  ─► check_width_obligations        hirt/typeck.rs
HIR (typed)
  ─► monomorphise                   hirtl/monomorphise.rs
HIR (monomorphic)
  ─► lower_block_expressions        hirtl/lower_block_expressions.rs
  ─► lower_method_calls             hirtl/method_lower.rs
  ─► desugar_user_calls             hirtl/out_args.rs
HIR (lowered)
  ─► flatten_aggregates             hirtl/flatten.rs
HIR (flat)
  ─► lower_to_sv                    svir/lower.rs
SV IR
  ─► emit_sv                        svir/emit.rs
.sv text
```

A test-only pass (`verilator_lint.rs`) lints every working example with
verilator.

## IRs

### CST — concrete syntax tree
Produced by tree-sitter. Owns exact layout including trivia. Consumed by the
Surface IR lowering and by editor tooling.

### Loader — `surface/loader.rs`
Turns a crate root `.plr` plus its `mod foo;` declarations into one combined
`SourceFile`. Each file is parsed and lowered, its CST spans offset into one
combined source buffer, with a single crate-wide `NodeId` counter (ids are
crate-unique). File modules (`mod foo;`) are read from `foo.plr` and spliced in
as `ModuleBody::Inline`, so by resolution every module is inline. Source is
supplied through a `SourceProvider` (filesystem by default; in-memory for
tests) — the seam a future VFS overlays editor buffers onto.

### Surface IR — `surface/ir.rs`
Source-shaped AST. Identifiers are textual `String`s carrying spans. Method
calls, named vs. positional arguments, `if`/`when`/block-expressions, and
`var`/`let` distinctions are preserved as written. Inline modules
(`Item::Mod`) nest the same item set; they are a name-resolution scope only and
are flattened away by `lower_to_hir` (and by `check_directions`), so no module
construct survives into HIR.

### HIR — `hir/mod.rs`
First IR structured for semantic analysis.

- Names are resolved to `DefId`/`LocalId`; no later pass looks up identifiers
  by string.
- `HirCall` is the single call shape — operators (`+`, `*`), method calls
  (`.reg`, `.posedge`), and struct constructors all lower here.
- `var` declarations are split from their equations: `HirVarDecl` + `HirEquation`.
- Every expression has a `HirType` slot, filled by `typeck` via the side
  tables `expr_types` / `local_types`.
- `Block` / `If` / `When` start as expressions; the late
  `lower_block_expressions` pass rewrites them into `HirStmt::If` /
  `HirStmt::AlwaysFf` with a synthetic result-local. After block lowering,
  no `Block`/`If`/`When` expressions remain.
- Parametric types (`Struct { def, args }`, `PortTypeRef { def, args }`)
  carry `GenericArgs` — a positional list of `GenericArg::{Type, Const,
  Domain}` matching the def's `generic_params`. Field declarations use
  `ValueKind::Param(i)` inside a `Value` to reference the enclosing item's
  i-th param in type position (the outer `ValueType.domain` carries any
  `@`-annotation), and `HirExprKind::Param(i)` inside `uint(N)` widths to
  reference the same param in const position. Typeck additionally produces
  `ValueKind::Var(_)` placeholders when a parametric callsite needs a
  structural inference variable independent of its domain (e.g. `reg`'s
  `self: A @clk`). All three are substituted out by typeck (Var), the
  monomorphise pass (Type-kind `Param`), and flatten (Const/Domain
  `Param` at struct/port use sites).
- `HirFn::is_prelude` flags synthesised intrinsic signatures (currently
  `reg`). Such fns drive typeck's arg slotting via the general user-fn
  path but are skipped by every later pass — their call sites lower
  inline (e.g. `always_ff`), not as separate SV modules.

### SV IR — `svir/ir.rs`
Shallow Verilog-shaped tree. `SvFile` of `SvModule`s with `parameters`,
`ports`, and `items` (`Logic`, `Assign`, `AlwaysFf`, `AlwaysComb`,
`Instance`). The emitter is a deterministic pretty-printer.

## Passes

### Surface IR

| Pass | File | What it does |
|---|---|---|
| `resolve_file` | `resolve.rs` | Two module-aware phases (rustc's resolver shape). **Phase 1** builds the module tree (`ModuleTree`: crate root, synthetic prelude, and every inline `mod foo { … }`) and the `DefId` table — fns, term-level constructors (`DefKind::Ctor`), modules (`DefKind::Mod`), classifying struct/port params as Type/Const/Domain into `generic_params`. Each module's name table is keyed by `(name, Namespace)` (Module vs Item — modules are split out so `mod df` can coexist with a `df` constructor; a type and its constructor share the Item namespace, so `struct S = S` collides). **Phase 1.5** resolves `use` imports to a fixpoint (for globs and chained imports), inserting import bindings into module tables with priority `Def > Import > Glob`; each binding carries a `Visibility` (a def's declared visibility, a plain `use`'s module-private scope, or a `pub use`'s re-export scope). **Phase 2** resolves every item body against the current module: bare names look up the module's own table (defs + imports) then the prelude; multi-segment paths and `crate`/`super`/`self` anchors resolve through the module tree; builds the per-fn locals table from `let`/`var`/params; populates `impl_methods: (owner_def, method_name) → method_def`; walks `Block`/`If`/`When` with fresh `let` scopes. **Phase 4 (privacy)** then walks every `use` (and path expression), checking each binding the path touches against the use-site module — the ancestor-subtree rule plus `pub`/`pub(crate)`/`pub(super)`/`pub(in …)`, with `pub use` carrying the re-export's visibility. Default is private (visible in the defining module and its descendants); a private item is unnameable from outside that subtree. The prelude (`reg`, `posedge`, `+`, `*`, `Clock`, `Event`, `Type`, `uint`, `bool`) is a module injected at lowest priority. Stable `DefId ↔ DefPath`/`DefPathHash` identity is built over the whole def tree (module-qualified paths). See `planning/modules.md`. |
| `check_directions` | `surface/direction.rs` | Verify connection operators agree with port field direction: `=` for `in`, `=>` for `out`. Reject `=>` on `let`. |

### HIR (untyped)

| Pass | File | What it does |
|---|---|---|
| `lower_to_hir` | `hir/lower.rs` | Bake in name resolution. Desugar method calls into `HirCall` with the method's `DefId`. Slot defaults into call sites. Split `var` decls from equations. Enter each struct/port's `generic_params` scope so field types resolve `A`-style names to `HirTypeKind::Param(i)` and type-application syntax (`Bus(uint(8))`) lowers to populated `GenericArgs`. |
| `check_drivers` | `hir/check_drivers.rs` | Every `var` must have exactly one driver (`Equation` or `=>` source). Zero is undriven; two is multiple-driver. |

### HIR (typed)

| Pass | File | What it does |
|---|---|---|
| `typeck::check_file` | `hirt/typeck.rs` | Eager unification walk. Per-fn `InferCtxt` carries type/domain/const var pools and an obligation queue. Writes back via `expr_types`, `local_types`, `method_resolutions`, `fn_residuals`. Queues `ConstEq` (normalised) obligations for symbolic widths; the fixpoint `discharge_obligations` pass at end-of-fn simplifies them via current bindings. Use sites of parametric defs allocate fresh `GenericArgs` (`fresh_args_for_def`) and substitute via `instantiate` (rustc's `EarlyBinder::instantiate` shape); call sites build the same `Substitution { args: GenericArgs, domain_locals }` shape with fresh inference variables, and propagate the callee's surviving residual constraints through the call. See `planning/type_inference.md` and `planning/parametricity.md`. |
| `check_width_obligations` | `hirt/typeck.rs` | Discharge `WidthEq` obligations where both sides are now ground. Surviving residuals carry forward (currently no-op; ready for parametric widths). |

### HIR (typed) → HIR (monomorphic)

| Pass | File | What it does |
|---|---|---|
| `monomorphise` | `hirtl/monomorphise.rs` | For every call whose callee has at least one Type-kind generic param, synthesise a specialised `HirFn` with the Type-kind args substituted out and rewrite the call to point at the spec's `DefId`. Const-kind and Domain-kind params stay polymorphic — only Type is monomorphised. The spec's name is `<orig>__<mangled_types>` (e.g. `pipeline_para__Write`); its `HirParam` list drops the entries whose name matched a substituted Type generic, and the rewritten call has the corresponding arg slots removed so the shape lines up. Reads typeck's `call_generics` side table; clones expr_types/local_types/method_resolutions for the spec's body with fresh `HirId`s and substituted types. Original Type-kind-generic fns stay in the HIR but are skipped by `lower_to_sv` (no SV construct matches a type-polymorphic module). See `planning/parametricity.md`. |

### HIR (monomorphic) → HIR (lowered)

| Pass | File | What it does |
|---|---|---|
| `lower_block_expressions` | `hirtl/lower_block_expressions.rs` | Rewrite `Block`/`If`/`When` expressions into statement-form. `if` becomes `HirStmt::If` with each branch ending in `Equation { lhs: __block_N, rhs: tail }`. `when` becomes `HirStmt::AlwaysFf { clock, dest, d_input }`. The synthetic `__block_N` local replaces the original expression position. |
| `lower_method_calls` | `hirtl/method_lower.rs` | Rewrite `HirExprKind::MethodCall` into a regular `HirCall` against the `DefId` from `method_resolutions`. No method-call shape remains after this. |
| `desugar_user_calls` | `hirtl/out_args.rs` | Rewrite user-fn calls into expression-statement position with binding leaves passed as out-args. `sv_lower` then emits each as one SV instance. |

### HIR (lowered) → HIR (flat)

| Pass | File | What it does |
|---|---|---|
| `flatten_aggregates` | `hirtl/flatten.rs` | Erase port and struct types at value positions. Each aggregate local splits into per-field locals named `p__field` (recursive for nested aggregates). For parametric aggregates, `instantiate_type` substitutes the receiver's `GenericArgs` into `ValueKind::Param(i)` (type position) and `HirExprKind::Param(i)` inside `uint(N)` widths (const position); Domain-kind args are pre-resolved into a `LocalId → Domain` map for `Domain::Clock(local)` lookups. Struct and single-domain-port instances stamp their `domain` over each field's `Unspecified` slot via `apply_struct_domain` / `apply_port_domain`. Whole-aggregate equations split into per-field equations with direction-aware LHS/RHS pairing (port field directions compose with the param's `in`/`out` direction). LocalId remap is owned by an `expansion: HashMap<LocalId, Vec<Leaf>>` table consulted by downstream rewrites — including the `clock` and `dest` fields of `HirStmt::AlwaysFf`. |

### SV

| Pass | File | What it does |
|---|---|---|
| `lower_to_sv` | `svir/lower.rs` | Walk flattened HIR, build SV IR. `HirStmt::AlwaysFf` → `SvItem::AlwaysFf` (reset-less or with reset clause). `HirStmt::If` → `SvItem::AlwaysComb` with `SvCombIf`. Method-derived modules get names `<owner>__<method>` to avoid SV reserved-word collisions. Phase D residuals from typeck become `SvItem::InitialAssert { cond: lhs == rhs }` items on the matching module — elaboration-time checks for constraints that survived monomorphic discharge. |
| `emit_sv` | `svir/emit.rs` | Deterministic pretty-printer. Hard-errors on any user identifier that collides with an SV reserved word. |

### Test-only

| Pass | File | What it does |
|---|---|---|
| Verilator lint | `verilator_lint.rs` | Pipe each working example through `verilator --lint-only`. Per-file `// verilator: …` directives inject parameter values or suppress specific warnings. Hard-fails if verilator isn't installed. |

## Prior art

The IR-per-phase shape and the eager-unify-with-deferred-obligations split
follow rustc directly. AST → HIR → THIR → MIR maps onto Surface IR → HIR
(untyped) → HIR (typed) → HIR (monomorphic) → HIR (lowered) → HIR (flat).
Monomorphisation specifically follows rustc's collector / shimming
approach: each Type-kind instantiation produces a fresh `DefId` and a
specialised body; Const-kind args stay polymorphic the way rustc's const
generics also do. The block/if late-flattening is rustc-style: a
tree-shaped form survives through type checking, and a late pass
introduces synthetic locals and statement-form control flow. The domain
solver is structured per OutsideIn(X) — one constraint generator,
separate solvers for types and domains. See `planning/type_inference.md`
for the typeck design rationale and `planning/parametricity.md` for the
const-kind inference / monomorphisation split.
