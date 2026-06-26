# Zero-width layout guards via prelude `const if` (workplan)

> **Progress (2026-06-26).** Done + committed: **Phase 0** (const-if folds inside
> an inline splice), the **ascending-direction flip** (decision 6: low-first for
> both `bits`/`Vec`, `[lo +: w]` everywhere), the **const slice bounds check**
> (Phase 2b's eager half), and **Phase 4** (`generate if` — `SvItem::GenerateIf`;
> a symbolic-const-generic `const if` now lowers to an SV conditional generate
> instead of being rejected; the `ConstIfNotConst` rejection is retired). So the
> whole `const if` axis (grounded fold inline, grounded fold at `mir_of`, and
> symbolic generate-if) is complete — the infrastructure the guards ride on.
> **Phase 1 prelude half LANDED** (`prelude.mrn` unblocked — the user's WIP was
> committed as checkpoint `6d49911`): `__slice_const`/`__slice_off` raw part-select
> primitives, `zero_bits() -> bits(0)`, and a `Slice` trait (bits) whose impl
> wraps them in a `const if w == 0 { zero_bits() } else { … }` guard; `mono_check`
> width-positivity relaxed `< 1` → `< 0` (width 0 is the legal effective-0-bit).
> **Phase 1 desugar LANDED for ground two-endpoint bits slices (2026-06-26):**
> `infer`'s Slice arm resolves `Slice::slice` (recorded as `method_resolution`);
> `mir_of` lowers a fully-ground two-endpoint `bits` slice to a `Call` (base width
> → impl generic in `substs` by name; `lo`/`hi` as named const args bound by the
> splice; receiver = base), so `x[4..8]` routes through the prelude guard →
> `(__inl0__self[4 +: 4])`. **Zero-width works:** `infer` allows a literal `h == l`
> slice, and `x[4..4]` folds the guard's `const if w == 0` to `zero_bits()` → `'0`
> into a `[-1:0]` effective-0-bit (`slice_zero_width.mrn`; verilator `-Wno-ASCRANGE`
> added for the intentional ascending range). **Offset form LANDED (2026-06-26):**
> `x[lo..+w]` routes to `Slice::slice_from{w}(lo)` — the const width rides as the
> `{w}` named arg, the runtime base `lo` as the value arg, and `slice_off_substs`
> binds both the base width and `lo`'s `uint(L)` width so the splice can declare
> the `lo` param wire. Ground (`slice_offset.mrn`) and symbolic-width
> (`slice_offset_param.mrn`, a `generate if`) both work; `infer` now allows a
> literal `w == 0` offset (mirrors the two-endpoint `h == l` allowance).
> **Concat guard LANDED (Phase 3, 2026-06-26):** `concat_hi` wraps its
> zero-width-operand case in a `const if` (`resize` needs none — the SV width-cast
> is already total). See Phase 3 below.
> **Zero-width non-routing slices now REJECTED (2026-06-26):** a zero-width slice
> only routes through the guard for a `bits` two-endpoint/offset form; a `Vec`
> slice or an elided `bits` form falls to the structural `slice_range_sv` path,
> which would emit an illegal zero-width part-select (`v[2 +: 0]` / a `[0:-1]`
> array range). `infer` now rejects those (`ZeroWidthSliceUnsupported`,
> `fail-expected/slice-vec-zero-width.mrn`) rather than miscompiling — negative
> space until a `Vec` guard lands.
> **Remaining (kept on the old structural `Slice` node / `slice_range_sv`, NON-zero
> width only):** elision (`x[lo..]` / `x[..hi]`) and `Vec` slices (inherent, not yet
> a resolvable method). A `Vec` zero-width guard (an inherent `slice` impl with a
> `zero_vec` primitive) would let those route too and is the path to deleting
> `slice_range_sv`'s read arm.
>
> **Symbolic slices — SOLVED + validated (2026-06-26).** Two blockers, both fixed:
> (1) *cross-frame rendering* — the inline splice rendered a caller generic against
> the callee sig (printed `W`/`hi` instead of `n`). Fixed: a `caller_const` helper
> pre-renders symbolic subst entries as a `Symbol` in the caller frame (`Deferred`
> placeholders pass through untouched), used by `compose_term` + the splice's
> named-arg loop; and `expr_value`'s `ConstParam` arm consults `self_subst`.
> (2) *divergent-arm `const if` typing* — the guard's arms used to differ in type
> (`bits(0)` then vs `bits(w)` else), so the const-if node mistyped as `bits(0)`,
> making the generate-if result wire `[-1:0]` instead of `[n-1:0]`. Fixed (Jon's
> insight) by making **`zero_bits {const w}() -> bits(w)`** (was `bits(0)`): both
> arms are now `bits(w)`, so the node types correctly — no divergent-type-arm
> feature needed, and no `where w == 0` clause (which would fire on the dead
> branch). `slice_param` now emits `if ((n - 1) == 0) { '0 } else { x[1 +: (n - 1)] }`
> with result wire `[(n-1)-1:0]`. **All `bits` slices now route through the guard —
> ground, symbolic, and zero-width.**
>
> _(Older note, for the general approach:)_ route
> `ExprKind::Slice` to a call of `Slice::slice` (two-endpoint) / `Slice::slice_from`
> (offset). Recommended path: in `infer`'s Slice arm resolve the `Slice` method on
> the base type (reuse `owner_of`/`trait_dispatch`/`select_by_header`/`call_def`/
> `pin_impl_self`, as `infer_type_path_call` does) and record `method_resolutions`
> + `call_substs` for the slice expr; in `mir_of` lower `ExprKind::Slice` to a
> `Call` (callee = resolved method, receiver = base, endpoints as named const args
> `{lo,hi}`/`{w}` + runtime `lo` value arg for offset) instead of
> `MExprKind::Slice`; the inline-splice + guard machinery then takes over and the
> backend `slice_range_sv` read path is deleted. Also relax `slice_literal` to
> allow a literal zero width. **Then Phase 2** (set guard) and **Phase 3**
> (concat/resize); the symbolic-width case of each is already served by Phase 4.
> (Also deferred: Phase 2b's symbolic-ground bounds, needing a recorded residual;
> and binding explicit const generics to non-inline instances — see Phase 4 note.)

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

> **Desugar finding (2026-06-26).** The surface shortcut `x.slice{lo,hi}()` does
> NOT work: it parses as `Call{ callee: Field{x,"slice"}, named:[lo,hi] }`, and
> `infer_call` returns `Type::Error` on a non-`Def` callee (then `mir_of` panics).
> The existing method machinery (`infer_method`/`call_def`) also can't *bind* a
> method's const generics from explicit endpoints (it infers generics from value
> args). So the desugar is **bespoke**: keep `ExprKind::Slice` (so `slice_literal`'s
> typing + const bounds stay), and in infer's Slice arm resolve the impl method via
> `owner_of(bt)` → `trait_dispatch(owner,"slice")` → `select_by_header`, record
> `method_resolutions[slice_expr]`, **and manually bind the method's `lo`/`hi`/`w`
> const generics from the endpoints into the recorded `call_substs`** (the part the
> machinery doesn't do for free); then `mir_of`'s Slice arm builds a `Call` from
> that resolution (receiver = base; endpoints already in the subst) instead of
> `MExprKind::Slice`, after which the inline-splice + guard machinery takes over
> and `slice_range_sv` (read) is deleted. This is the one remaining intricate step.

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

### Phase 2 — slice **set** zero-width guard (compiler-special) — DONE (grounded)

- **LANDED (2026-06-26):** `place_leaves_dir`'s `BitRange` arm calls
  `slice_width_is_zero` (mirrors `slice_range_sv`'s width computation, grounded
  via `self_subst`); a grounded `w == 0` set drives **nothing** (skips the illegal
  `[lo +: 0]`). The compiler-applied dual of the prelude read guard (a set is an
  lvalue, not a value). Ascending flip of the set path landed earlier. Existing
  slice-sets unchanged (non-zero ⇒ guard inactive). A *symbolic* zero-width set
  (generate-if for a drive) is deferred with the symbolic read case. No dedicated
  working example — a standalone zero-width set fails completeness (it drives
  nothing); the guard's real use is generic tiling where some iteration folds to 0.

### Phase 2b — slice bounds (decision 5)

- **infer const bounds — LANDED (2026-06-26).** `slice_literal` now returns a
  3-way `SliceTy` (`Ok` / `Oob` / `NotImpl`); when the high endpoint (or
  `offset + width`) and base length `N` both fold, `high > N` (and `low < 0`) is a
  clean `SliceOutOfBounds` diagnostic instead of an illegal `[lo +: w]` past the
  end. Direction (width ≥ 0) was already enforced. `fail-expected/slice-out-of-bounds.mrn`.
- **mono_check symbolic-ground bounds — LANDED (2026-06-26).** A *parametric* high
  endpoint (`x[0..k]` with `k` a const generic) that grounds out-of-range only at a
  literal instantiation is now caught. `infer` records a `SliceBoundsResidual
  { high, len }` whenever the eager `high <= N` check can't fold (mirrors
  `fit_residuals`); `mono_check` grounds both against the instantiation's subst and
  reports `high > len` ("slice out of range"). Covers two-endpoint and offset
  (const base) forms. `fail-expected/slice-symbolic-oob.mrn` (k=12 vs bits(8)) +
  unit test `mono_check_decides_slice_bounds` (bad k=12 / clean k=4). The eager
  const check (above) still handles the all-literal case.

