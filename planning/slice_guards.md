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

`w` (the output width) **must be a constant** — SV's one hard rule. `lo` (the low
end) **may be variable** — a runtime base is the mux-style slice, SV's indexed
part-select `x[lo +: w]`. This splits the read into **two prelude ops** by the
kind of `lo` (resolved with Jon, 2026-06-26):

```mirin
// raw part-select — verilog-bodied, assumes w >= 1; zero-ness is NOT its problem.
// `W`/`L` are inferred (named, elidable); `w` is provided (positional const).
#[inline]
fn __slice_raw {const W: integer, const L: integer}
              (self: bits(W), const w: integer, lo: uint(L)) -> bits(w) = verilog {
    assign ${return} = ${self}[${lo} +: ${w}];
}

// offset / variable-base form — what `x[lo..+w]` desugars to. `lo` is a runtime
// value; the zero-width guard lives here.
#[inline]
fn slice_from {const W, const L} (self: bits(W), const w: integer, lo: uint(L)) -> bits(w) {
    const if w == 0 { zero_bits() } else { __slice_raw(self, w, lo) }
}

// two-endpoint form — what `x[a..b]` desugars to. `lo`/`hi` are positional consts.
#[inline]
fn slice {const W} (self: bits(W), const lo: integer, const hi: integer) -> bits(hi - lo) {
    const if hi - lo == 0 { zero_bits() } else { __slice_raw(self, hi - lo, lo) }
}
```

(`Vec(N, A)` gets the duals, low-first, `-> Vec(w, A)`. Signatures are
illustrative — exact generic placement nails down in Phase 1.)

- **Named `{}` = elidable/inferable, positional `()` = must be provided** (Jon's
  convention, 2026-06-26): a self-width `W` / base-width `L` are *inferred* so they
  are named generics; the slice's `lo`/`hi`/`w` *must be given* so they are
  **positional** — even though they are `const`. A `const` positional param is a
  *const generic* (`sig.rs:1605` classifies `const` as `TermKind::Const`), so it
  lands in `generic_params`, **not** value params — which also means S7's
  `inline_check` integer-*value*-param rule (it scans value params) doesn't touch
  them. The one true value param is `slice_from`'s `lo`. (Phase-1 confirm: the
  grammar accepts `const` in the positional section, not just `{}`.)
- **`slice_from`'s `lo` is `uint(L)`, not `integer`.** `integer` is *not*
  synthesized to a net — `build_module` elides every integer param/result (only a
  const-*function* emits SV `int`), so a runtime `integer` base would be elided and
  never reach hardware. `uint(L)` with `L` inferred is the index signal's own
  width (never constrains the slice). *Open (separable):* making runtime `integer`
  synthesizable to SV `int` would let `lo` be `integer` and unify index/count
  types — a broader decision, out of this plan's critical path.
- **Two ops because the kind of `lo` differs**: `slice` takes `lo` as a positional
  const (const base, folds inside `[lo +: w]`); `slice_from` takes `lo` as a
  runtime `uint` (the mux base). Whether they share one raw primitive or each has
  its own is a Phase-1 detail.
- **`[lo +: w]` everywhere** — no `[msb:lo]` special case; a constant `lo` folds
  inside the indexed part-select, so SV quality is unaffected.
- **`..+` is required when `lo` is variable.** `x[a..b]` requires the low endpoint
  constant; a runtime base must be written `x[lo..+w]` (an infer rule). Otherwise
  `x[lo..lo+4]` (const width, runtime base) would smuggle a variable base into the
  two-endpoint form.
- **The guard is the `const if`** — and because the op is `#[inline]`, it folds at
  the call site:
  - **grounded `w`** (a literal at the call): the inline splice folds the
    `const if` to the taken branch — the immediate work (Phase 0/1).
  - **symbolic `w`** (a generic caller, `w` rides as `#()`): the *same* `const if`
    lowers to an SV `generate if` (comptime_if step 5) — the long pole, with
    **zero backend guard code**.

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

### Phase 0 — `const if` folding inside an inline splice (grounded) — DONE

The forcing-function core. Make a `const if` in a spliced `#[inline]` body fold
against the call-site const generics. **Landed** — `inline_const_if.mrn`
(`choose{k=0}`⇒`a`, `choose{k=1}`⇒`b`, each a plain wire) + golden + CLEAN +
VERILATOR_CLEAN. Key finding folded into the splice: an explicitly-provided const
generic (`{k = 0}`) is recorded as a **named arg** with its `substs` slot left
`<deferred>` (infer does not fold the value into the subst), so the splice binds
it by matching the named arg to the callee's const generic param and grounding
its value via `mir_const_arg` — Phase 1's slice ops will rely on the same path.

