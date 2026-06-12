# Inline verilog

Status: implemented. Decisions settled 2026-06 (this doc is the record; the
examples are the acceptance tests).

## Shape: fn bodies only

```mirin
fn ff_en {dom clk: Clock} (en: bool @clk, d: uint(8) @clk, out q: uint(8) @clk) = verilog {
    always_ff @(posedge ${clk}) if (${en}) ${q} <= ${d};
}
```

A fn whose body is `= verilog { … }`. The **signature is the contract**:
types, directions, and domains are declared there, so inference, domain
checking, direction checking, and call-site instantiation are exactly those
of an ordinary fn — no new checking semantics. The block's *implementation*
is trusted (the compiler cannot verify the verilog drives what the signature
claims); the verilator `-Wall` corpus lint is the honesty backstop —
an undriven output or width mismatch in the splice fails it.

Mixed Mirin/verilog within one body is deliberately out of scope: driver
accounting inside opaque text has no good answer. Factor the verilog part
into its own fn.

This is Rust's `asm!`-with-operands, HDL-shaped; as a wrapper mechanism it
is Chisel's BlackBox, except the body is carried inline and emitted as an
ordinary module.

## Interpolation: braces only

`${name}` / `${expr}`. A bare `$` passes through verbatim — `$clog2`,
`$signed`, `$display` need no escaping (the rejected alternative, bare
`$name` splices, collides with exactly those).

- `${p}` where `p` is a **scalar value param** (or `result`) → the port's
  emitted SV name. Splices exist because emitted names aren't always source
  names (reserved-word renames; flattening). Aggregate params may be
  *declared* (their flattened ports exist) but not *spliced* — error,
  lifted when a field-projection splice (`${s.valid}`) is worth designing.
- `${clk}` where `clk` is a **dom generic** → the clock port's name.
- anything else → a **const expression** over integer literals and
  Const-kind generics (`${n + 1}`), rendered as an SV constant expression
  (`(n + 1)` against the module's SV parameter). No const *eval* needed —
  symbolic renders are legal SV.
- Unknown names, field/local references, and malformed `${…` are
  diagnostics with spans inside the raw block.

## Lexing

The raw block is one token from the grammar's first **external scanner**:
brace-counting from the opening `{` (verilog concatenations are balanced),
skipping `"strings"`, `// line` and `/* block */` comments so braces inside
them don't miscount. `${…}` is *not* parsed by tree-sitter — the compiler
splits the raw text during body lowering, which keeps the scanner trivial.

## Pipeline

- `body(def)`: a verilog fn gets an empty HIR block plus a
  `VerilogTemplate` (text/splice segments, resolved against the signature).
- `infer`: nothing to walk; signature rules (explicit-mode domains etc.)
  still apply via `sig_of`.
- `check_drivers`/`completeness`: a verilog body is trusted to drive its
  out params.
- backend: ports from the signature as usual; the body is one
  `SvItem::Verbatim` with splices substituted. Call sites instantiate
  normally.
- `mirin-fmt`: verilog-bodied fns pass through verbatim.

## As built

- Splices resolve in order: `result`, dom generics (they're also seeded as
  body locals for `posedge`, but in a template they mean the clock port),
  scalar value params, then the const fragment.
- Const splices render *symbolically* (`${n + 1}` → `(n + 1)` against the
  module's SV parameter) — no evaluation needed.
- ~~Known gap: `SvInstance` carried no parameter bindings.~~ Closed:
  instances bind Const-kind generics from the call's recorded
  instantiation (`#(.n(8))`; symbolic values pass through against the
  caller's own SV parameters).
