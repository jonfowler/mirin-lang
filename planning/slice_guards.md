# Zero-width layout guards via prelude `const if` (workplan)

> **Direction (Jon, 2026-06-26):** the zero-width slice/concat guards are **not**
> backend-synthesised. The layout operations are *primitives that do not support
> zero-width*; the guard is a Mirin `const if` written in `prelude.mrn`. The
> **read** can be a pure primitive; the **set** needs to be a special compiler
> construct (it is an lvalue drive, not a value). This reverses the
> "synthesise the guard directly in the backend" conclusion in
> `planning/comptime_if.md` and `planning/slicing.md`.

This is also the **forcing function** for `const if` inside an `#[inline]` Mirin
body (S7's deferred increment): the slice-read guard *is* an `#[inline]` fn with a
`const if` in `prelude.mrn`, so making it work both delivers the guard and is the
acceptance test that inline + `const if` compose.

## The shape

A layout primitive emits SV that is only legal for a positive width. Wrap it in a
Mirin `const if` on the (output) width:

```mirin
// prelude.mrn — user-facing slice; what `x[a..b]` (read) desugars to.
#[inline]
fn slice {const lo: integer, const w: integer} (x: bits(W)) -> bits(w) {
    const if w == 0 { zero_bits() }        // effective-0-bit value, never read
    else            { __slice_raw(x, lo) } // the raw part-select primitive (w >= 1)
}
```

- **`__slice_raw`** — a verilog-bodied primitive: `assign ${return} = ${x}[${lo} +: ${w}];`
  (or `${x}[${lo + w - 1} : ${lo}]` when `lo` is a constant, for the nicer
  `[msb:lo]` form). It assumes `w >= 1`; zero-ness is *not* its problem.
- **The guard is the `const if`** — and because `slice` is `#[inline]`, the guard
  folds at the call site:
  - **grounded `w`** (a literal at the call): the inline splice folds the
    `const if` to the taken branch — the immediate work (Phase 0/1).
  - **symbolic `w`** (a generic caller, `w` rides as `#()`): the `const if`
    lowers to an SV `generate if` (comptime_if step 5) — the long pole, but now
    the *same* prelude `const if`, with **zero backend guard code**.

`set` is the dual but cannot be a value fn (it drives a place), so the compiler
keeps the `BitRange` place and applies the *same* `const if` guard itself at the
drive (Phase 2): grounded `w == 0` ⇒ emit no drive; symbolic ⇒ generate-if.

## Why this is better than backend synthesis

- **One mechanism.** Zero-width is handled by `const if` everywhere — slice read,
  slice set, `concat_hi`, `resize` — instead of a bespoke guard baked into each
  backend lowering. The backend stops knowing about zero-width entirely (except
  the set drive, which is compiler-special anyway).
- **User-expressible.** A user writing their own layout helper gets the same
  guard with the same `const if` they already have, not a privileged builtin.
- **Tests the inline + const-if path.** The prelude slice guard exercises
  `const if` through `#[inline]` on every slice — the broadest possible coverage.

## Workplan

Each phase is independently committable, golden-gated, with examples. Phase 0 is
the prerequisite; 1–3 build on it; 4 (symbolic) lands last and covers the
generic-width tail.

### Phase 0 — `const if` folding inside an inline splice (grounded)

The forcing-function core. Make a `const if` in a spliced `#[inline]` body fold
against the call-site const generics.

- **mir::const_eval**: accept the def's const bindings (the splice's composed
  `self_subst`), consulted at `MExprKind::ConstParam(i)` — the MIR analogue of the
  `Frame` const-binding in `planning/alternative/inline_bodies-frame-constgen.md`.
- **backend splice**: handle `MExprKind::ConstIf` in `expr_value`/`expr_leaves`
  (currently `todo!`): evaluate the cond with the composed const subst, keep the
  taken branch (`block_value`/`block_leaves`). A symbolic cond stays a hard stop
  until Phase 4.
- **inline_check**: drop the blanket `ConstIf` rejection; reject only the case
  that would reach the unbuilt symbolic path (until Phase 4) — i.e. allow a
  `const if` whose condition grounds at the (eventual) call, defer the rest. (If
  precise per-call detection is awkward, keep rejecting a `const if` over a
  *symbolic* generic and allow it once Phase 4 lands.)
- **test**: `#[inline] fn choose {const k}() -> uint(8) { const if k == 0 { a } else { b } }`,
  called `choose{k=0}` / `choose{k=1}`, each a plain wire.

### Phase 1 — slice **read** as primitive + prelude guard

- **`__slice_raw`** primitive in `prelude.mrn` (verilog-bodied): the part-select,
  `w >= 1`. Decide const-`lo` `[msb:lo]` vs uniform `[lo +: w]` (open q below).
