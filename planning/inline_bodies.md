# Splicing Mirin-bodied `#[inline]` fns

Status: **v1 implemented** (combinational, value-returning). A Mirin-bodied
`#[inline] fn` splices via `splice_inline_body` (`backend/lower.rs`): a fresh
prefix-scoped nested `SvLower` over the callee's `(body, inf, mir, sig)`, value
params bound to caller-side `__inl{site}__<param>` wires, items merged into the
caller. The v1 restrictions (clocked / `var` / out-param / `const if` / integer
params) live in the `inline_check(def)` front-end query (`hir/check.rs`). Still
deferred: `const if` folding via call-site const generics on the const-eval
`Frame` (see "const generics" below + `alternative/inline_bodies-frame-constgen.md`),
and the clocked / out-param / `var` shapes. Historically only **verilog-bodied**
inline fns spliced (`render_inline` — a `${result} = EXPR` template); a
Mirin-bodied `#[inline] fn` used to hit a front-end rejection (`InlineNonVerilogBody`,
retired) / a backend panic.

## Why we want it

`#[inline]` exists so a small helper emits as a wire at the call site instead of
a module instance. The next workstream (planning/slicing.md, planning/comptime_if.md)
wants to write the zero-width slice/concat guards as Mirin `#[inline]` fns
carrying a `const if` — e.g.

```
#[inline]
fn slice {const lo, const hi} (x: bits(W)) -> bits(hi - lo) {
    const if hi - lo == 0 { … } else { x[hi-1 .. lo] }
}
```

Without splicing these would each be a module instance (ugly SV, the thing
`#[inline]` avoids), and — critically — the `const if` inside them must fold
using the *concrete* `lo`/`hi` from the call site (see "Const generics", below).
Splicing is what lets a primitive be written once in Mirin and emit clean inline
SV at every call.

## Shape — recursive sub-lowering (not an HIR pass)

`SvLower` is built around one def's `(body, inf, sig)` and `LocalId`-indexed
name tables. Splicing a *callee* body means lowering a *different* `(body, inf,
sig)` while emitting into the caller's module. Two ways:

- **HIR inline pass** (rejected): rewrite the caller's HIR to inline the callee
  before lowering. Wrong shape for Mirin: the backend looks up types by `ExprId`
  *per def* (`self.inf`), so freshly-minted inlined nodes would have no inference
  data — you'd have to re-infer or remap. rustc can inline at MIR because types
  ride *on* the MIR; Mirin's per-def `ExprId`-keyed inference does not.
- **Sub-lowering** (chosen): lower the callee's own `(body, inf, sig)` — each is
  a per-def salsa query, already available and correct — with its params bound to
  the caller's argument values and its emitted names prefixed, merging its items
  into the caller. No new HIR, no infer remap: we just switch which `(body, inf,
  sig)` the lowering reads, exactly as `emit_instance` already does for a module
  instance (it just splices instead of instantiating).

So `render_inline`'s non-verilog branch calls a new `splice_inline_body(uc) ->
SvExpr` / leaves, the inline twin of `emit_instance`.

(rustc parallel, confirmed in review: the MIR inliner clones the callee body and
instantiates it with the call's substs, remapping locals/blocks via an
`Integrator` visitor and binding args to param-local temporaries — *because types
ride on MIR*. Sub-lowering reproduces that integrate-with-subst at lowering time:
the name-prefix + item-merge is the `Integrator`, the param-as-wire binding is
the arg temporaries, and the depth limit matches rustc's history-depth guard.)

## Mechanics

`splice_inline_body(uc)`:

1. **Fetch** callee `body`/`inf`/`sig` (salsa per-def).
2. **Bind only VALUE params as wires.** For each callee *value* param (receiver
   included), declare a caller-side wire `__inl{site}__<param>` per leaf and
   `assign` it the caller's argument leaf — reusing the flatten/connection
   machinery `emit_instance` has. Param reads inside the callee resolve to those
   wires (a param used twice is one wire, not a duplicated caller expression). A
   zero-width leaf binds as the uniform `[-1:0]` wire — no special case, so this
   composes with the zero-width representation (planning/slicing.md). **Integer /
   const-generic params are NOT wires:** `build_module` already elides
   integer-typed params (no port — `lower.rs`), and a slice helper's `lo`/`hi`
   are const generics that must ride through const eval (next item), never a
   wire.
3. **Sub-lower the body** in a *fresh nested `SvLower`* over the callee's
   `(body, inf, sig, self_subst)` whose emitted `items`/`mono_reqs` are drained
   into the caller and whose tail `SvExpr` (scalar) / leaves (aggregate) is the
   spliced value. `SvLower` holds `&'a` refs fixed at construction, so this is a
   second context, not a field-swap. Every name it mints (`local_names`, the
   `__block_N`/`__call_N` synth counter) is prefixed `__inl{site}__` so `var`s,
   nested blocks, and nested instances merge without colliding.
4. **Type generics** thread through the callee `self_subst` (as monomorphised
   copies already substitute Type-kind generics before flattening).

### Const generics — the load-bearing detail

A `const if` (or a width) inside the callee references the callee's **const
generics** (`ConstParam`). Spliced with concrete args (`slice{lo=4, hi=8}`),
those must ground, or the `const if` cannot fold and the width is wrong.

