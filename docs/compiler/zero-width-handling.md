# Zero-width handling

How the compiler keeps generic code total at its width limits, where a slice,
concat, or pack collapses to zero bits/elements.

## The problem

SystemVerilog has no native zero-width type.

Generic hardware code, though, routinely produces zero-width values at its
parameter limits: `x[lo..hi]` with `hi == lo`, an offset slice `x[lo..+w]` with
`w == 0`, a `concat_hi` with a zero-width operand, a `pack` of a zero-width
element. These are not user errors — they are the boundary cases generic code
must handle to be correct over its whole parameter range.

Stripping them away is not an option. A zero-width value still has to cross
module boundaries, and a parametric module has a fixed port list: a port that
*vanished* at length 0 would be unrepresentable. So zero-width values need an SV
representation even when they carry no bits — and SV offers no clean one. Lower
the layout ops naively and they emit illegal or silently-wrong SV exactly at the
limits.

## The solution

Represent a zero-width value as a **degenerate-but-real net**, and guard only the
**producers** that can't emit one directly.

- **Representation.** When a parametric range collapses to zero width it reduces
  to a degenerate SV range, and we keep exactly that as the net: `bits(0)` is one
  `[-1:0]` leaf, `Vec(0, A)` is one `[0:-1]` leaf per element-leaf of `A`.
  SystemVerilog treats both as genuinely zero-sized almost everywhere, so they
  flow through the normal flatten/leaf machinery unchanged. The empty *value* is
  `'0` for a packed leaf and `'{default: '0}` for an unpacked-array leaf (the
  array analog of `'0` — plain `'0` type-errors on it and `'{}` crashes
  verilator).

- **Guards on layout ops.** Three positions are *not* tolerant, and they are
  exactly the producers we guard: a part-select `x[lo +: 0]` is an out-of-range
  **error**; a concat operand `{a, <0-width>}` *miscompiles* (verilator reads the
  `[-1:0]` operand as a 2-bit replicate, `WIDTHTRUNC`); and a zero-width drive
  (slice set) must write nothing rather than emit a degenerate part-select. Two
  mechanisms handle them:
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

The **resize family** (`resize`, `extend`, `truncate`, `extend_lsb`,
`truncate_lsb`) needs no `const if` — but not because its old form was total. The
naive lowerings all break at width 0: a size cast `to'(x)` is an illegal `0'(x)`,
a `[to-1:0]` part-select reverses, a `{(to-n){…}}` replicate goes zero-count. The
fix routes each through its result **net**: a statement-form inline body
materializes a real wire of its result type, so the body can name it. The
MSB-aligned ops (`resize`/`extend`/`truncate`) become the type cast
`type(result)'(x)`, which coerces width identically (zero/sign-extend or truncate)
yet stays legal at zero width. The LSB-aligned ops shift the window into place —
`extend_lsb` is `type(result)'(x) << (to-n)`, `truncate_lsb` is
`type(result)'(x >> (n-to))` — a shift, never a reversed part-select, so both are
total at width 0 too.

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

Keeping the net rather than dropping it to "no net" is also what lets a
zero-width value cross a module boundary (see *The problem*): a degenerate
`[-1:0]` / `[0:-1]` port is fine where a vanished one would be unrepresentable.

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
