# Q3 plan — typed HIR: `sig_of` / `body` / `infer`

Q3 is the chunky slice: it ports the typed-HIR core of the old compiler
(`lower_to_hir` + `typeck`) onto per-def queries, behind the signature/body
firewall (`query_engine.md` §3.1 firewall 3). This doc inventories **every** old
pass and says where it goes, then details the Q3 sub-slices.

Grounded in a read of the current code: `hir/lower.rs`, `hir/mod.rs`,
`hirt/typeck.rs`, `hirt/normal_const.rs`, `hir/check_drivers.rs`,
`surface/direction.rs`, and the downstream `hirtl/*` + `svir/*`.

## 0. The load-bearing fact

When the old `typeck` infers function `F` that calls `G` (or constructs struct
`S`, or accesses a field of port `P`), it reads **only the callee's signature** —
`params`, `return_type`, `generic_params`, field declarations — plus `G`'s
**residual** constraints (`fn_residuals`, keyed by callee `DefId`). It never
touches the callee's body (`build_sig_subst` / `fresh_args_for_def` /
`instantiate` in `typeck.rs`; confirmed no body access). Likewise `lower_to_hir`
gets all cross-def info from the *surface signature* + resolver tables, never
from another def's HIR.

So the firewall is real and the dependency edges are:

```
sig_of(F)   ← crate_def_map · (signature types of F)
body(F)     ← crate_def_map · sig_of(F)
infer(F)    ← body(F) · sig_of(F) · sig_of(callees/structs/ports) · sig_residuals(callees)
                       · const_eval(widths)   [Q4]
```

Editing inside `F`'s body re-runs `body(F)`/`infer(F)` only; callers of `F` (which
depend on `sig_of(F)`) are untouched. This is the single most valuable boundary.

## 1. Full pass inventory → query mapping

Every pass in `ir_pipeline.md`, where it lands, and how it changes. Slice column:
**Q2 done**, **Q3** (this slice), **Q4** (const-eval), **Q5** (back end).

| # | Old pass (file) | Today's shape | Target query | Key | Slice | How it changes |
|--|--|--|--|--|--|--|
| 1 | `load_crate` (`surface/loader.rs`) | whole-crate splice into one combined buffer + crate-wide `NodeId` | — (deleted) | — | Q0–Q2 ✓ | Replaced by the VFS + `SourceRoot` input + `crate_def_map` stitching per-file `item_tree`s. The combined buffer and crate-wide id counter are gone. |
| 2 | tree-sitter parse | per call | *(transient, no query)* | file | Q0 ✓ | Parses inside any query needing the tree (§7 CST wrinkle). |
| 3 | (new) stable ids | — | `ast_id_map` | file | Q1 ✓ | hash-of-identity `FileAstId`s. |
| 4 | (new) item summary | — | `item_tree` | file | Q1 ✓ | lean firewall (name+vis+id+ctor+impl methods+use-tree); **no types/bodies**. |
| 5 | `resolve_file` ph.1/1.5/4 (`resolve.rs`) | whole-crate | `crate_def_map` | crate (`SourceRoot`) | Q2 ✓ | module tree, `{Module,Item}` namespaces, `use` fixpoint, privacy, ctor + impl-method index, `DefPath`/`DefPathHash`. |
| 5b | `resolve_file` ph.2 *(body name resolution)* | whole-crate | folded into `body(def)` lowering + a `resolve_in_scope` primitive on the def map | def | **Q3** | Bare names / paths inside a body resolve through the module's table → prelude → (Q3a adds the prelude + the in-scope lookup deferred from Q2c). Locals come from the body itself. |
| 6 | `check_directions` (`surface/direction.rs`) | whole-file; resolution-only, **no types** | `directions(def)` diagnostic query (or fold into `body`) | def | **Q3** | Already structural; becomes a per-def check reading resolution. Could stay a surface-level check or move onto HIR — see Open Q. |
| 7 | `lower_to_hir` (`hir/lower.rs`) | whole-file, per-item core | **split** → `sig_of(def)` + `body(def)` | def | **Q3** | The signature half (params, return, field types, `generic_params`, param `LocalId`s) becomes `sig_of`; the statement/equation/expr tree becomes `body`. `HirId`/`LocalId` go **owner-relative**. |
| 8 | `check_drivers` (`hir/check_drivers.rs`) | per-fn, body-only, pre-typeck | `check_drivers(def)` diagnostic query | def | **Q3** | Mechanical lift — already per-fn over `body.statements`. |
| 9 | `typeck::check_file` (`hirt/typeck.rs`) | per-fn `InferCtxt`, file-scoped `FileCtx` | `infer(def)` | def | **Q3** | Already per-fn. `FileCtx` lookups (`fns`/`structs`/`ports`) become `sig_of(callee)` queries; `fn_residuals` becomes `sig_residuals(callee)` (so callers depend on sig, not body). Writes `expr_types`/`local_types`/`method_resolutions`/`call_generics`/residuals — all per-def now. |
| 10 | `check_width_obligations` (`hirt/typeck.rs`) | post-typeck, over per-fn residuals | folded into `infer(def)`; ground-width folding → `const_eval(def)` | def | **Q3** (ground) / **Q4** (symbolic) | Ground obligations discharge inside `infer`; symbolic/dependent widths route through `const_eval` in Q4. |
| 11 | `monomorphise` (`hirtl/monomorphise.rs`) | per-call, fresh `DefId` per Type instantiation | `mono_instance(def, type-args)` | (def, type-args) | Q5 | per-instantiation query; reads `call_generics`. |
| 12 | `lower_block_expressions` (`hirtl/…`) | per-fn | per-def lowering query | def | Q5 | reads `expr_types`/`local_types` to size synthetic locals. |
| 13 | `lower_method_calls` (`hirtl/method_lower.rs`) | per-fn (+ callee param shapes) | per-def lowering query | def | Q5 | reads `method_resolutions` + callee `sig_of`. |
| 14 | `desugar_user_calls` (`hirtl/out_args.rs`) | whole-file (needs which fns return) | per-def, reading `sig_of` | def | Q5 | "has a return type" is signature info → per-def once it reads `sig_of`. |
| 15 | `flatten_aggregates` (`hirtl/flatten.rs`) | per-fn (+ struct/port defs) | per-def lowering query | def | Q5 | reads struct/port `sig_of` + `expr_types`/`local_types`. |
| 16 | `lower_to_sv` (`svir/lower.rs`) | per-fn (+ prelude defs) | `verilog(def)` | def | Q5 | per-module; reads submodule **interface** (`sig_of`) only. |
| 17 | `emit_sv` (`svir/emit.rs`) | whole-file pretty-print + reserved-word check | crate-level `emit` (+ per-def `verilog`) | crate/def | Q5 | reserved-word validation stays whole-crate. |
| — | verilator lint (`verilator_lint.rs`) | test-only | unchanged | — | — | stays an external test. |

