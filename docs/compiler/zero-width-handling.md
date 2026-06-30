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

- **Producers stay total — and value-correct — by construction.** Two hazards at
  width 0. *Legality:* a size cast `to'(x)` is an illegal `0'(x)`, a `[lo +: 0]`
  part-select is out-of-range, `[hi:lo]` reverses, a concat operand `{a, <0-width>}`
  miscompiles. *Value:* a zero-width net has no defined value — SV leaves widening
  one tool-defined, possibly **X** — so an op that *reads* a zero-width source must
  not depend on it. Each bits-family op is lowered to arithmetic that is total at 0
  *and* never trusts a zero-bit value, cast onto the op's result **net** (a
  statement-form inline body materializes a real wire of its result type):
  - **truncate / truncate_lsb** → the width cast `type(result)'(x)` (`>>` first for
    the lsb form): the zero-width case is the *result*, which reads no bits.
  - **extend / resize** → `(n != 0) ? type(result)'(x) : '0`. The cast does the
    width coercion (zero/sign-extend or truncate); when the *source* is zero-width
    (`n == 0`) the result is forced to `'0` — a zero-width value is the empty sum of
    base powers, i.e. 0, signed or not — without reading the zero-bit net.
  - **extend_lsb** → `type(result)'(x) << (to-n)`: at `n == 0` the `<< (to-n)`
    moves the (zero-width) operand fully out, so it is 0 whatever its bits.
  - **bit slice** `x[lo +: w]` → `type(result)'(x >> lo)` (`lo` const or runtime):
    shift then truncate; `x` is a real source, the zero-width is only the result.
  - **concat_hi** `{hi, lo}` →
    `(type(result)'(hi) << n) | (type(result)'(lo) & ~('1 << n))`. Both operands
    can be zero-width without leaking a (possibly X) value into the other's range:
    the high operand is shifted out when `m == 0` (`X << n == 0`), and the low
    operand is masked — `~('1 << n)` is 0 at `n == 0`, and `X & 0 == 0`. At `n > 0`
    the mask is the low-`n`-ones (redundant with the cast's zero-fill, harmless).

- **Two cases that aren't a single packed cast** keep a structural lowering:
  - **Vec slices** (multi-leaf, unpacked) and **elided** bit slices stay in the
    backend `Slice` node: ground-zero emits the empty value, a symbolic width
    emits a per-leaf `generate if` (SV §27.5 elaborates only the taken arm).
  - **slice set** is an lvalue: a grounded zero-width drive is skipped (drives
    nothing) — the dual of the read.

(Cosmetic edge: a bit slice of a 1-bit value at its top — `x[1..1]`, `W==1` — emits
`x >> 1`, which trips a `WIDTHEXPAND` lint at that one degenerate instantiation;
the result is still a correct zero-width `'0`. Narrower than `ASCRANGE` and absent
from the corpus.)

## Why the surface area is small

The work sits at the *producer*. The moment a layout op would yield zero width it
yields a representable degenerate net instead — by construction (a width cast /
shift that is total at 0), not by a guard wrapped around a fragile primitive.
Every *consumer* — arithmetic, comparison, registers, instance hookup, port
emission, equation wiring — then sees an ordinary (degenerate) net, not a special
"absent" case. So:

- No operator special-cases zero width (`+`, `==`, `reg`, … are untouched). Only
  the handful of layout ops that actually *produce* zero width carry any
  zero-width logic.
- No type-system change: there is no special `bits(0)` type, and zero-width is not
  a distinct case in inference or monomorphisation. Bounds checks already treat
  width 0 as legal (only `< 0` is rejected).
- The zero-width logic lives in two places: `prelude.mrn` (the bits family — cast
  and shift forms, total at 0 by construction) and the backend `Slice` node (Vec
  and elided slices, slice set).

Keeping the net rather than dropping it to "no net" is also what lets a
zero-width value cross a module boundary (see *The problem*): a degenerate
`[-1:0]` / `[0:-1]` port is fine where a vanished one would be unrepresentable.

## Disadvantage: downstream warnings

The degenerate ranges trip a verilator lint that must be suppressed project-wide:
`ASCRANGE` (the `[-1:0]` / `[0:-1]` ranges). This is cosmetic for our intentional
zero-width nets, but suppressing it globally means a *genuinely* mis-ordered range
elsewhere would not be flagged. It is relatively safe because the compiler never
emits a descending `[a:b]` range otherwise (bits slices lower to a shift; the
backend `Slice` node uses the ascending `[lo +: w]` form), so the only ascending
ranges in generated SV are the intentional zero-width ones.

Note we do **not** need `-Wno-UNDRIVEN`: the empty values are *driven* (`'0` /
`'{default: '0}`), not left undriven. Driving was the deciding factor in choosing
the degenerate-net representation over a dropped-port one.
