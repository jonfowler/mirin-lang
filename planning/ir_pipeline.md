# IR pipeline

Polar's compiler is a staged pipeline. Each IR has a defined role and a fixed set
of passes against it. This doc is the map; the code in
`packages/polar-compiler/src/` is the source of truth.

## Overview

```
source.plr
  ─► tree-sitter parse              parser/tree_sitter.rs
CST
  ─► lower_cst                      surface_ir.rs
Surface IR
  ─► resolve_file                   resolve.rs
  ─► check_directions               direction.rs
  ─► lower_to_hir                   hir/lower.rs
HIR (untyped)
  ─► check_drivers                  hir/check_drivers.rs
  ─► typeck::check_file             typeck.rs
  ─► check_width_obligations        typeck.rs
HIR (typed)
  ─► lower_block_expressions        hir/lower_block_expressions.rs
  ─► lower_method_calls             hir/method_lower.rs
  ─► desugar_user_calls             hir/out_args.rs
HIR (lowered)
  ─► flatten_aggregates             hir/flatten.rs
HIR (flat)
  ─► lower_to_sv                    sv_lower.rs
SV IR
  ─► emit_sv                        sv_emit.rs
.sv text
```

A test-only pass (`verilator_lint.rs`) lints every working example with
verilator.

## IRs

### CST — concrete syntax tree
Produced by tree-sitter. Owns exact layout including trivia. Consumed by the
Surface IR lowering and by editor tooling.

### Surface IR — `surface_ir.rs`
Source-shaped AST. Identifiers are textual `String`s carrying spans. Method
calls, named vs. positional arguments, `if`/`when`/block-expressions, and
`var`/`let` distinctions are preserved as written.

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

### SV IR — `sv_ir.rs`
Shallow Verilog-shaped tree. `SvFile` of `SvModule`s with `parameters`,
`ports`, and `items` (`Logic`, `Assign`, `AlwaysFf`, `AlwaysComb`,
`Instance`). The emitter is a deterministic pretty-printer.

## Passes

### Surface IR

| Pass | File | What it does |
|---|---|---|
| `resolve_file` | `resolve.rs` | Build `DefId` table for top-level items and impl methods. Build per-fn locals table from `let`/`var`/params. Seed the prelude (`reg`, `posedge`, `+`, `*`, `Clock`, `Event`, `uint`, `bool`). Populate `impl_methods: (owner_def, method_name) → method_def`. Walk `Block`/`If`/`When` with fresh `let` scopes. |
| `check_directions` | `direction.rs` | Verify connection operators agree with port field direction: `=` for `in`, `=>` for `out`. Reject `=>` on `let`. |

### HIR (untyped)

| Pass | File | What it does |
|---|---|---|
| `lower_to_hir` | `hir/lower.rs` | Bake in name resolution. Desugar method calls into `HirCall` with the method's `DefId`. Slot defaults into call sites. Split `var` decls from equations. |
| `check_drivers` | `hir/check_drivers.rs` | Every `var` must have exactly one driver (`Equation` or `=>` source). Zero is undriven; two is multiple-driver. |

### HIR (typed)

| Pass | File | What it does |
|---|---|---|
| `typeck::check_file` | `typeck.rs` | Eager unification walk. Per-fn `InferCtxt` carries type/domain var pools and an obligation queue. Writes back via `expr_types`, `local_types`, `method_resolutions`. Queues `WidthEq` obligations for symbolic widths. See `planning/type_inference.md`. |
| `check_width_obligations` | `typeck.rs` | Discharge `WidthEq` obligations where both sides are now ground. Surviving residuals carry forward (currently no-op; ready for parametric widths). |

### HIR (typed) → HIR (lowered)

| Pass | File | What it does |
|---|---|---|
| `lower_block_expressions` | `hir/lower_block_expressions.rs` | Rewrite `Block`/`If`/`When` expressions into statement-form. `if` becomes `HirStmt::If` with each branch ending in `Equation { lhs: __block_N, rhs: tail }`. `when` becomes `HirStmt::AlwaysFf { clock, dest, d_input }`. The synthetic `__block_N` local replaces the original expression position. |
| `lower_method_calls` | `hir/method_lower.rs` | Rewrite `HirExprKind::MethodCall` into a regular `HirCall` against the `DefId` from `method_resolutions`. No method-call shape remains after this. |
| `desugar_user_calls` | `hir/out_args.rs` | Rewrite user-fn calls into expression-statement position with binding leaves passed as out-args. `sv_lower` then emits each as one SV instance. |

### HIR (lowered) → HIR (flat)

| Pass | File | What it does |
|---|---|---|
| `flatten_aggregates` | `hir/flatten.rs` | Erase port and struct types at value positions. Each aggregate local splits into per-field locals named `p__field` (recursive for nested aggregates). Whole-aggregate equations split into per-field equations with direction-aware LHS/RHS pairing. LocalId remap is owned by an `expansion: HashMap<LocalId, Vec<Leaf>>` table consulted by downstream rewrites — including the `clock` and `dest` fields of `HirStmt::AlwaysFf`. |

### SV

| Pass | File | What it does |
|---|---|---|
| `lower_to_sv` | `sv_lower.rs` | Walk flattened HIR, build SV IR. `HirStmt::AlwaysFf` → `SvItem::AlwaysFf` (reset-less or with reset clause). `HirStmt::If` → `SvItem::AlwaysComb` with `SvCombIf`. Method-derived modules get names `<owner>__<method>` to avoid SV reserved-word collisions. |
| `emit_sv` | `sv_emit.rs` | Deterministic pretty-printer. Hard-errors on any user identifier that collides with an SV reserved word. |

### Test-only

| Pass | File | What it does |
|---|---|---|
| Verilator lint | `verilator_lint.rs` | Pipe each working example through `verilator --lint-only`. Per-file `// verilator: …` directives inject parameter values or suppress specific warnings. Hard-fails if verilator isn't installed. |

## Prior art

The IR-per-phase shape and the eager-unify-with-deferred-obligations split
follow rustc directly. AST → HIR → THIR → MIR maps onto Surface IR → HIR
(untyped) → HIR (typed) → HIR (flat). The block/if late-flattening is
rustc-style: a tree-shaped form survives through type checking, and a late pass
introduces synthetic locals and statement-form control flow. The domain
solver is structured per OutsideIn(X) — one constraint generator, separate
solvers for types and domains. See `planning/type_inference.md` for the
typeck design rationale.
