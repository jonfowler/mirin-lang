# Slicing (`bits` and `Vec`)

Status: designed, not yet implemented. Slicing belongs to `bits` and `Vec`
directly (planning/bits.md §2, planning/vectors.md "deliberately later"); on
`uint`/`sint` you slice through `bits` (`u.pack()[..]`).

A slice selects a contiguous run of bits (from a `bits(N)`) or elements (from a
`Vec(N, A)`). The result is `bits(w)` / `Vec(w, A)`. Single-bit/element indexing
(`x[i]` → `bool` / element) is unchanged and distinct from a width-1 slice
(`x[i..+1]` → `bits(1)` / `Vec(1, A)`).

## Syntax — Rust-style `..`

Two forms, half-open:

```
x[a..b]        // two endpoints
x[off..+w]     // offset + constant width   (SV indexed part-select)
x[a..]  x[..b] // elide one end
```

`..` over `:` deliberately: `:` is already type ascription / named results, and
it carries SV's *inclusive* `[hi:lo]` reading — exactly the convention we are
overturning. `..` reads as half-open (Rust/Python), so the exclusive end is no
surprise, and it leaves room for a future `a..b` range value to unify with
`range(n)`.

## Semantics — half-open, ascending (low-first), for both `bits` and `Vec`

A slice is the half-open interval `[low, high)`, written **low-first / ascending
for both `bits` and `Vec`**: `x[low..high]` selects indices `{low, …, high-1}`.
The high endpoint is always exclusive, so the width is `high - low` (Rust-style
length: write `x[4..len]`, never `x[4..len-1]`). The full value is `x[0..N]`.

> **Ascending for both (decided 2026-06-26).** `bits` was previously written
> high-first (`x[8..4]`) to mirror SV's `[msb:lo]`. Dropped: we emit the indexed
> part-select `[low +: w]` regardless (see "SV lowering"), so source order need
> not mirror SV bit order — and a single ascending form removes the wart that the
> offset form `x[lo..+w]` was low-first while the two-endpoint `bits` form was
> high-first. Both forms now anchor the low end: `x[4..8] ≡ x[4..+4]`.

so `x[4..8]` = bits `{4,5,6,7}` = SV `x[7:4]` — **4 bits, not 5**; to include bit
8 write `x[4..9]`. `v[2..5]` = elements `{2,3,4}`.

**Direction is enforced.** The width `high - low` must be `≥ 0` (i.e. `high ≥
low`) for both types; `high < low` is the wrong-order error (hint to swap the
endpoints). A **zero** width is allowed (below).

**Elision** defaults the missing end (same rule for both types now):

```
x[lo..]  ⇒  x[lo..N]      x[..hi]  ⇒  x[0..hi]
```

Bare `x[..]` is redundant with `x` and is rejected.

## SV lowering — width must be constant, base may be runtime

SystemVerilog's one hard rule (IEEE 1800; confirmed across packed and unpacked):

- constant part-select `x[msb:lo]` — **both** bounds must be constant.
- indexed part-select `x[base +: w]` / `x[base -: w]` — **base may be a runtime
  variable, but `w` must be a constant.**
- a **variable width is illegal SV** — there is no legal form, in any context
  (not "valid but unsynthesizable"; the tool rejects it).

So every Mirin slice lowers through *(low endpoint, constant width)*, and emits
the indexed part-select **`[low +: width]` uniformly** (decision 2026-06-26 — no
`[msb:lo]` special case; a constant `low` folds inside `[low +: w]`, so SV quality
is unaffected):

| Mirin | width | → SV |
|---|---|---|
| `x[4..8]` (const) | 4 | `x[4 +: 4]` |
| `x[i..+4]` (runtime base) | 4 | `x[i +: 4]` |
| `v[2..5]` (const) | 3 | `v[2 +: 3]` |
| `v[i..+3]` (runtime base) | 3 | `v[i +: 3]` |
| width does not fold to a constant | — | **error: slice width must be constant** |

Rule: compute `width = high - low`; it must fold to a constant. Always emit
`[low +: width]` — uniform across `bits`/`Vec` and both syntactic forms, so we
never need `-:` (we always anchor at the known low end). The **low endpoint may be
runtime** (the mux-style slice), but only via the offset form: `x[lo..+w]` allows
a runtime `lo`; the two-endpoint `x[a..b]` requires `a` constant (write
`x[lo..+w]` for a runtime base). The slice itself lowers to the prelude `slice` /
`slice_from` ops (`planning/slice_guards.md`), not a dedicated backend node.

`uint`/`sint` do not slice directly — `u.pack()[hi..lo]` (no dedicated accessor
for now).

## Slice as an lvalue (slice-set)

`x[a..b] = y` and `x[off..+w] = y` assign into the slice; the RHS width must
match. This rides the partial-drive completeness machinery already built for
`b[i] = …` — a slice whose base uses a `for`-genvar is recognised as covering
its run via `index_uses_forbound`, like a compound index drive.

## Zero-width values

A zero-width slice (`x[4..4]`, `x[i..+0]`, any width that folds to 0) is **total,
not an error** — generic code routinely folds a width to 0 at its limit. How the
compiler keeps it total (degenerate `[-1:0]` / `[0:-1]` nets, `const if` guards on
the layout primitives, the read/set/Vec split) is documented once in
[docs/compiler/zero-width-handling.md](../docs/compiler/zero-width-handling.md).

## Direction and bounds checks (where they live)

**Direction** — which end is "low" (bits high-first, vec low-first) and, for
*constant* endpoints, the width-≥0 / ordering check — is an **`infer`** thing
(structural + const arithmetic infer already does): ascending-bits / descending-vec
errors fire there.

**Bounds** — `high ≤ N`, `low ≥ 0` — are static (mirroring single-index bounds)
when endpoints fold to constants: checked in **`infer`** (`slice_literal` →
`SliceOutOfBounds`). When endpoints are **symbolic but ground at instantiation**,
the bounds defer to **`mono_check`** (a recorded residual). A runtime base with
constant width is bounds-checked
only when the base is statically bounded; otherwise it is a simulation-time
concern. (Zero width is allowed — see the zero-width doc linked above.)

## Deferred

- **`-:` (top-anchored) form** — unneeded; we always anchor at the low end.
- **Variable-width slices** — no legal SV form (would need a barrel-shifter;
  not a slice).
- **Surface concatenation** (the dual of slicing) — shares the wanted
  `SvExpr::Concat`/`Slice` backend nodes (planning/pack_resize.md).
