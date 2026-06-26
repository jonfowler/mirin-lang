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

So every Mirin slice lowers through *(low endpoint, constant width)*, and emits
the indexed part-select **`[low +: width]` uniformly** (decision 2026-06-26 — no
`[msb:lo]` special case; a constant `low` folds inside `[low +: w]`, so SV quality
is unaffected):

| Mirin | width | → SV |
|---|---|---|
| `x[8..4]` (const) | 4 | `x[4 +: 4]` |
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

## Zero-width values — effective-0-bit, guarded at the layout primitives

A zero-width slice (`x[4..4]`, `x[i..+0]`, or any width that folds to 0) is
**not an error** — generic code routinely folds a width to 0 at its limit, and
erroring there forces an `n == 0` special case into every parameterised
construction.

### Rejected: drop to zero leaves

The tempting fix — represent `bits(0)`/`Vec(0, A)` as *no leaves* and emit
nothing — is rejected: it makes a value's **port/leaf structure depend on a
width**. A module carrying `bits(M)` would then need a distinct
monomorphisation for every zero/non-zero combination of its widths, and
transitively for every *internal* intermediate that can fold to zero, across
calls into further modules. `concat_with_middle {M, N} (x: bits(M), bool,
y: bits(N)) -> bits(M+1+N)` would split into four structural forms instead of
one parameterised module — a monomorphisation explosion, and finicky to track.

### Representation: uniform `[W-1:0]`, an effective-0-bit signal

Representation stays uniform: a zero-width value is just `[W-1:0]` at `W == 0`.
The LRM blesses this — the range spec (§7.4.1) says msb/lsb "may be any integer
value—positive, negative, or zero ... The lsb value may be greater than, equal
to, or less than the msb value," with the example `logic [-1:4] b;` a 6-bit
vector. So `logic [-1:0]` is a **legal** (nominally 2-bit) "effective-0-bit"
signal: its bits are never meaningfully consumed, and its port/leaf shape is
identical to any other width — so **nothing monomorphises on zero-ness** and
there is no explosion. Plain pass-through is unaffected: `x + y`, port
connections, and `assign` on `[-1:0]` operands are all legal and harmless.

### The problem is local to the layout primitives

Only operations whose emitted SV computes a bit *layout* from a width go wrong
on a zero, and only two do:

- **slice** with output width 0 → `x[hi-1:lo]` is a reversed/empty range
  `x[lo-1:lo]` (illegal).
- **concat** with a zero-width operand → a `[-1:0]` operand contributes junk
  bits, and an all-zero concat is illegal.

Zero *padding* needs nothing: §11.4.12.1 — a zero replication `{0{x}}` "is
considered to have a size of zero and is ignored," provided the concat has ≥1
positive-size operand. So `extend`/`resize`'s `{ {(to-n){1'b0}}, self }` is
already correct at `to == n`; only a zero-width `self` needs the concat guard.

### The fix: a `const if` guard written in the prelude — NOT backend-synthesised

> **Direction (2026-06-26):** the guard is **not** synthesised in the backend.
> The layout operations are *primitives that do not support zero-width*, and the
> guard is a Mirin `const if` wrapping them — the **read** in `prelude.mrn`, the
> **set** applied by the compiler (it is an lvalue, not a value). Full plan +
> rationale: `planning/slice_guards.md`.

Guard the layout primitive with a **`const if`** on the (output) width — for
slice, on `hi - lo`, which also covers a zero-width input (you cannot slice a
positive width out of nothing). For the **read**, this is an ordinary `#[inline]`
Mirin fn in the prelude — what `x[a..b]` desugars to:

```mirin
#[inline]
fn slice {const lo, const w} (x: bits(W)) -> bits(w) {
    const if w == 0 { zero_bits() }        // zero result — never read
    else            { __slice_raw(x, lo) } // the raw part-select primitive (w >= 1)
}
```

