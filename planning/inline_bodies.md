# Splicing Mirin-bodied `#[inline]` fns

Status: designed, not implemented. Today only **verilog-bodied** inline fns
splice (`render_inline` — a `${result} = EXPR` template); a Mirin-bodied
`#[inline] fn` hits a panic (was a silent `SvExpr::Lit("0")`, fixed in
`c2316c1`). This doc designs the real splice.

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

## Mechanics

`splice_inline_body(uc)`:

1. **Fetch** callee `body`/`inf`/`sig` (salsa per-def).
2. **Bind params like an instance binds ports.** For each callee value param
   (receiver included, for methods), declare a caller-side wire
   `__inl{site}__<param>` per leaf and `assign` it the caller's argument leaf —
   reusing the flatten/connection machinery `emit_instance` already has. Param
   reads inside the callee then resolve to those wires (so a param used twice is
   one wire, not a duplicated caller expression).
3. **Sub-lower the body** with a fresh name space: every callee local/synthetic
   name is prefixed with `__inl{site}__`, so `var`s, nested `__block_N`, and
   nested instances merge into the caller's `items` without colliding. The tail
   expression's `SvExpr` (scalar) / leaves (aggregate) is the spliced value.
4. **Type generics** thread through `self_subst` (already how monomorphised
   copies substitute Type-kind generics before flattening).

### Const generics — the load-bearing detail

A `const if` (or a width) inside the callee references the callee's **const
generics** (`ConstParam`). Spliced with concrete args (`slice{lo=4, hi=8}`),
those must ground, or the `const if` cannot fold and the width is wrong.

- **Widths** already ground via the call's substitution into `ConstArg`s
  (`subst_const` + `eval_const`), the same path `emit_instance` uses — reuse it
  at the inline site.
- **`const if` conditions** are *expressions*, not `ConstArg`s, and
  `const_eval::eval_cond` today uses `Frame::root`, which marks `ConstParam`
  symbolic. So extend it: `eval_cond(db, krate, def, cond, subst)` threads the
  call's const-generic binding, and `eval_expr`'s `ConstParam(i)` consults
  `subst[i]` (a const value → fold; else symbolic). `splice_inline_body` passes
  the call's subst. This is what makes an inline `const if` helper fold to one
  arm at each call site — the whole point of writing the guard in Mirin.

## v1 scope and deferrals

- **v1: combinational, value-returning bodies** — the slice/concat-guard shape.
  Scalar and aggregate (leaf) results.
- **Clocked inline bodies** (a `when`/`.reg` inside): the callee's domain/clock
  generics must bind to the caller's clocks (as `emit_instance` threads the clock
  connection). Doable via the same param/generic binding; deferred to keep v1
  small. Until then, reject a clocked inline Mirin body with a diagnostic rather
  than mis-thread a clock.
- **out-params** (`=>` connections from an inline body): deferred; inline use is
  value-returning.
- **`var` / cyclic equations** in an inline body: should fall out of item
  merging (the leaves become prefixed caller wires with the same equations), but
  not a v1 target beyond what the guard helpers need.
- **Nested/recursive inline**: `splice_inline_body` recurses through
  `inline_call`; guard with a depth limit to turn accidental inline cycles into
  a clean error rather than a stack overflow.

## Touch points

- `backend/lower.rs`: `splice_inline_body` (twin of `emit_instance`); route the
  non-verilog branch of `render_inline` to it; a per-site name prefix threaded
  into local/synth naming; remove the panic.
- `hir/const_eval.rs`: subst-aware `eval_cond` (+ `ConstParam` consults the
  binding).
- No new IR / no new pass — this lives inside the existing `verilog` emission
  query, so `planning/ir_pipeline.md` needs no stage change (a one-line note at
  most).

## Tests

- `#[inline] fn id(a) { a }` → spliced wire, not a module, not `0`.
- inline const-generic `const if` (the `inl_cif.mrn` corner that currently
  mis-folds): `choose{k=0}` ⇒ `a`, `choose{k=1}` ⇒ `b`, each a plain wire.
- inline fn whose body calls another fn (nested instance merges under the
  prefix).
- aggregate-returning inline fn (per-leaf splice).
- a clocked inline body is rejected with a diagnostic (until clocked splicing
  lands).