- **mir::const_eval**: accept the def's const bindings (the splice's composed
  `self_subst`), consulted at `MExprKind::ConstParam(i)` — the MIR analogue of the
  `Frame` const-binding in `planning/alternative/inline_bodies-frame-constgen.md`.
- **backend splice**: handle `MExprKind::ConstIf` in `expr_value`/`expr_leaves`
  (currently `todo!`): evaluate the cond with the composed const subst, keep the
  taken branch (`block_value`/`block_leaves`). A symbolic cond stays a hard stop
  until Phase 4.
- **inline_check**: **drop the `ConstIf` rejection entirely** — grounding is a
  call-site property, not a per-def one (decision 4), so the front end can't and
  shouldn't classify it.
- **splice-site diagnostic**: when the const-if can't fold at the splice (symbolic
  width, generic caller) and generate-if (Phase 4) isn't built, emit a **clean
  call-site diagnostic**, not a panic. (Today the symbolic-cond path panics in
  `eval_const_cond` — that is the compiler-state artifact to replace.)
- **test**: `#[inline] fn choose {const k}() -> uint(8) { const if k == 0 { a } else { b } }`,
  called `choose{k=0}` / `choose{k=1}`, each a plain wire.

### Phase 1 — slice **read** as primitive + prelude guard

> **Ascending flip LANDED (2026-06-26)** — the direction half (decision 6) shipped
> ahead of the prelude half: `slice_literal` (infer) and `slice_range_sv` (backend)
> are now low-first/ascending for both `bits` and `Vec`, emitting `[low +: width]`
> uniformly (decision 3). Examples flipped (`slice_bits`/`slice_elide`/`slice_set`/
> `slice_const_expr`/`slice_param`, + `slice_vec`/`slice_vec_set` re-emit `+:`) +
> goldens. **The prelude half (the `Slice` trait + `__slice_raw` + `zero_bits` +
> the `[..]`→method desugar + the zero-width guard) is DEFERRED** — it edits
> `prelude.mrn`, which currently has the user's uncommitted (slicing-adjacent) WIP;
> resume once that's clear. The backend still emits the slice directly
> (`slice_range_sv`) without the zero-width guard until then.

- **`__slice_raw {const w}(self, lo: uint)`** primitive in `prelude.mrn`
  (verilog-bodied): `${self}[${lo} +: ${w}]`, assumes `w >= 1`.
- **`zero_bits() -> bits(0)`** builtin (+ `Vec` dual) for the guard's taken branch
  — flattening to the uniform effective-0-bit leaf (decision 2).
- **`slice {const lo, const hi}`** (two-endpoint) and **`slice_from {const w}(self,
  lo: uint)`** (offset) prelude ops, each `#[inline]` + `const if` guard, with
  `Vec` duals (low-first, `Vec(w, A)`).
- **`Slice` trait** for the bits family (`fn slice(self, const lo, const hi) ->
  bits(hi - lo)`, `fn slice_from(self, const w, lo: uint(L)) -> bits(w)`, each with
  the guard) + **inherent** `slice`/`slice_from` on `Vec` (decision 1).
- **desugar** `x[a..b]` / `x[lo..+w]` / elision (read position) → the `slice` /
  `slice_from` method (compute the low end + `w`; **always-ascending**, low-first
  for both types — decision 6; enforce `..+` when the base is runtime). This
  replaces the backend's read-side `MExprKind::Slice` lowering (`slice_range_sv`
  for reads) with a desugaring; the **direction** check stays in `infer`.
- **flip the existing S4 bits syntax** to ascending: infer's `bits` arm and the
  `slice_bits`/`slice_elide` examples + goldens (`x[8..4]` → `x[4..8]`).
- **test**: promote a zero-width read example (`x[4..4]`, and a parametric
  `x[n..n]` at its limit grounded) to working + verilator-clean; the current
  S4 slice examples keep passing through the new path (golden review — they now
  emit `[lo +: w]` uniformly, decision 3, ascending, decision 6).

### Phase 2 — slice **set** zero-width guard (compiler-special)

- Keep `Projection::BitRange` (lvalue). Apply the `const if` guard *at the drive*
  in the compiler: grounded `w == 0` ⇒ emit nothing; symbolic ⇒ generate-if
  (Phase 4). No prelude fn (a set is not a value).
- **flip the set path to ascending** (decision 6): the `BitRange` direction +
  `slice_set` example/golden (`x[8..0] = …` → `x[0..8] = …`).
