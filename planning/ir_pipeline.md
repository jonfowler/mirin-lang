# IR pipeline

Mirin's compiler is a **query-based, demand-driven** pipeline on salsa
(`planning/query_engine.md`): each stage is a tracked query, recomputed only
when its inputs change. This doc is the map; the code in
`packages/mirin-compiler/src/` is the source of truth. (The retired
whole-crate-pass compiler lives in `packages/mirin-compiler-old/`, EOL, kept
for reference only.)

## Overview

```
.mrn text (Vfs overlay)                          base/vfs.rs, base/db.rs
  ─► parse_text          tree-sitter CST          base/parser.rs
  ─► ast_id_map(file)    stable syntax ids        syntax/ast_id.rs
  ─► item_tree(file)     per-file item skeleton   syntax/item_tree.rs   ← syntactic firewall
  ─► syntax_errors(file)                          syntax/syntax_errors.rs
  ─► crate_def_map(crate) module tree, DefIds     nameres/def_map.rs
  ─► per def:
       sig_of(def)        signature types         hir/sig.rs
       body(def)          name-resolved body HIR  hir/body.rs
       infer(def)         types + domains         hir/infer.rs
       check_drivers(def) var driver counts       hir/check.rs
       directions(def)    port-direction checks   hir/check.rs
  ─► sv_module(def)       per-def SV lowering     backend/lower.rs
  ─► sv_file / verilog    assemble + emit         backend/lower.rs, backend/ir.rs
```

The CLI (`main.rs`) forces `verilog` (or prints the CST). `mirin-lsp` forces
the per-def queries for diagnostics, hover, and go-to-definition. A test-only
harness lints every working example with verilator
(`tests/examples.rs::corpus_is_verilator_clean`).

## IRs

### CST — concrete syntax tree
Produced per file by tree-sitter (`base/parser.rs`); owns exact layout
including trivia. Consumed by `ast_id_map`/`item_tree` lowering, by `sig_of`'s
and `body`'s targeted re-parsing, and by editor tooling (`mirin-fmt`,
highlighting).

### Item tree — `syntax/item_tree.rs`
The per-file syntactic firewall: a lean skeleton of the file's items (kinds,
names, nesting) keyed by stable `AstId`s. Body edits that don't change the
skeleton don't invalidate name resolution.

### Def map — `nameres/def_map.rs`
Crate-wide name resolution over the per-file item trees: the module tree
(crate root, synthetic prelude, inline `mod`s), `(name, Namespace)` tables,
`use`-import fixpoint with visibility, the `impl_methods` (inherent),
`trait_methods` (decls), and `trait_impls` (per-trait impl list) indexes, and
stable `DefId ↔ DefPath` identity. See `planning/modules.md`,
`planning/traits.md`.

### Typed-HIR vocabulary — `hir/types.rs`
One term language (Q7, `planning/q7_terms_and_domains.md`): `Type`, `ConstArg`
(width/const position), and `Domain` are the three kinds of `Term`; a generic
argument list is a `Vec<Term>`. Inference variables live in a **single index
space** (`InferVar`) whose kind the inference table tracks. A shared `Folder`
trait owns the structural recursion used by substitution and resolution.
Domains are a component of a value's type (`uint(8) @clk` ≠ `uint(8)`), with
one subtyping edge: `@const` below every clock.

### Body HIR — `hir/body.rs`
Per-def, owner-relative arenas (`ExprId`, `LocalId`), names resolved to
`DefId`/`LocalId`. `var` decls split from their driving equations; method
dispatch deferred to `infer`. Depends on `sig_of(self)` only — never another
def's body.

### SV IR — `backend/ir.rs`
Shallow Verilog-shaped tree (`SvFile` of `SvModule`s with parameters, ports,
items). The emitter is a deterministic pretty-printer that hard-errors on SV
reserved-word collisions.

## Passes (queries)

