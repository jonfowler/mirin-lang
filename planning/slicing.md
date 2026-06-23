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

## Semantics — half-open, high endpoint exclusive

A slice is the half-open interval `[low, high)`: **the high endpoint is always
exclusive**, so the width is `high - low` (Rust-style length: write `x[4..len]`,
never `x[4..len-1]`).

Write order encodes the type's natural direction:

- **`Vec` is written low-first**, ascending: `v[low..high]` → elements
  `{low, …, high-1}`. Full vector is `v[0..N]`.
- **`bits` is written high-first**, descending: `x[high..low]` → bits
  `{low, …, high-1}`, matching SV's MSB:LSB reading. Full word is `x[N..0]`.

so `x[8..4]` = bits `{4,5,6,7}` = SV `x[7:4]` — **4 bits, not 5**; to include
bit 8 write `x[9..4]`. The two-endpoint and offset forms agree:
`x[8..4]` ≡ `x[4..+4]` (the offset is always the low/base end, never reversed).

**Direction is enforced.** The width `high - low` must be `≥ 0` in the type's
natural direction:

- ascending on `bits` (`x[4..8]`) → error, hint to write `x[8..4]` / `x[4..+4]`.
- descending on `Vec` (`v[5..2]`) → error, hint to write `v[2..5]`.

A negative width is the wrong-order error; a **zero** width is allowed (below).

**Elision** defaults the missing end to the start/length of the natural
direction:

```
bits:  x[hi..]  ⇒  x[hi..0]      x[..lo]  ⇒  x[N..lo]
Vec:   v[lo..]  ⇒  v[lo..N]      v[..hi]  ⇒  v[0..hi]
```

Bare `x[..]` is redundant with `x` and is rejected.

## SV lowering — width must be constant, base may be runtime

SystemVerilog's one hard rule (IEEE 1800; confirmed across packed and unpacked):

- constant part-select `x[msb:lo]` — **both** bounds must be constant.
- indexed part-select `x[base +: w]` / `x[base -: w]` — **base may be a runtime
  variable, but `w` must be a constant.**
- a **variable width is illegal SV** — there is no legal form, in any context
  (not "valid but unsynthesizable"; the tool rejects it).

So every Mirin slice lowers through *(low endpoint, constant width)*:

| Mirin | width | → SV |
|---|---|---|
| `x[8..4]` (const) | 4 | `x[7:4]` — plain part-select (nicest) |
| `x[i..+4]` (runtime base) | 4 | `x[i +: 4]` |
| `x[i..i+4]` | 4 (folds) | `x[i +: 4]` |
| `v[2..5]` (const) | 3 | `v[4:2]` |
| `v[i..+3]` (runtime base) | 3 | `v[i +: 3]` |
| width does not fold to a constant | — | **error: slice width must be constant** |

Rule: compute `width = high - low`; it must fold to a constant. Emit a plain
`[msb:lo]` when `low` is *also* constant (cleaner Verilog), otherwise
`[low +: width]`. This is uniform across `bits`/`Vec` and both syntactic forms,
so we never need `-:` (we always anchor at the known low end).

`uint`/`sint` do not slice directly — `u.pack()[hi..lo]` (no dedicated accessor
for now).

## Slice as an lvalue (slice-set)

`x[a..b] = y` and `x[off..+w] = y` assign into the slice; the RHS width must
match. This rides the partial-drive completeness machinery already built for
`b[i] = …` — a slice whose base uses a `for`-genvar is recognised as covering
its run via `index_uses_forbound`, like a compound index drive.

## Zero-width slices — vacuous, zero leaves

A zero-width slice (`x[4..4]`, `x[i..+0]`, or any width that folds to 0) is
**not an error** — generic code routinely folds a width to 0 at its limit, and
erroring there forces an `n == 0` special case into every parameterised
construction.

SV has no zero-width signal, and range-underflow does not give us one: at
`N == 0`, `logic [N-1:0]` becomes `logic [-1:0]`, which is legal syntax
(negative-index ranges are allowed) but is *not* zero-width — it is a 2-bit
ascending vector by the usual `|msb − lsb| + 1` size rule (some tools instead
apply the signed `msb − lsb + 1 = 0` and reject it as a reversed range). Either
way the width is never reliably 0, so underflowing silently yields a *wrong*
2-bit signal — worse than an error. The standard fix is to *remove zero-width
signals entirely* — which is exactly Mirin's leaf model: a zero-width value
(`bits(0)`, `Vec(0, A)`) has **no leaves**, so it emits no SV at all. This works because widths are grounded at
monomorphisation, so a zero width is known at emit time. Consequences, all the
right thing:

- `let z: bits(0) = x[4..4];` declares nothing.
- `x[4..4] = y` emits no assigns (no-op) — never a degenerate `[base +: 0]`.
- a `bits(0)` result is a port with zero pins (absent).
- generic pack/unpack stays **total**: the `n == 0` arm is the identity element.

A *literally* constant zero-width slice is almost always a typo, so it earns a
**warning** (not an error) — totality without losing the diagnostic.

**v1 limitation.** When a width rides as a *symbolic* SV parameter
(`#(parameter int N)`) and could be 0 at an SV-level instantiation, the generic
body still emits `[N-1:0]` / `[base +: N]`, which break at `N == 0`. Guarding
that needs `generate`/`if` and is deferred; v1 does not emit such guards. The
grounded-zero case (the common one) is fully handled.

## Bounds checks

Static check when endpoints are constant (mirrors single-index bounds): require
`high ≤ N` and `low ≤ high` (zero width allowed, negative width rejected as the
direction error). A runtime base with constant width is bounds-checked only when
the base is statically bounded; otherwise it is a simulation-time concern.

## Deferred

- **`-:` (top-anchored) form** — unneeded; we always anchor at the low end.
- **Variable-width slices** — no legal SV form (would need a barrel-shifter;
  not a slice).
- **`generate`-guarded zero-width for symbolic SV-parameter widths** (above).
- **Surface concatenation** (the dual of slicing) — shares the wanted
  `SvExpr::Concat`/`Slice` backend nodes (planning/pack_resize.md). Designing
  the pair together is what lets zero-width values compose cleanly through a
  concat (a text-spliced `{${a}, ${b}}` cannot drop a zero-width operand; a
  structured `Concat` node over leaves can).
```
