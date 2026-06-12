# Parametricity: Const-kind inference and residual constraints

This doc covers the next slice of generic-function work, picking up from
where `planning/type_inference.md` left a placeholder: Const-kind generic
parameters (`{ param N: usize }`), the inference machinery they need, and
the obligation system that handles width arithmetic the unifier can't
discharge locally.

The first-pass goal is to make programs like

```polar
fn bitwise_xor { param N: usize } (a: uint(N), b: uint(N)) -> uint(N) { ... }
fn add        { param N: usize } (a: uint(N), b: uint(N)) -> uint(N) { ... }
fn concat     { param M: usize, param N: usize }
              (a: uint(M), b: uint(N)) -> uint(M + N) { ... }
```

type-check and elaborate end-to-end. `+` and `*` then fold into the same
machinery as user-defined fns — `infer_arith_call`'s hand-rolled path goes
away.

## Prior art

- **Haskell + kind polymorphism.** `data Vec (n :: Nat) a` treats `Nat`-kind
  and `Type`-kind parameters uniformly. Substitution dispatches per kind on
  each parameter reference but lives in one walk. GHC's `TyVar` /
  `CoVar` / `Id` split tracks the kind on the variable; for us
  `GenericArg::{Type, Const, Domain}` does the same.
- **rustc `GenericArgs`.** A single positional `&[GenericArg]` array,
  parameter index → kinded arg. Substitution is `EarlyBinder::instantiate`.
  We already mirror this shape for struct/port use sites; this work extends
  it to function call sites and to width positions inside types.
- **OutsideIn(X) and GHC's constraint solver.** Type-checking emits a set
  of `Wanted` constraints; the solver simplifies, the residual moves onto
  the function's signature. Polar's `Obligation` queue is the same
  mechanism; we extend it to `ConstEq` constraints over widths.

## State at the start of this work

In place:
- `GenericArg { Type, Const, Domain }` and `GenericArgs(Vec<GenericArg>)`
  already exist in HIR (`hir/mod.rs`).
- Struct/port use sites carry `GenericArgs`; flatten substitutes them into
  field types via `instantiate_type`, including Const-kind args
  (`parametric_width_port.plr` proves this end-to-end).
- Function call sites: Type-kind and Domain-kind inference work via
  `SigSubst`; Const-kind is a no-op stub (`typeck.rs` `build_sig_subst`).
- Widths are `HirExpr`s. Param references inside widths are
  `HirExprKind::Local(LocalId)`. Arithmetic operators (`+`, `*`) take a
  hand-rolled path that unifies operand types directly (`infer_arith_call`).
- `Obligation::WidthEq` is declared in the obligation enum but its
  discharge pass is a no-op.

The four phases below close this gap.

## Phase A — unified GenericArg substitution

No semantic change; this is a refactor so Phases B and D have one
substitution shape to extend.

- Replace `SigSubst { domain_subst, type_subst }` with `SigSubst { args:
  GenericArgs }`, indexed by the callee's `generic_params` position.
- Replace `Substitution { type_subst: HashMap<u32, HirType>, domain_subst:
  HashMap<LocalId, Domain> }` likewise. Use-site and call-site become the
  same struct.
- Add `HirExprKind::Param(u32)` mirroring `ValueKind::Param(u32)`. Width
  expressions that reference a Const-kind generic param lower to
  `Param(i)` instead of `Local(LocalId)`. Resolves the existing
  inconsistency where Type-kind references are positional but Const-kind
  references are LocalId-keyed.
- `apply_to_type` walks both type positions and width positions, looking up
  `args[i]` and splicing the kind-matched payload.
- Domain-kind args populate from inference of named-section `dom` params
  (the existing context-driven path); other kinds populate from positional
  generic args / inference variables. This asymmetry stays — it matches the
  named-vs-positional surface distinction.

Tests: the existing `parameterized_port.plr` and `parametric_width_port.plr`
keep passing. All 115 lib + 15 CLI tests stay green.

## Phase B — Const-kind inference variables

Goal: `add { N }(uint(N), uint(N)) -> uint(N)` and `bitwise_xor { N }(...)`
type-check. Move `+` and `*` onto the general parametric path.

- Add `HirExprKind::ConstVar(ConstVarId)` and a const-var pool on
  `InferCtxt` parallel to `type_vars` / `domain_vars`. Same union-find
  resolution.
- `build_sig_subst` for Const-kind generic params allocates a fresh
  ConstVar and packages it as `GenericArg::Const(HirExpr::const_var(id))`.
- `unify_widths` learns the three HM rules:
  - `(ConstVar(α), ConstVar(β))` if α == β: ok.
  - `(ConstVar(α), other)` or `(other, ConstVar(α))`: bind α → other,
    occurs-check applies.
  - Both ground: existing constant-equality check.
  - Else: defer (handled in Phase D).
- `apply_to_type` recurses into `ValueKind::UInt { width }` and, more
  generally, walks `HirExpr`s for `Param(i)` references, substituting
  `args[i]`'s `Const` payload.
- `resolve_type` chases const vars when reading widths back out.
- Migrate `infer_arith_call` and `infer_reg_call`'s width-handling onto
  the general path. Delete the hand-rolled branches.

Sum-of-monomials normalisation (see below) is folded in here, even though
it shines hardest in Phase D — having it from the start means widths
written `N + 1` and `1 + N` already canonicalise to the same expression.