### Phase 3 — `concat_hi` / `resize` guards via prelude `const if` — DONE (2026-06-26)

- **`concat_hi` guarded** (the real case). SV concat forbids a zero-width operand:
  `{y, <0-width>}` doesn't error-but-misparse — verilator reads the `[-1:0]` as a
  6-bit replicate and `WIDTHTRUNC`s. So `concat_hi` wraps the zero-width-operand
  case in a `const if`: when an operand folds to width 0 the result is the other
  operand, re-typed to `bits(n + m)` by `__resize_bits` (a free-fn width-cast
  primitive — Mirin has no `recv.method{generics}()` call syntax, so the guard
  can't write `x.resize{to}()`); the both-zero arm is `zero_bits`. Every arm is
  declared `bits(n + m)`, so the `const if` types cleanly. The nonzero path is
  `__concat` (the raw `{hi, lo}`). `concat_zero_width.mrn` (both operand sides)
  verilator-clean; `tuple_bitpack` unchanged semantically (extra inline layer).
- **`resize` needs NO guard.** SV's width-cast `to'(self)` is *already total* for a
  zero-width input — verilator accepts `8'(<0-width>)` cleanly (and `0'(x)` for a
  zero-width target). So a zero-width `resize` (uint/sint/bits) is fine as-is; no
  guard, and no `zero_uint`/`zero_sint` builtins are needed. (The earlier worry
  about "zero-width `self`" only bites concat, not the width-cast.)

### Phase 4 — symbolic widths: `generate if` (comptime_if step 5) — DONE

`SvItem::GenerateIf` (sibling of `SvItem::GenerateFor`) landed: a `const if`
whose condition is still symbolic at emit (a const generic riding as a `#()`
param) lowers to it (SV §27.5: only the selected block elaborates, so the dead
arm's out-of-range constructs never exist). The value-position lowering
(`const_if_generate`, `backend/lower.rs`) declares a fresh wire, captures each
branch's own items into its generate block (swapping `self.items`), and drives
the wire per branch; the cond renders as an SV expression over the `#()` params.
infer's `ConstIfNotConst` rejection is **retired** — a symbolic-const-generic
cond now grounds (inline splice) or generates (otherwise); only a *runtime*
(clocked) cond is still rejected (`ConstIfRuntimeCond`).
`examples/working/const_if_generate.mrn` (`-GW=8`) + golden + CLEAN +
VERILATOR_CLEAN; the old `fail-expected/const-if-symbolic-generic.mrn` promoted.

**Note (pre-existing, surfaced here):** an explicitly-passed const generic to a
non-inline *instance* (`choose{k=1}(…)`) isn't bound as `#(.k(1))` — it lands in
the call's named args with the subst slot deferred (the same shape Phase 0 fixed
for the *splice*), so `emit_instance_core` misses it. Out of scope for generate-if
(validated standalone via `-G`); fix when explicit const-generic instances are
exercised (mirror Phase 0's named-arg binding in `emit_instance_core`).

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
