# IR pipeline

Polar's compiler is a **query-based, demand-driven** pipeline on salsa
(`planning/query_engine.md`): each stage is a tracked query, recomputed only
when its inputs change. This doc is the map; the code in
`packages/polar-compiler/src/` is the source of truth. (The retired
whole-crate-pass compiler lives in `packages/polar-compiler-old/`, EOL, kept
for reference only.)

## Overview

```
.plr text (Vfs overlay)                          base/vfs.rs, base/db.rs
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

The CLI (`main.rs`) forces `verilog` (or prints the CST). `polar-lsp` forces
the per-def queries for diagnostics, hover, and go-to-definition. A test-only
harness lints every working example with verilator
(`tests/examples.rs::corpus_is_verilator_clean`).

## IRs

### CST — concrete syntax tree
Produced per file by tree-sitter (`base/parser.rs`); owns exact layout
including trivia. Consumed by `ast_id_map`/`item_tree` lowering, by `sig_of`'s
and `body`'s targeted re-parsing, and by editor tooling (`polar-fmt`,
highlighting).

### Item tree — `syntax/item_tree.rs`
The per-file syntactic firewall: a lean skeleton of the file's items (kinds,
names, nesting) keyed by stable `AstId`s. Body edits that don't change the
skeleton don't invalidate name resolution.

### Def map — `nameres/def_map.rs`
Crate-wide name resolution over the per-file item trees: the module tree
(crate root, synthetic prelude, inline `mod`s), `(name, Namespace)` tables,
`use`-import fixpoint with visibility, `impl_methods` index, and stable
`DefId ↔ DefPath` identity. See `planning/modules.md`.

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
| `crate_def_map(crate)` | `nameres/def_map.rs` | Module tree + def table + imports + privacy + prelude (rustc's resolver shape, two phases + import fixpoint). |
| `sig_of(def)` | `hir/sig.rs` | Lower a def's signature from its CST node: generic params (Type/Const/Domain classification), value params with directions/defaults, struct/port fields, return type. Pure fn signatures are **lifted** (implicit `__Dom` appended, stamped over unannotated slots); explicit signatures require domain annotations (`SigDiagnostic`). |
| `body(def)` | `hir/body.rs` | Lower + name-resolve the body into the per-def arenas; split `var` decls from equations; record declared types on `let`/`var` locals. |
| `infer(def)` | `hir/infer.rs` | Eager-unification walk over `body(def)` against `sig_of` of self + callees (never their bodies — the type-layer firewall). One kinded union-find `InferenceTable` (domain vars carry a `Clock`/`Domain` sort). Domain checking per `domain_checking_redux.md`: `unify` strict, `subsume` (`@const` coercion) at coercion sites, joins at branch/operand merges, record/field domain stamping, the builtin `reg : {dom D: Clock}(self: T @ D, rstn: Reset @ D, init: T @const) -> T @ D`, `when`-clock connection. Undecidable constraints queue as obligations, retried at an end-of-body fixpoint; survivors are `const_residuals`. Unconstrained domains default to `@const`. |
| `check_drivers(def)` | `hir/check.rs` | Every `var` has exactly one driver. |
| `directions(def)` | `hir/check.rs` | Connection operators agree with port-field / param direction (`=` in, `=>` out). |
| `sv_module(def)` | `backend/lower.rs` | Per-def lowering to one SV module: register recognition (`.reg`), block/if/when statement-forming, method-call rewriting, out-arg desugaring, aggregate flattening (structs/ports → per-field leaves, domain stamping, direction-aware equation splitting), type-generic monomorphisation at call sites. Width residuals emit as `initial assert`. |
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
