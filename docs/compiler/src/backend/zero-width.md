# Zero-width handling

Generic hardware routinely produces zero-width values at the limits of its
parameter range: a slice `x[lo..hi]` where `hi == lo`, an offset slice `x[lo..+w]`
where `w == 0`, a concatenation with a zero-width operand, a pack of a zero-width
element. These are not user errors — they are the boundary cases generic code must
handle to be correct across its whole range. SystemVerilog, though, has no
zero-width type, so the backend has to give a zero-width value a representation and
keep the operations that produce one legal. This chapter covers how. Zero width is
wholly a backend concern: the types and values upstream never special-case it.

## The problem

SystemVerilog offers no clean zero-width value. Lower a slice, cast, or pack
naively and it emits illegal or silently wrong SV exactly at the limits — a size cast
`to'(x)` becomes an illegal `0'(x)`, a part-select `[lo +: 0]` is out of range, a
range `[hi:lo]` reverses.

Stripping the zero-width value away is not an option either. A zero-width value
still has to cross a module boundary, and a parametric module has a fixed port
list — a port that *vanished* at length zero would be unrepresentable. So the value
needs an SV form even when it carries no bits.

## A degenerate but real net

The representation is a degenerate-but-real net: when a parametric range collapses
to zero width, the backend keeps exactly that degenerate range as the net. A
`bits(0)` is one `[-1:0]` leaf; a `Vec(0, A)` is one `[0:-1]` leaf per element-leaf
of `A`. SystemVerilog treats both as genuinely zero-sized almost everywhere, so
they flow through the normal flatten-and-leaf machinery unchanged. The empty value
is `'0` for a packed leaf and `'{default: '0}` for an unpacked-array leaf.

## Guarding the producers

With the representation settled, only the *producers* that cannot emit a degenerate
net directly need guarding, and the guard depends on whether the width is known:

- **Known zero** — the backend emits the empty value and skips the part-select or
  drive entirely; there is nothing to read or write.
- **Possibly zero** (a symbolic width that might evaluate to zero) — the backend
  wraps the producer in a `generate if (width != 0)`, so the illegal arm never
  elaborates at the instantiation where the width is zero.

The bits-family arithmetic operations need no backend guard: their prelude
definitions are written to be total at zero width by construction — a shift past
the operand, a mask that vanishes — so a zero-width input flows through them
without producing an illegal or undefined value.

That keeps the emitted Verilog legal and value-correct at the limits of a generic's
range, without the rest of the compiler having to know that zero width is special.