## 2. The three Q3 queries in detail

### `sig_of(def) -> Signature`
The "signature layer" RA keeps separate from the item summary. A pure function of
the def's **syntactic signature** + `crate_def_map` (to resolve type paths to
`DefId`s). Produces: lowered `params` (with owner-relative param `LocalId`s),
`return_type`, struct/port field types, and `generic_params` (Type/Const/Domain),
with `ValueKind::Param(i)` / `Domain::Param(i)` / `HirExprKind::Param(i)` slots for
generic refs. **No body.** Prelude defs (`reg`, `posedge`, `+`, `*`, builtin
types) get synthesised signatures here (the old `HirFn::is_prelude` path).

Source of the signature syntax: lower transiently from the CST (re-parse inside
the query, like `item_tree` does) — keeping `item_tree` the lean firewall rather
than fattening it with types (Open Q below).

### `body(def) -> Body`
The statement/equation/expr tree for a fn body, with **owner-relative** `HirId`s
and `LocalId`s (today's crate-global `next_hir_id` becomes per-owner — the §2.4
migration). Bakes in name resolution: bare names/paths → `DefId`/`LocalId` via the
def map's in-scope lookup; `let`/`var`/`=>` locals from the body; `var` split into
`HirVarDecl` + `HirEquation`; method calls left as deferred `MethodCall`
(resolved in `infer`). Depends on `crate_def_map` + `sig_of(self)` (for param
`LocalId`s and the generic scope).

### `infer(def) -> Inferred`
Per-fn `InferCtxt` lifted almost verbatim (it is already per-fn). Depends on
`body(self)`, `sig_of(self)`, `sig_of` of every callee/struct/port it touches, and
their `sig_residuals`. Produces `expr_types`, `local_types`, `method_resolutions`,
`call_generics`, and this def's own residuals. Method dispatch reads the receiver's
inferred type + `crate_def_map.impl_method(owner, name)` (owner = struct/port def,
or a prelude builtin-type `DefId` for `uint`/`bool`/`Clock`). Symbolic widths queue
obligations; ground ones discharge here, the rest become residuals (and, in Q4,
route through `const_eval`).

## 3. Migration items Q3 forces

1. **Owner-relative ids.** `HirId`/`LocalId` are file-global in `lower.rs`
   (`next_hir_id`); they must reset per owner so a body edit cannot renumber
   another def (`query_engine.md` §2.4, §6.2).
2. **Signature layer.** `item_tree` deliberately holds no types; `sig_of` is the
   new layer that lowers signature *types*. Pick its input source (Open Q).
