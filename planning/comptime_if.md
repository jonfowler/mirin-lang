# Compile-time `const if`

Status: steps 1–5 landed (parse, HIR, type-check, grounded fold, and the symbolic
`generate if` lowering — `SvItem::GenerateIf`, 2026-06-26, slice_guards.md
Phase 4). A symbolic-const-generic condition now generates; only a *runtime*
(clocked) condition is rejected.

`const if cond { … } else { … }` is a **compile-time conditional**: `cond` is a
constant expression resolved at elaboration, and only the *selected* arm is
elaborated. The discarded arm may be invalid for a given instantiation — an
out-of-range slice when a width folds to 0 — which a normal `if` (a mux over
both arms) could not express. It is the mechanism the zero-width slice/concat
guard needs (planning/slicing.md).

## Why a distinct construct, not "lift any const-condition `if`"

The behaviour that a normal `if` cannot have is **not elaborating the dead arm**:
its width/bounds obligations are not enforced, and the arms need not share
internal structure. That is a real semantic difference, not an optimisation, so
it is explicit:

- a normal `if` is a mux — both arms elaborate, both must type the same, both
  arms' obligations are enforced;
- making it implicit (treat any const-condition `if` as compile-time) would make
  *whether your dead arm is protected* depend on an inferred property; a refactor
  that made the condition runtime would silently turn the guard back into a mux
  and newly reject the dead arm — action at a distance.

**Scope of "obligations not enforced" (v1).** The drop happens at **backend
elaboration**: the grounded fold emits only the taken arm, so a *generic* dead
arm's deferred width/bounds obligations (e.g. an out-of-range slice when a width
folds to 0 — the motivating case) are never checked. They are **not** dropped at
infer time: both arms are type-checked, so an *eager* check on a **non-generic**
dead arm still fires (`const if 1==1 { a } else { uint(8)::999 }` errors on the
dead `999`). Generic dead arms — what the slice/concat guards rely on — defer and
are fine; the eager-literal gap on non-generic dead arms is a known v1 limitation
(full infer-time obligation scoping is future work).

Precedent is uniformly explicit: C++ `if constexpr`, D `static if`, Zig
`comptime`, and SV's own `generate if`.

## Semantics

- **Condition must be constant.** It is rejected if it carries a clock domain
  (`Domain::Clock` or a domain-generic `Domain::Param` — i.e. depends on runtime
  data): `InferDiagnosticKind::ConstIfRuntimeCond`. `Const`/`Unspecified`/`Infer`
  domains are left to lowering.
- **Typed like `if`** — the condition is inferred and both arms unify with the
  result, so the value type is well-defined regardless of which arm an
  instantiation selects. (Divergent-type arms are a future extension; for the
  slice guard both arms are the same `bits(w)`.)
- **Only the selected arm is elaborated** — see lowering.

## Lowering — two modes by whether the condition is known at emit

- **grounded** (landed): the condition closes to a constant at emit
  (`const_eval::eval_cond`) — a literal, or a const generic that has been
  inlined to a concrete value. Emit only the taken arm (`block_value` /
  `block_leaves`); the other is never produced, so its (possibly invalid) SV
  never reaches the tool. No new SV node.
- **symbolic** (step 5, **landed** 2026-06-26): the controlling const generic
  rides out as an SV `#()` parameter, so the condition is still symbolic at emit.
  It lowers to `SvItem::GenerateIf` (sibling of `SvItem::GenerateFor`) — SV §27.5
  instantiates only the selected generate block, so the dead arm is never
  elaborated. The value-position lowering (`const_if_generate`) declares a result
  wire and drives it per branch, each branch's own items captured into its
  generate block. (slice_guards.md Phase 4.)

## Implementation map

- grammar: `const_if_expression` (value form; both arms required), in the
  expression choice next to `if_expression`.
- HIR: `ExprKind::ConstIf { cond, then_branch, else_branch }`, lowered in
  `body.rs` alongside `if_expression`.
- infer: typed like `If` + the const-condition domain check.
- check / const_eval: folded into the existing `If` arms (or-patterns) — driver
  counting and const evaluation treat both the same.
- backend: `expr_value` / `expr_leaves` fold via `eval_const_cond`.

## Notes / open

- **Statement form** (`const if` driving leaves, no value) is not implemented;
  only the value form. Concat's zero-width guard may want it.
- **Inline Mirin-bodied fns** now splice (S7, `planning/inline_bodies.md`); a
  `const if` *through* an inline fn folds in the **grounded** case via the inline
  splice (the symbolic case awaits the generate-if, step 5). **Direction
  (2026-06-26):** the slice/concat zero-width guard is therefore the *prelude*
  `const if` wrapping a raw layout primitive — **not** synthesised in the backend
  (the earlier conclusion, now reversed). The read guard is an `#[inline]` prelude
  fn; the set guard is compiler-applied at the `BitRange` drive (a set is an
  lvalue, not a value). This makes a `const if` through an inline body the
  *forcing function* for step 5, and removes all zero-width logic from the
  backend. Full plan: `planning/slice_guards.md`.
- **Divergent-type arms** and the `generate if` (step 5) path are the two known
  extensions.