- **`zero_bits()`** (or a typed-literal form) producing a `bits(0)` effective-0-bit
  value for the guard's taken branch (open q below).
- **prelude `slice`** wrapper (`#[inline]` + `const if`), and its `Vec` dual
  (low-first, `Vec(w, A)`).
- **desugar** `x[a..b]` / `x[off..+w]` / elision (read position) → a call to the
  prelude `slice` (compute `lo`/`w` as const args; base may be runtime). This
  replaces the backend's read-side `MExprKind::Slice` lowering (`slice_range_sv`
  for reads) with a desugaring; the typing/bounds/direction checks in `infer`
  stay (they type the slice and derive `lo`/`w`).
- **test**: promote a zero-width read example (`x[4..4]`, and a parametric
  `x[n..n]` at its limit grounded) to working + verilator-clean; the current
  S4 slice examples keep passing through the new path (golden review).

### Phase 2 — slice **set** zero-width guard (compiler-special)

- Keep `Projection::BitRange` (lvalue). Apply the `const if` guard *at the drive*
  in the compiler: grounded `w == 0` ⇒ emit nothing; symbolic ⇒ generate-if
  (Phase 4). No prelude fn (a set is not a value).
- **test**: `x[4..4] = y` (zero-width set) emits no drive and stays
  verilator-clean; existing slice-set examples unchanged.

### Phase 3 — `concat_hi` / `resize` guards via prelude `const if`

- Wrap the zero-width-operand case of `concat_hi` (and the zero-width `self` case
  of `resize`) in a prelude `const if`, removing reliance on any backend zero
  handling. `resize`'s zero *pad* already works (`{0{x}}` is ignored by SV); only
  a zero-width input/operand needs the guard.
- **test**: a `concat_hi` with a zero-width operand; `resize` to/from a
  zero-width — both grounded — verilator-clean.

### Phase 4 — symbolic widths: `generate if` (comptime_if step 5)

- `SvItem::GenerateIf` (sibling of `SvItem::GenerateFor`); a `const if` whose
  condition is still symbolic at emit lowers to it (SV §27.5: only the selected
  generate block is elaborated, so the dead arm's out-of-range select never
  exists). This makes the prelude guards work for *generic* widths uniformly,
  with no backend special-casing. Until it lands, a symbolic-width slice through
  the prelude guard is a clean front-end rejection (Phase 0's `inline_check`
  narrowing), exactly the wall that exists today — just relocated.

## Open questions (for design before/while building)

1. **Slice desugaring surface.** Does `x[a..b]` desugar to a method
   (`x.slice::<lo, w>()`, a `Slice`/`Index`-style trait) or a free prelude fn
   resolved by the compiler (like operator → `add`)? It must carry `lo`/`w` as
   **const** args and the base as a value. A trait keeps `bits`/`Vec` duals clean.
2. **Zero-width literal.** How does the guard's taken branch name a `bits(0)` /
   `Vec(0, A)` value — a `zero_bits()` builtin, a typed literal `bits(0)::0`, or
   `[-1:0]` synthesised? It must flatten to the uniform effective-0-bit leaf
   (`planning/slicing.md` "Representation"), not to *no* leaves.
3. **Const-`lo` nice form.** Keep emitting `[msb:lo]` for a constant low endpoint
   (cleaner Verilog) by const-splicing `${lo + w - 1}:${lo}` in `__slice_raw`, or
   accept uniform `[lo +: w]` everywhere? (Affects golden diffs on existing slice
   examples.)
4. **Phase 0 `inline_check` narrowing.** Can the front end tell "this `const if`
   will ground at every call" from "it may stay symbolic"? If not, the safe v1 is
   to allow `const if` in inline bodies but keep emission's symbolic-cond stop as
   the backstop until Phase 4 — and ensure that stop is a diagnostic, not a panic.
5. **Bounds/direction checks** stay in `infer` (they type the slice and derive
   `lo`/`w`) regardless of the lowering move — confirm nothing in the read-path
   bounds check assumed the backend `Slice` node.

## Doc cross-refs updated for this direction

- `planning/slicing.md` — "Zero-width values" now points here (read primitive +
  prelude `const if`; set compiler-special), not backend synthesis.
- `planning/comptime_if.md` — the "synthesise directly in the backend" note is
  reversed: guards live in prelude `const if`; inline-Mirin `const if` is the
  mechanism (Phase 0).
- `planning/inline_bodies.md` — `const if` in inline is the forcing function, not
  an off-critical-path nicety.
- `planning/mir_progress.md` — S7's `const if` increment reopened as active work
  (Phase 0), tracked against this plan.