`__slice_raw` is a verilog-bodied primitive (`${x}[${lo} +: ${w}]`) that assumes
`w >= 1`; zero-ness is the `const if`'s job, not the primitive's. Because `slice`
is `#[inline]`, the guard folds at the call site. Two lowering modes, by whether
the width is known at emit:

- **grounded** (the width folds to a literal — the common case): the inline
  splice folds the `const if` to the taken branch. No generate, no `[-1:0]`
  select ever reaches the tool. (Needs `const if`-in-inline — `slice_guards.md`
  Phase 0/1.)
- **symbolic** (the width rides as an SV `#()` parameter): the *same* `const if`
  lowers to an SV **generate if**. §27.5: a conditional generate "select[s] at
  most one generate block ... instantiated into the model," so the dead
  `else { __slice_raw }` is **not elaborated** and its out-of-range select is
  never checked. A *procedural* `always_comb if` will not do — both arms
  elaborate. (`slice_guards.md` Phase 4 / comptime_if step 5.)

The **set** (`x[a..b] = y`) is the dual but cannot be a value-returning prelude
fn — it drives a place — so it stays a **special compiler** construct: the
compiler keeps the `BitRange` place and applies the same `const if` guard at the
drive (grounded `w == 0` ⇒ no drive; symbolic ⇒ generate-if).

This one mechanism — `const if`, in the prelude for reads, compiler-applied for
sets — lets a single parameterised module (`#(W)`) cover every width including
zero, with **no zero-width logic in the backend** (the alternative to both the
rejected per-pattern explosion *and* bespoke backend guard synthesis).

A *literally* constant zero-width slice is almost always a typo, so it earns a
**warning** (not an error) — totality without losing the diagnostic.

### Prerequisite — `const if` through `#[inline]`, then `generate if`

The read guard is a `const if` inside an `#[inline]` body, so it needs the
inline-splice + `const if` fold (`slice_guards.md` Phase 0) — the grounded case,
which covers most real code and lands first. The symbolic case additionally needs
the **`generate if`** lowering: like the structural generate-`for`
(`SvItem::GenerateFor`), a const-conditioned `if` that stays symbolic at emit
becomes an `SvItem::GenerateIf` (new). The grounded fold needs no new SV node;
the generate-if follows (Phase 4). Exposing `const if` as a language construct is
what lets the prelude primitives (slice, `resize`, `concat_hi`) and user code all
express the same guard — instead of the backend synthesising it.

## Direction and bounds checks (where they live)

**Direction** — which end is "low" (bits high-first, vec low-first) and, for
*constant* endpoints, the width-≥0 / ordering check — is an **`infer`** thing
(structural + const arithmetic infer already does): ascending-bits / descending-vec
errors fire there.

**Bounds** — `high ≤ N`, `low ≤ high` — are static (mirroring single-index bounds)
when endpoints are constant: checked in **`infer`**. When endpoints are **symbolic
but ground at instantiation**, the width-≥0 and bounds checks defer to
**`mono_check`**, exactly like the negative-width residual it already decides. A
runtime base with constant width is bounds-checked only when the base is
statically bounded; otherwise it is a simulation-time concern. (Zero width is
allowed; a *literal* zero-width slice earns a warning, above.)

## Deferred

- **`-:` (top-anchored) form** — unneeded; we always anchor at the low end.
- **Variable-width slices** — no legal SV form (would need a barrel-shifter;
  not a slice).
- **Generate-if (symbolic-width) zero guard** — the grounded-width fold lands
  first (covers most code); the `SvItem::GenerateIf` lowering follows. Both are
  part of this design (see "Zero-width values"), not a separate workstream.
- **Surface concatenation** (the dual of slicing) — shares the wanted
  `SvExpr::Concat`/`Slice` backend nodes (planning/pack_resize.md), and is the
  other layout primitive that needs the zero-width concat guard. Worth designing
  alongside so the guard machinery is built once.