- **Widths** already ground via the call's substitution into `ConstArg`s — but
  note `emit_instance`/`render_inline` **double-substitute**: `call_subst` *then*
  the caller's `self_subst` (`lower.rs` two `subst_const_opt` calls), so a const
  arg projecting onto an *outer* type param (`A::bit_size`, `Assoc{self_ty: A}` —
  the pack/slice guard shape) grounds once the enclosing module is
  monomorphised. The inline site must compose the same two; factor them into one
  shared helper.
- **`const if` conditions** are *expressions*, not `ConstArg`s, and
  `const_eval` marks `ConstParam` symbolic at the root frame. Bind const generics
  **on the `Frame`** (mirroring `Frame::bindings` for value params), populated in
  both `Frame::root` (from the composed `call_subst ∘ self_subst`) and
  `enter_call` (so a generic reached through a *recursive* const-helper call also
  grounds — a threaded entry-point `subst` only reaches the outermost frame and
  leaks symbolic one call deep). `eval_expr`'s `ConstParam(i)` consults the
  Frame binding; a bound-but-still-symbolic arg correctly re-defers. This is the
  recorded alternative `planning/alternative/inline_bodies-frame-constgen.md`,
  adopted over the threaded-`subst` sketch — it also closes a pre-existing
  recursive-symbolic leak independent of inlining.

## Relationship to the slice/concat guards — the FORCING FUNCTION (2026-06-26)

**Reversed direction (Jon, 2026-06-26):** the zero-width slice/concat guard is
**not** backend-synthesised. The layout operations are *primitives that do not
support zero-width*, and the guard is a Mirin `const if` wrapping them — the
**read** as an `#[inline]` fn in `prelude.mrn`, the **set** applied by the
compiler at the `BitRange` drive (a set is an lvalue, not a value). So a `const
if` *through* an inline body is the slicing critical path, and the slice-read
guard is the acceptance test that inline + `const if` compose. Full plan:
`planning/slice_guards.md`. (This supersedes the earlier "guards are
backend-synthesised, inline is off the critical path" reconciliation.)

- **Read guard: a prelude `#[inline]` `const if`.** `x[a..b]` desugars to a
  prelude `slice` fn whose body is `const if w == 0 { zero } else { __slice_raw }`.
  Splicing it folds the guard at the call site (grounded) — so inline + `const if`
  is on the slicing critical path, not independent of it.
- **Set guard: compiler-special.** A set drives a place, so it stays a compiler
  construct (`Projection::BitRange`) with the compiler applying the same `const
  if` guard at the drive.

The mono interaction is unchanged and is the reason the *symbolic* case still
needs step-5 `generate if`: a spliced `const if` grounds (folds) only when
`call_subst ∘ self_subst` makes the controlling const generic a *literal at this
call site* (`choose{k=0}`, or the caller's own generic bound by its caller's
mono). When the caller is itself generic and the width rides out as a `#()`
param, the condition stays symbolic and hits the **step-5 `generate if` wall** —
the *same* wall whether the guard is in the prelude or the backend. Moving it to
the prelude removes all backend zero-width code and makes the grounded case
(most real code) the immediate, inline-splice-driven deliverable.

## v1 scope and deferrals

- **v1: combinational, value-returning bodies** — `id`, reinterprets, simple
  helpers. Scalar and aggregate (leaf) results.
- **Front-end validation (where the restriction lives).** `#[inline]` is today
  only a flag (`def_map`), with no body-shape check. Add a front-end `check`
  query over `body(def)` that, when `data.inline` and the body is non-verilog,
  rejects a `when`/`.reg` (clocked), an out-param connection, or (v1) a `var`
  with a spanned diagnostic — *not* a backend panic (emission runs only on a
  diagnostic-free crate). This is the home for every "not yet" below.
- **Clocked inline bodies**: the callee's domain/clock generics would bind to the
  caller's clocks (as `emit_instance` threads the clock). Deferred; rejected by
  the front-end check above until then.
- **out-params** (`=>` from an inline body): deferred; rejected by the check.
- **`var` / cyclic equations**: should fall out of item merging, but deferred
  past what the v1 helpers need; rejected by the check until validated.
- **Nested/recursive inline**: `splice_inline_body` recurses through
  `inline_call`; guard with a depth limit to turn accidental inline cycles into
  a clean error rather than a stack overflow.

## Touch points

- `backend/lower.rs`: `splice_inline_body` (twin of `emit_instance`); route the
  non-verilog branch of `render_inline` to it; a per-site name prefix threaded
  into local/synth naming; remove the panic.
- `hir/const_eval.rs`: const-generic bindings on `Frame` (populated in
  `Frame::root` + `enter_call`); `ConstParam` consults them. A shared helper for
  the `call_subst ∘ self_subst` composition, used by the inline site and
  `emit_instance` alike.
- a front-end `check` query for inline-body shape validation (clocked / out-param
  / `var` rejection) with its own `*DiagnosticKind`.
- No new IR / no new pass — splicing lives inside the existing `verilog` emission
  query and the validation is a `check`-style query, so `planning/ir_pipeline.md`
  needs no stage change (a one-line note at most).

## Tests

- `#[inline] fn id(a) { a }` → spliced wire, not a module, not `0`.
- inline const-generic `const if` (the `inl_cif.mrn` corner that currently
  mis-folds): `choose{k=0}` ⇒ `a`, `choose{k=1}` ⇒ `b`, each a plain wire.
- inline fn whose body calls another fn (nested instance merges under the
  prefix).
- aggregate-returning inline fn (per-leaf splice).
- a clocked inline body is rejected with a diagnostic (until clocked splicing
  lands).