Tests: new examples for `add`, `bitwise_xor`. Migration of `+`/`*`
verified by every existing example still emitting equivalent SV.

## Phase D — residual obligations on signatures

(Skipping Phase C, the strict-check variant. Going straight to
deferred-then-propagate.)

Goal: `concat { M, N }(uint(M), uint(N)) -> uint(M + N)` type-checks.
Width equations the unifier can't immediately decide are kept as
obligations on the function's signature and propagated through call
sites.

- `Obligation` gains `ConstEq { lhs: HirExpr, rhs: HirExpr, span }` (the
  enum entry already exists as `WidthEq`; rename for consistency and
  generalise — these aren't only width-typed).
- `unify_widths` defers instead of erroring when neither side is a single
  var and the two normal forms aren't syntactically equal.
- After fn typeck completes, `flush_obligations` runs to fixpoint:
  attempts each obligation with current bindings; bindings that simplify
  can unlock other obligations.
- Whatever survives is normalised once more and attached to the fn's HIR
  signature as `residual_constraints: Vec<ConstraintShape>`. Same shape
  for prelude fns, user fns, and synthesised method shells.
- At a call site, the callee's residuals are substituted with the call's
  `GenericArgs`, then pushed as fresh obligations in the caller's
  context. Empty after substitution → silently discharged. Non-empty and
  ground → discharged immediately (error if false). Non-empty and still
  symbolic → propagates to the caller's residuals.
- Monomorphisation (flatten + SV lowering) sees only fully-grounded args
  for emitted modules; residuals must discharge there. (Library fns that
  are never reached never get monomorphised, and their residuals are
  never checked. That's fine — they emit no Verilog.)

Recursive Const-generic functions are rejected in this phase: a fn
calling itself with non-trivial Const args (i.e. anything but
identity-substituted) is a hard error. Lifting the restriction needs an
explicit residual annotation on the signature so the residual set can be
bounded; we'll revisit when there's a concrete motivating example.

Tests: `concat`, `n + n ~ 2 * n` style equalities discharging through
normalisation, `pad{M, N}(uint(M)) -> uint(M + N)` propagating through
two layers of generic call.

## Phase D′ — Verilog assertions from residuals

Small follow-up. After monomorphisation, any residual that survives to
SV emission is a predicate over the synthesised module's `parameter`
declarations. Emit it as

```systemverilog
initial begin
  assert (M + N == 24);
end
```

so SV elaboration catches the violation. Statically-decidable residuals
discharge at compile time and emit nothing.

This is the only respect in which Polar's situation is *better* than
GHC's: GHC's deferred type errors become runtime exceptions; ours become
elaboration-time assertion failures, which is the right granularity for
HDL.

## Sum-of-monomials normal form

All Const-typed `HirExpr`s flowing through unification, residual storage,
and propagation are normalised to a canonical linear form:

```text
NormalConst = (constant: i64, terms: Vec<(coefficient: i64, var: ConstVarId | Param(u32))>)
```

The `terms` vec is sorted by `(var_kind, id)` and zero-coefficient terms
are dropped. Two normalised constants are equal iff their fields are
identically equal — `Vec` comparison suffices, no further smart check.

What this gets us:

- `M + N` and `N + M` collapse to one form.
- `N + N` and `2 * N` collapse to one form.
- `N + 1 + 1` and `N + 2` collapse to one form.
- `unify_widths` after normalisation only needs to check "are both
  ground and equal?" or "is one side a single variable?" — the
  arithmetic cases (`M + N ~ M + K`) reduce by cancellation.

What it doesn't get us:

- Multiplication by non-constants (`M * N`) stays as an opaque term.
  Falls out of the linear-form assumption; we accept the limitation.
- Division, modulo, comparisons. Out of scope for the first cut.

Implementation lives in a small `normal_const.rs` module: a builder that
walks `HirExpr` and folds `+`/`-`/literal-`*` into the canonical form,
plus a printer for diagnostics.

## What this leaves for later

- Full Presburger / non-linear constraint solving. Sum-of-monomials
  covers the common cases; richer arithmetic stays opaque and either
  discharges by syntactic equality or propagates.
- `where` clauses on signatures (`fn pad { N } (...) where N > 0`).
  The obligation enum can grow `ConstPredicate { expr, span }`
  trivially; the solver work is the same as ConstEq.
- Recursive Const-generic fns. Need a residual-bound check or an
  explicit annotation to terminate residual propagation.
- Type-level reasoning over `bits` vs `uint` vs `sint` — out of scope
  here; this doc is only concerned with the width parameter, not the
  element-type tagging.

## Tracking and updates

When Phase A lands, update `planning/ir_pipeline.md`:
- typeck row references the unified `SigSubst { args: GenericArgs }`.
- flatten row references the unified `Substitution` (drop the separate
  type/const/domain tables from the description).

When Phase B lands, update `planning/type_inference.md`:
- The infer-var paragraph mentions a third pool (`const_vars`).
- The walk paragraph notes that `Call` of arithmetic operators no longer
  takes a bespoke path.

When Phase D lands, update both:
- `ir_pipeline.md` adds a `residual_constraints` field to the HIR fn
  signature description.
- `type_inference.md` rewrites the "Obligation queue" section: from
  "discharge what we can, drop the rest" to "discharge to fixpoint,
  attach residual to signature, propagate at call sites."
