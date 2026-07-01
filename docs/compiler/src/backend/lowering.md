# Lowering to SystemVerilog

The backend turns each definition into one SystemVerilog module, lowering from the
MIR. This chapter covers that per-definition lowering: where the backend reads
from, how a definition's parameters and results become ports, how the body
becomes module items, and how symbolic widths are ground to numbers. Specialising
a *generic* definition across its call sites is the next chapter; keeping the
emitted SV legal at zero width is the one after.

## From the MIR, mostly

`sv_module(def)` lowers from the [MIR](../mir/mir.md): the MIR carries the
resolved types, the settled dispatch, and the drive-target places, so the backend
walks it without reaching back into the inference side-table. It still consults a few other queries:

- the **signature** gives the port shapes and the generic parameters;
- the **HIR body** (and its inference) are read for inline code — an
  inline-verilog template emitted verbatim, or an `#[inline]` callee being
  spliced;
- **inference** is also read for the width residuals it could not settle (below).

## Ports and flattening

A module's ports come from the definition's value parameters and its result
places. Aggregate types do not survive to the port list: the backend **flattens**
them to per-field **leaves** — the scalar signals a field ultimately reduces to. A
struct or port becomes one scalar per field (named with a `__` suffix), a `Vec`
becomes an unpacked array, a tuple becomes its indices. Each leaf's direction is the fold of the parameter's direction with the
field's: an `out` parameter carrying an `in` port field drives *into* the
instance, and the leaf is an input. A `dom` generic becomes a clock input port.

A definition that is compile-time only — an `integer`-returning or all-`integer`
function — produces no module at all.

## The body

With the ports declared, the backend walks the MIR block to module items. It walks
most shapes directly; two it recognises specially:

- **Registers.** `x.reg(rstn, init)` lowers to an `always_ff` with a synchronous,
  active-low reset to `init` — the one place sequential logic enters the emitted
  Verilog.
- **Inline-verilog bodies.** A `= verilog { … }` function emits its template text
  verbatim, with the call's generic names and const expressions spliced in.

## Grounding widths

A type carries its width as a const expression, not a number, so before the
backend can render `logic [W-1:0]` it grounds the width: it evaluates every
`ConstArg` through [constant evaluation](../typed-hir/const-eval.md), turning
`uint(n + 5)` into `uint(8)` once `n` is known. A width that stays symbolic — a
generic the call site left open — renders as a SystemVerilog parameter expression
for the elaborator to resolve.

The width and literal-fit constraints inference deferred (the optimistic checking
of the [Overview](../architecture/overview.md)) come home here. Where one is
ground, the backend has already decided it. Where it reduces to bare parameters,
the backend emits an `initial assert` — `assert (n == m)`, `assert (255 < (1 << n))`
— so the elaborator checks it even though the compiler could not. The ground
instances of those constraints are decided ahead of time too, by the
monomorphisation checks the next chapter describes.

One definition becomes one module — unless it is generic, in which case it becomes
a *family* of modules, one per instantiation. That is monomorphisation, next.
