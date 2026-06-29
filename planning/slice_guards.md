# Slice / concat zero-width guards (workplan — COMPLETE)

This workplan is **done**. The mechanism it built — keeping slices, `concat_hi`,
and packs total when a width folds to 0 — is documented as a stable compiler
feature in [docs/compiler/zero-width-handling.md](../docs/compiler/zero-width-handling.md).
The slice *surface* (syntax, semantics, lowering, bounds) lives in `slicing.md`.
This file keeps only the design decisions that aren't captured in either.

## What landed

- `const if` as a language construct: grounded fold inline (the inline splice) and
  at `mir_of`, symbolic → `SvItem::GenerateIf`. (`comptime_if.md` is the spec.)
- Prelude guards in `prelude.mrn`: `__slice_const`/`__slice_off`/`__concat` raw
  primitives + `zero_bits {const w}() -> bits(w)`, wrapped in `const if w == 0`
  guards; `concat_hi` likewise. `resize` needs none.
- Slice **set** zero guard (compiler-special, an lvalue) and **Vec** slice
  zero/symbolic handling (backend `slice_generate` + `'{default: '0}`).
- Bounds: eager const check in `infer` (`SliceOutOfBounds`); symbolic-but-grounding
  via a recorded residual decided in `mono_check`.

See `docs/compiler/zero-width-handling.md` for how/why; the git history of
`prelude.mrn`, `backend/lower.rs`, `backend/mono_check.rs`, and `hir/infer.rs`
for the implementation.

## Slice-surface decisions (not in slicing.md)

- **Two prelude ops, split by the kind of `lo`.** `slice(self, const lo, const hi)`
  (two-endpoint, const base — `x[a..b]`) and `slice_from(self, const w, lo: uint(L))`
  (offset, runtime base — `x[lo..+w]`). `w` is always const; `..+` is required when
  `lo` is variable. `slice_from`'s `lo` is `uint(L)` (a real net), not `integer`.
- **Named `{}` vs positional `()` convention.** Named = elidable/inferable
  (self-width `W`, base-width `L`); positional = must be provided (`lo`/`hi`/`w`),
  even when `const`. A `const` positional param is a *const generic* (in
  `generic_params`, not value params), so the integer-value-param rule doesn't
  touch it.
- **Trait for the bits family, inherent on `Vec`.** A `Slice` trait returns
  `bits(hi - lo)` (user-extensible, works today via the `BitPack`/`bit_size`
  precedent); `Vec` gets inherent `slice`/`slice_from` returning `Vec(hi - lo, A)`.
  Unifying the two under one trait needs an associated result type (deferred with
  the rest of associated types — `pack_resize.md`). `x[a..b]` desugars to the
  method either way.
- **Lowering form `[lo +: w]` everywhere** (no `[msb:lo]`), **ascending/low-first**
  for both `bits` and `Vec`.
- **Where checks live.** Direction + const-endpoint ordering → `infer`. Bounds:
  const → `infer`, symbolic-but-grounding → `mono_check` (a recorded residual). A
  `const if` is a *call-site* property, so there is no per-def `inline_check`
  narrowing — the fold happens at the splice, which has the call's args.
