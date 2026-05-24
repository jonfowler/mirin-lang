We are going to aim to produce a source-to-Verilog version of the compiler on a small subset of the language.

To achieve this we are going to reduce some of the current scope, to focus on the core features.

## Primitive types

- `uint(N)` — fixed-width unsigned integer; N may be a literal or a `const` parameter. This is the primary data type.
- `usize` — type for compile-time width constants (used in `const` parameters such as `const bits: usize`). Parametric width is supported: `uint(bits)` where `bits: usize` is a valid `const` parameter.
- `bool` — single-bit boolean.
- `Clock` — clock domain type; only appears in named parameter sections.
- `Reset` — active-low reset, always domain-qualified (`Reset @clk`).

## User-defined types

- **Structs** — positive value types declared with `struct Name = constructor { field: T, ... }`. Both the type name and the constructor name resolve to the struct's `DefId`. Construct via the constructor identifier: `constructor { field: value, ... }`. Parametric structs (`struct Bus(A: Type)`) remain out of scope.
- **Ports** — compound types declared with `port Name { #clk: Clock } = constructor { in/out field: T @clk, ... }`. Fields carry an `in` or `out` direction. Ports do not carry a top-level domain (HIR rejects `@` annotations on ports); clocking flows through the port's clock parameter into per-field types. Parametric ports (`port DF{clk}(A: Type)`) remain out of scope.

## Function parameter directions

- Positional parameters may carry an `in` or `out` keyword. The direction is preserved on `HirParam::direction`; later passes validate uses against it. The direction is only meaningful for port-typed parameters at present.

## Primitive operations

- `+` and `*` take two `uint(N)` of the same width and produce `uint(N)`.
- `.reg(rst, reset_val)` — register a value. Signature (compiler-provided):

```
fn reg{#clk}(self @clk, rst: Reset @clk, reset_val: uint(N)) -> uint(N) @clk
```

The `{#clk}` named parameter is inferable from the domain of `self` and may be omitted in calls.
Call syntax: `expr.reg(rstn, 0)`.

## What is out of scope for the first pass

- Slicing (`expr[hi:lo]`)
- Parametric type application: type arguments other than `uint(N)` with a const width, including `Stream8{clk}` / `Bus(uint(8))` style use sites.
- Parametric struct/port declarations (`struct Bus(A: Type)`, `port DF{clk}(A: Type)`).
- `impl` blocks and method dispatch beyond `.reg`. Path expressions (`Type::method()`) and struct field access (`record.field`) are not yet lowered.
- Any `.method()` calls other than `.reg(...)`.

Examples documenting intended but unsupported features live in `todo-examples/`.