| Query | File | What it does |
|---|---|---|
| `parse_text` | `base/parser.rs` | tree-sitter parse of one file's text. |
| `ast_id_map(file)` | `syntax/ast_id.rs` | Stable per-file syntax-node ids (name-anchored, position-fallback) so later queries can find their CST node across edits. |
| `item_tree(file)` | `syntax/item_tree.rs` | Lower the CST to the item skeleton. |
| `syntax_errors(file)` | `syntax/syntax_errors.rs` | Collect tree-sitter ERROR/MISSING nodes as diagnostics. |
| `crate_def_map(crate)` | `nameres/def_map.rs` | Module tree + def table + imports + privacy + prelude (rustc's resolver shape, two phases + import fixpoint). The prelude is the synthetic builtins PLUS real source: `src/prelude.mrn` (operator traits + builtin impls), injected into every crate by the vfs and collected into the `$prelude` module. |
| `sig_of(def)` | `hir/sig.rs` | Lower a def's signature from its CST node: generic params (Type/Const/Domain classification), value params with directions/defaults, struct/port fields, return type. Generic args at type references lower kind-directed — named-section args (`DF{clk}`) become real `Domain`/`Const` args aligned with the params by index. Pure fn signatures are **lifted** (implicit `__Dom` appended, stamped over unannotated slots); explicit signatures require domain annotations; unresolved type names diagnose (`SigDiagnostic`). |
| `body(def)` | `hir/body.rs` | Lower + name-resolve the body into the per-def arenas; split `var` decls from equations; record declared types on `let`/`var` locals. An inline-verilog fn (`= verilog { … }`) instead stores a splice-resolved `VerilogTemplate` (`planning/inline_verilog.md`). Diagnoses unresolved names/types, overflowing literals, bad splices, and direction prefixes that disagree with their connector. |
| `infer(def)` | `hir/infer.rs` | Eager-unification walk over `body(def)` against `sig_of` of self + callees (never their bodies — the type-layer firewall). One kinded union-find `InferenceTable` (domain vars carry a `Clock`/`Domain` sort). Domain checking per `domain_checking.md`: `unify` strict, `subsume` (`@const` coercion) at coercion sites, joins at branch/operand merges, record/field domain stamping, the builtin `reg : {dom D: Clock}(self: T @ D, rstn: Reset @ D, init: T @const) -> T @ D`, `when`-clock connection. Undecidable constraints queue as obligations, retried at an end-of-body fixpoint where `const_eval` grounds what it can; true survivors are `const_residuals`. Negative evaluated uint widths reject. Unconstrained domains default to `@const`. Shape checking: positional-arity at calls, unknown/missing/duplicate fields at record constructors, field access on non-aggregates. Trait bounds (`planning/traits.md`): callee predicates instantiate to `Trait` obligations, solved in the same fixpoint (param-env candidates, then impl-header matching; a matched impl's bounds nest at depth+1); method dispatch probes inherent → trait impls → param-env bounds. |
| `const_eval` (helper, `hir/const_eval.rs`) | `hir/const_eval.rs` | Demand-driven interpreter over body HIR (`planning/const_eval.md`): per-local thunks (let / driving equation / call out-connection), memoized with cycle markers; if/else, records, operator-method arithmetic and comparisons on `integer`/bool (i128; `a + b` desugars to `a.add(b)`, matched by method name). Not a salsa query — called from `infer`/backend, deterministic from `body()` inputs. Note: this reaches *callee bodies* (the one deliberate exception to the sig-only firewall, as in rustc CTFE). |
| `check_drivers(def)` | `hir/check.rs` | Per-leaf drive paths (syntactic, pre-type): every `var` is driven, and no two drives overlap (one path a prefix of the other) — for every local kind, params included. Disjoint per-field wiring is legal. |
| `completeness(def)` | `hir/check.rs` | Typed drive completeness (post-infer — field sets need types): a field-driven struct local must cover every leaf; an `out` param must be driven at all. Partially-driven port locals deferred to direction-folding (Q5d). |
| `directions(def)` | `hir/check.rs` | Connection operators agree with port-field / param direction (`=` in, `=>` out). |
| `sv_module(def)` | `backend/lower.rs` | Per-def lowering to one SV module: register recognition (`.reg`), verbatim emission of inline-verilog templates, block/if/when statement-forming, method-call rewriting, out-arg desugaring, aggregate flattening (structs/ports → per-field leaves, domain stamping, direction-aware equation splitting), type-generic monomorphisation at call sites; Const-kind generics bind as instance parameters (`#(.n(8))`) from the per-call instantiations `infer` records (rustc's node substs); a call recorded against a trait-method DECL re-selects to the unique matching impl once the self type is concrete (`Instance::resolve`, `planning/traits.md`). Widths ground through `const_eval` at the type chokepoints; `integer`-typed locals/ports and const-only fns are elided (compile-time only). Width residuals emit as `initial assert`. |
| `sv_file` / `verilog(crate)` | `backend/lower.rs` | Assemble modules deterministically; pretty-print. |
| `reserved_words(crate)` | `backend/reserved.rs` | SV reserved-word table for the emitter's collision check. |

## Prior art

The query graph and per-def granularity follow rust-analyzer (`base-db` →
`hir-def` → `hir-ty`); the term language and single-variable-space inference
table follow chalk; eager-unify-with-deferred-obligations and the
domain-inference split follow rustc / OutsideIn(X). Monomorphisation follows
rustc's collector approach: Type-kind instantiations get fresh specialised
defs; Const-kind args stay polymorphic. See `planning/q7_terms_and_domains.md`
for the Q7 representation/domain plan, `planning/type_inference.md` for typeck
rationale, and `planning/parametricity.md` for const-kind inference vs
monomorphisation.