3. **Prelude + in-scope lookup.** Deferred from Q2c. `crate_def_map` (or a thin
   query over it) gains a synthetic prelude module and a `name → res` lookup that
   walks the module's own table → prelude (no ancestor walk for bare names, per
   the old resolver). Prelude builtin **types** need `DefId`s so method dispatch
   can key on them.
4. **`sig_residuals(def)`.** Carve the residual set out as signature output so a
   caller's `infer` depends on the callee's *signature*, never its body.

## 4. Sub-slices (each leaves mirin-db building + tested)

- **Q3a — prelude + in-scope resolution.** Add the synthetic prelude module
  (builtin type `DefId`s + intrinsic fn names) to `crate_def_map`, and a
  `resolve_in_scope(module, name, ns)` primitive (own table → prelude). Small,
  unblocks everything. Tests: prelude name resolves; user name shadows prelude.
- **Q3b — `sig_of(def)`.** Owner-relative param ids, signature type lowering,
  `generic_params`, synthesised prelude signatures. Tests: param/return/field
  types; a body edit leaves `sig_of` value-equal (firewall); generic slots.
- **Q3c — `body(def)`.** Owner-relative body HIR, name-resolved, var/equation
  split, deferred method calls. Tests: locals/shadowing; `var` driver shape;
  paths-only name refs resolve.
- **Q3d — `infer(def)`.** Lift the per-fn `InferCtxt`; `FileCtx` lookups become
  `sig_of` queries; residuals as `sig_residuals`; ground-width discharge; literal
  `const_eval` stub (symbolic → Q4). Tests: scalar/struct/port inference; method
  dispatch; a caller re-infers iff a callee's *signature* changed, not its body.
- **Q3e — per-def checks.** `check_drivers(def)`, `check_directions` (def or
  surface), ground `check_width_obligations` as diagnostics on the def map /
  `infer`. Tests: undriven/multiple-driver; direction mismatch.

## 5. Deferred (boundaries this plan must respect)

- **Q4 — `const_eval(def)` + dependent widths.** `uint(cfg.bits)` anon-const
  bodies get their own `DefId`s using the already-baked `DefPathSegmentKind::
  AnonConst` (§2.3); `infer` of one def pulls `const_eval` of another → the
  sideways edge; cycle detection via the query stack.
- **Q5 — back end.** monomorphise (per-instantiation), block/method/out-arg
  lowering + flatten (per-def), `verilog(def)` + crate `emit`. At parity the CLI
  switches over and `mirin-compiler` is retired.

## 6. Resolved decisions

1. **Signature source — lower types transiently from the CST inside `sig_of`;
   do not fatten `item_tree`.** `sig_of` must resolve type paths (`uint`, `Bus`,
   `Clock`) to `DefId`s through `crate_def_map`, which is impossible at
   `item_tree` time (pre-name-resolution) — so the type lowering has to live in a
   post-def-map query regardless. Putting it in `sig_of` keeps `item_tree` the
   lean firewall and mirrors rust-analyzer's separate signature layer. Re-parsing
   is cheap and already transient elsewhere.
2. **`check_directions` — a per-def `directions(def)` query over `body(def)` +
   callee `sig_of`, type-independent.** It matches a call's connection operators
   (`=` vs `=>`, in the body) against the callee's port/fn field directions (in
   the signature), so it needs both — but **no types**. It therefore sits in Q3e
   beside `check_drivers`, ahead of `infer`. (There is no "surface IR" stage to
   host it anymore; it becomes a body-level structural check.)
3. **Owner-relative ids — per-body arena (RA shape).** `body(def)` returns a
   `Body` owning its own `Arena<Expr>` / locals, with `ExprId`/`LocalId` as arena
   indices reset to 0 per body. The owner is implicit in *which* `body(def)` was
   queried; cross-body references carry the `DefId`. A body edit rebuilds one
   arena and renumbers nothing else — no global counter (mirrors RA's `Body` +
   `ExprId`).
4. **Domain — a *component* of the type, not a separate solve.** The domain lives
   inside each node's type (`ValueType { kind, domain }`; the "FUSED" note in
   `query_engine.md` §3) and is produced by the same `infer(def)` walk, not a
   parallel pass. It carries its **own constraint set** — a subtyping lattice
   (`@const` is a supertype of every concrete clock; an unconstrained domain var
   compacts to `@const`, MLsub-style) with a `Clock`-kind bound for register-like
   ops — kept distinct from the structural type-equality machinery, because
   domains form a lattice rather than participating in unification
   (`domain_checking.md`). The slogan: *one constraint generator, a lattice solve
   for the domain component of the type* — not "domains inferred entirely
   separately."
