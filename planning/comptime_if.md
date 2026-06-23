# Compile-time `const if`

Status: steps 1–4 landed (parse, HIR, type-check + const-condition rejection,
grounded fold). The symbolic `generate if` lowering (step 5) is not yet built.

`const if cond { … } else { … }` is a **compile-time conditional**: `cond` is a
constant expression resolved at elaboration, and only the *selected* arm is
elaborated. The discarded arm may be invalid for a given instantiation — an
out-of-range slice when a width folds to 0 — which a normal `if` (a mux over
both arms) could not express. It is the mechanism the zero-width slice/concat
guard needs (planning/slicing.md).

## Why a distinct construct, not "lift any const-condition `if`"

The behaviour that a normal `if` cannot have is **not elaborating the dead arm**:
its width/bounds obligations are dropped, and the arms need not share internal
structure. That is a real semantic difference, not an optimisation, so it is
explicit:

- a normal `if` is a mux — both arms elaborate, both must type the same, both
  arms' obligations are enforced;
- making it implicit (treat any const-condition `if` as compile-time) would make
  *whether your dead arm is protected* depend on an inferred property; a refactor
  that made the condition runtime would silently turn the guard back into a mux
  and newly reject the dead arm — action at a distance.

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
- **symbolic** (step 5, not built): the controlling const generic rides out as
  an SV `#()` parameter, so the condition is still symbolic at emit. This needs
  an `SvItem::GenerateIf` (sibling of the existing `SvItem::GenerateFor`) — SV
  §27.5 instantiates only the selected generate block, so the dead arm is never
  elaborated. Until it lands, a symbolic condition is a hard stop in the backend
  (`eval_const_cond` panics) — acceptable because emission only runs on a
  diagnostic-free crate, and the front-end already rejects the *runtime* case,
  so the only thing that reaches it is a genuinely-symbolic const.

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
- **Inline Mirin-bodied fns** currently mis-lower (emit `0`) — a pre-existing
  limitation (inline is built for verilog-bodied prelude primitives). So a
  `const if` reached *through* such an inline fn will not fold correctly. The
  slice/concat guard must be synthesised **directly** in the backend lowering of
  the slice/concat expression (where the bounds are in hand), not delegated to
  an inline Mirin primitive.
- **Divergent-type arms** and the `generate if` (step 5) path are the two known
  extensions.
