# Zero-width handling

How the compiler keeps generic code total at its width limits, where a slice,
concat, or pack collapses to zero bits/elements.

## The problem

Generic hardware code routinely produces zero-width values at its parameter
limits: `x[lo..hi]` with `hi == lo`, an offset slice `x[lo..+w]` with `w == 0`, a
`concat_hi` with a zero-width operand, a `pack` of a zero-width element. These are
not user errors — they are the boundary cases generic code must handle to be
correct over its whole parameter range.

SystemVerilog has no first-class zero-width type, and its degenerate forms are
sharp:

- A zero-width **packed** vector is `[-1:0]` — a degenerate ascending range
  (verilator: `ASCRANGE`), tolerated as 0 bits.
- A zero-length **unpacked** array is `[0:-1]` — the array analog, tolerated as
  empty.
- A zero-width **part-select** `x[lo +: 0]` is an out-of-range **error**.
- A zero-width **concat operand** `{a, <0-width>}` *miscompiles*: verilator reads
  the `[-1:0]` operand as a 2-bit replicate (`WIDTHTRUNC`).
- A packed zero-width net is fillable by `'0`; an unpacked zero-length array is
  **not** (`'0` type-errors, `'{}` crashes verilator) — only the default pattern
  `'{default: '0}` fills it.

So naively lowering generic layout ops emits illegal or silently-wrong SV exactly
at the limits.

## The solution

Represent a zero-width value as a **degenerate-but-real net**, and guard only the
**producers** that can't emit one directly.

- **Representation.** `bits(0)` is one `[-1:0]` leaf; `Vec(0, A)` is one `[0:-1]`
  leaf per element-leaf of `A`. These flow through the normal flatten/leaf
  machinery unchanged. The empty *value* is `'0` for a packed leaf and
  `'{default: '0}` for an unpacked-array leaf (the array analog of `'0`).

- **Guards on layout ops, two mechanisms:**
  1. *Prelude `const if`* for the Mirin-expressed ops (`prelude.mrn`). The raw
     primitives (`__slice_const`, `__slice_off`, `__concat`) assume width ≥ 1; a
     `const if w == 0 { zero_bits{w}() } else { <primitive> }` wraps each. The
     empty value is `zero_bits {const w}() -> bits(w)` — a `w`-bit `'0`, only ever
     taken at `w == 0`. It returns `bits(w)` (not `bits(0)`) so both arms of the
     `const if` share a type — no divergent-arm typing. A *symbolic* width lowers
     the `const if` to a `generate if` (SV §27.5 elaborates only the taken arm).
  2. *Compiler-special* for constructs that aren't plain values. The slice **set**
     is an lvalue: a grounded zero-width drive is skipped (drives nothing), the
     dual of the read guard. **Vec** slices (multi-leaf, unpacked) are handled in
     the backend: ground-zero emits the empty value, symbolic emits a per-leaf
     `generate if`.

`resize` needs **no** guard — the SV width-cast `to'(x)` is already total for a
zero-width source or target.

## Why the surface area is small

The guard sits at the *producer*. The moment a layout op would yield zero width,
it yields a representable degenerate net instead. Every *consumer* — arithmetic,
comparison, registers, instance hookup, port emission, equation wiring — then sees
an ordinary (degenerate) net, not a special "absent" case. So:

- No operator special-cases zero width (`+`, `==`, `reg`, … are untouched). Only
  the handful of layout ops that actually *produce* zero width carry a guard.
- No type-system change: there is no special `bits(0)` type, and zero-width is not
  a distinct case in inference or monomorphisation. Bounds checks already treat
  width 0 as legal (only `< 0` is rejected).
- The guards live in one place each: `prelude.mrn` for the bits family, and two
  backend arms (slice read, slice set) for the rest.

Keeping the net (rather than dropping it to "no net") is what makes a zero-width
value cross a module boundary: a parametric module has a fixed port list, so a
port that *vanished* at length 0 would be unrepresentable — but a degenerate
`[-1:0]` / `[0:-1]` port is fine, exactly like any other width.

## Disadvantage: downstream warnings

The degenerate ranges trip a verilator lint that must be suppressed project-wide:
`ASCRANGE` (the `[-1:0]` / `[0:-1]` ranges). This is cosmetic for our intentional
zero-width nets, but suppressing it globally means a *genuinely* mis-ordered range
elsewhere would not be flagged. It is relatively safe because the compiler never
emits `[a:b]`-style ranges otherwise (slices lower to `[lo +: w]`), so the only
ascending ranges in generated SV are the intentional zero-width ones.

Note we do **not** need `-Wno-UNDRIVEN`: the empty values are *driven* (`'0` /
`'{default: '0}`), not left undriven. Driving was the deciding factor in choosing
the degenerate-net representation over a dropped-port one.
