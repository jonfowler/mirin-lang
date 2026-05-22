We are going to aim to produce a source-to-Verilog version of the compiler on a small subset of the language.

To achieve this we are going to reduce some of the current scope, to focus on the core features.

## Primitive types

- `uint(N)` — fixed-width unsigned integer; N may be a literal or a `const` parameter. This is the primary data type.
- `usize` — type for compile-time width constants (used in `const` parameters such as `const bits: usize`). Parametric width is supported: `uint(bits)` where `bits: usize` is a valid `const` parameter.
- `bool` — single-bit boolean.
- `Clock` — clock domain type; only appears in named parameter sections.
- `Reset` — active-low reset, always domain-qualified (`Reset @clk`).

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
- Type arguments / parameterized types other than `uint(N)` with a const width
- Struct, port, and impl items beyond the built-in `Reg` primitive
- Any `.method()` calls other than `.reg(...)`