- **test**: `x[4..4] = y` (zero-width set) emits no drive and stays
  verilator-clean; existing slice-set examples unchanged.

### Phase 2b — symbolic slice bounds in mono_check (decision 5)

- **infer** keeps the *constant*-endpoint checks: direction (which end is low),
  width ≥ 0, and static bounds (`high ≤ N`, `low ≤ high`).
- **mono_check** gains the *symbolic-but-ground* slice obligations — width ≥ 0 and
  bounds — decided at instantiation, exactly like its existing negative-width
  positivity check. A runtime base is sim-time (or static when the base is
  statically bounded).
- Independent of the guard work; can land alongside Phase 1/2.

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

## Resolved decisions (with Jon, 2026-06-26)

1. **Slice surface.** Two prelude ops, split by the kind of `lo`:
   `slice(self, const lo: integer, const hi: integer)` (two-endpoint, const base —
   `x[a..b]`) and `slice_from(self, const w: integer, lo: uint(L))` (offset,
   runtime base — `x[lo..+w]`). **Convention:** named `{}` = elidable/inferable
   (self-width `W`, base-width `L`), positional `()` = must be provided (`lo`/`hi`/
   `w`) — even when `const`. A `const` positional param is a *const generic*
   (`sig.rs:1605`), so it is in `generic_params`, not value params, and S7's
   integer-value-param rule doesn't touch it. `w` is always const; `..+` is
   required when `lo` is variable. `slice_from`'s `lo` is `uint(L)` (a runtime
   net) — *not* `integer`, which is elided/not synthesized (see the integer-`int`
   note in "The shape"). **Surface = a trait** (decided): a trait method whose
   return is a function of its *own* const generics works today (`BitPack`'s
   `bits(bit_size)` precedent), so a `Slice` trait returning `bits(hi - lo)` is
   expressible now and is **user-extensible**. *Unifying* `bits` + `Vec` under one
   trait needs an **associated type** for the result shape (`bits(w)` vs
   `Vec(w, A)`), deferred from the trait core (`pack_resize.md`). So: a `Slice`
   trait for the **bits family** now + **inherent** `slice`/`slice_from` on `Vec`
   (`A` is the impl's generic → `Vec(hi - lo, A)` concrete); `x[a..b]` desugars to
   the method either way, unify under one trait when associated types land.
6. **Always-ascending direction.** `x[low..high]` is low-first for **both** `bits`
   and `Vec` (`bits` was high-first); width `high - low ≥ 0`, full `x[0..N]`.
   Removes the wart that the offset form was already low-first while two-endpoint
   `bits` was high-first (`x[4..8] ≡ x[4..+4]`), and the bits-high-first SV-mirror
   rationale is moot under `[lo +: w]` emission. **Changes the existing S4 path:**
   infer's `bits` direction arm, the slice-set `BitRange` direction, and the
   `slice_bits`/`slice_elide`/`slice_set` examples + goldens flip — folded into
   Phase 1 (read) / Phase 2 (set). See `slicing.md` "Semantics".
2. **Zero-width literal.** A prelude **builtin** — `zero_bits() -> bits(0)` and a
   `Vec` dual (or one polymorphic `zero<T>()`). Must flatten to the uniform
   effective-0-bit leaf (`[-1:0]`; `slicing.md` "Representation"), not to *no*
   leaves.
3. **Lowering form.** `[lo +: w]` **everywhere** — no `[msb:lo]` special case (a
   constant `lo` folds inside the indexed part-select).
4. **No per-def `const if` narrowing — it can't exist.** Whether a `const if` in
   an inline body grounds is a property of the **call site** (its const args), not
   the def, so `inline_check` (per-def, no call context) cannot classify it — the
   same `slice` grounds at `x[8..4]` and stays symbolic at `x[n..m]`. So Phase 0
   **drops the `inline_check` const-if rejection entirely**, folds at the *splice*
   (which has the call's args), and — until Phase 4 — emits a **clean call-site
   diagnostic** if it's still symbolic (today that path *panics*; that is a
   compiler-state artifact to fix, not a law).
5. **Direction = infer; bounds = infer (const) / mono_check (symbolic).**
   Direction (which end is low — bits high-first, vec low-first — plus the
   width-≥0 / ordering check on *constant* endpoints) is an `infer` thing. Bounds
   (`high ≤ N`, `low ≤ high`) and the width-≥0 check on **symbolic** endpoints
   that ground at instantiation go to **mono_check** — exactly like the
   negative-width residual it already decides; a *runtime* base is sim-time (or
   static if the base is statically bounded). Neither depends on the backend
   `Slice` node — both work on the slice's typed form.

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
