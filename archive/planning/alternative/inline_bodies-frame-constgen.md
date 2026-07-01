# Alternative: bind const generics on the const_eval `Frame`, not a threaded `subst`

Alternative to the "subst-aware `eval_cond`" mechanism in
`planning/inline_bodies.md` (the const-generic folding half of the splice). The
sub-lowering / param-as-wire / item-merge half of that doc is kept as-is; this
only swaps out *how a spliced `const if` grounds its const generics*.

## The doc's mechanism

The doc proposes a new entry-point signature
`eval_cond(db, krate, def, cond, subst)` and makes `eval_expr`'s
`ExprKind::ConstParam(i)` consult `subst[i]`. `splice_inline_body` passes the
call's `call_subst` as `subst`.

## The problem it under-serves

`const_eval` is already a *recursive interpreter over callee bodies*: `eval_expr`
on a `Call` builds a callee `Frame` via `enter_call` and evaluates the callee's
return (`hir/const_eval.rs:601`, `:455`). The activation record is the `Frame`,
which already carries the call-site bindings for the callee's **value** params
(`Frame::bindings`, keyed by `LocalId`).

But `enter_call` binds *only value params* and the `Frame` has *no* slot for
const generics, so a `ConstParam(i)` reached **through a recursive call** stays
symbolic regardless of the top-level `subst`. The doc's threaded `subst` argument
only reaches the *outermost* `eval_cond` frame. Two real cases slip through:

1. A guard whose condition calls a const helper that takes the generic as a
   *value* param — `const if is_zero(hi - lo) { … }` where
   `fn is_zero(n: integer) -> bool` is itself a fn. `enter_call` binds `n` to the
   evaluated `hi - lo`, fine — but if instead the helper reads the generic as a
   const generic of *its own* (`is_zero{n}()`), the inner `ConstParam` has no
   binding and the whole condition goes symbolic, spuriously forcing the
   step-5 `generate if` (or panicking if step 5 isn't built).
2. The doc's `subst[i]` consult omits composition with the caller's
   `self_subst`. `render_inline`/`emit_instance` deliberately double-substitute
   (`subst_const_opt(c, node_subst)` **then** `subst_const_opt(c, self_subst)` —
   `lower.rs:2636-2637`, `:2775-2776`) so that a const arg that projects onto an
   *outer* type param (`A::bit_size`, `Assoc{self_ty: A}`) grounds once the
   enclosing module is monomorphised. A spliced `const if` over such an arg would
   not ground under the doc's single-level `subst[i]`.

## The alternative: put the const-generic binding on the `Frame`

Mirror exactly what `Frame::bindings` already does for value params:

- Add `const_bindings: Vec<Option<ConstArg>>` (or reuse a `Term`-keyed map) to
  `Frame`, indexed by the def's generic-param index.
- `Frame::root` takes an optional binding; the standalone `eval_cond`/`eval_width`
  callers pass the call's `call_subst` *already composed with* the caller's
  `self_subst` (the same two `subst_const_opt` calls the backend uses today —
  factor them into one helper so the inline site and `emit_instance` share it).
- `eval_expr`'s `ConstParam(i)` consults `frame.const_bindings[i]`: a bound
  `ConstArg` is re-entered through `eval_const_arg` (so a bound `Param`/`Assoc`
  that is *still* symbolic in the caller correctly re-marks `symbolic` and
  defers); an unbound slot stays symbolic exactly as today.
- `enter_call` computes the callee's const binding from the *call's* recorded
  subst, evaluated in the **caller** frame, and stores it on the new callee
  `Frame` — closing case 1 for free.

The backend's `splice_inline_body` then needs **no new `eval_cond` entry point**:
it grounds a spliced `const if` by calling the same `eval_cond` with a `Frame`
seeded from `call_subst ∘ self_subst`, identically to how a width grounds.

## How it lowers through the pipeline

Unchanged from the doc except inside `hir/const_eval.rs`: the `Frame` gains a
field, `enter_call`/`root` populate it, `ConstParam` reads it. No IR change, no
pass change. The backend touch point shrinks (no bespoke `eval_cond` signature).

## What it makes easy

- One uniform rule: "a const generic resolves against the active frame's
  binding," at every depth — the value-param model extended to const params.
- Closes the latent `enter_call` hole that exists *today*, independent of inline
  splicing (any generic const fn called from another const fn).
- Reuses the existing double-subst discipline rather than re-deriving a weaker
  single-level one at the inline site.

## What it makes hard

- Slightly more surgery in `const_eval` (a new `Frame` field threaded through
  `root`/`enter_call`) versus the doc's single `match` arm. But the doc's version
  is the one that needs *follow-up* surgery once case 1 bites.

## Head-to-head

The doc's threaded `subst` is the minimal change that makes the *motivating*
example (`choose{k=0}` ⇒ `a`) fold, and for a guard whose condition is a direct
arithmetic comparison of the generics (`hi - lo == 0`) it is sufficient. The
Frame-based version is strictly more general, fixes a pre-existing symbolic-leak
in recursive const eval, and removes a backend-side re-implementation of the
caller-subst composition. I would pick the Frame-based version: it is the same
amount of conceptual surface (const params behave like value params), it is the
shape `const_eval` already wants, and it avoids shipping a folding mechanism that
is correct only one call deep and only when no outer type-param projection is
involved. If the team wants the absolute smallest v1, the doc's version is
acceptable **provided** the doc (a) states the single-call-depth limitation
explicitly and (b) composes `self_subst` into the `subst` it passes — without (b)
it mis-folds the `A::bit_size`-style guard, which is exactly the slice/pack
family the workstream targets.
