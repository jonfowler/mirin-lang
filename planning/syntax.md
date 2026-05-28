# First-pass syntax subset

This document defines the small Polar surface syntax subset that current examples, tooling, and parser work should target.

## Scope

- `fn` declarations
- `struct` declarations
- `port` declarations
- named argument sections
- positional argument sections
- clocked types with `@clk`
- clock-associated resets with `Reset @clk`
- `let` bindings (sequential, forward-only) and `var` signal declarations (block-scoped, supports cyclic equations)
- component connection blocks: `=` for sinks (inputs), `=>` for sources (outputs, introduces a `var`-scoped name)
- method-style calls such as `value.reg{...}()`

## Conventions

- Use Rust-like spacing for bindings and fields: `name: Type`
- Use trailing commas inside braced lists and record literals
- Keep named interface arguments in braces and ordinary value arguments in parentheses
- Use the `param` keyword for compile-time parameters and `dom` for clock-domain bindings; both place the name into the type environment as well as the value environment
- A named `param`/`dom` without a default is inferred from the call site; with a default, the default is the single fallback (no inference). Positional `param`/`dom` must always be supplied explicitly.

## Domains

- For now, only clock domains are in scope as part of the type system
- Write clocked values as `T @clk`
- Write resets as `Reset @clk`
- Treat generalized metadata as deferred design work, not part of the initial syntax subset

## Components

Components use:

- an optional named argument section
- an optional positional argument section
- an optional return type
- a block body

```rust
fn multAdd
  { dom clk: Clock, rstn: Reset @clk = high, c: uint[8] @clk = 0, }
  ( a: uint[8] @clk, b: uint[8] @clk )
  -> uint[8] @clk
  {
    let mult = a * b;
    let mult = mult[8:0];
    let mult = mult.reg{rstn}();
    let add = mult + c;
    return add;
  }
```

## Structs

Structs are positive data types and use Rust-like field syntax.

The more detailed current design for parameterized structs and ports is documented in `planning/structs_and_ports.md`.

```rust
struct Packet {
  valid: bool,
  payload: uint[8],
}
```

## Ports

Ports can carry directions on individual fields and may themselves take named parameters.

```rust
port Stream8
  { dom clk: Clock }
  {
    out valid: bool @clk,
    out data: uint[8] @clk,
    in ready: bool @clk,
  }
```

### Port position rule

- Keep data in positive positions where sensible
- A write input is an explicit exception
- If a port appears in an argument position but is being driven by the callee, mark that argument direction explicitly

```rust
fn connectStream
  { dom clk: Clock }
  ( upstream: Stream8{clk}, out downstream: Stream8{clk} )
  {
    downstream.valid = upstream.valid;
    downstream.data = upstream.data;
    upstream.ready = downstream.ready;
  }
```

## Bindings and cycles

Polar uses two distinct binding forms:

- `let x = expr` — sequential lexical binding. Forward-only scope. Supports shadowing for pipeline-style code.
- `var x: T` — block-scoped signal declaration. Participates in cyclic equations for register feedback and mutual structural wiring.

State feedback example:

```rust
var count: uint[8] @clk;
count = (count + 1).reg{rstn}(0);
```

See `planning/cycles_and_scoping.md` for full scoping rules.

## Component connection blocks

When instantiating a component, fields are connected using a braced argument block. The operator encodes direction:

- `field = expr` — sink: the component reads from this expression (input)
- `field => name` — source: the component drives this signal; `name` is introduced as a block-scoped signal if not already declared

```rust
reg_df {
  in_dat  = x * 4,
  output => out_df,
}();
```

The `in`/`out` direction keywords are optional and checked for consistency when present. See `planning/port_connections.md` for the full connection syntax.

## `impl` blocks

`impl` syntax is part of the current surface-language direction, but it is not required in the first parser slice. The current proposal is documented in `planning/impl.md`.

## Open questions kept out of the first parser slice

- exact inference rules for `dom clk`
- generics and const generics beyond simple examples
- generalized metadata syntax
- clock inference for cyclic `var` equations: inferring the clock domain of a `var` from its own equation requires a fixpoint pass, not a simple forward walk. See `planning/known_issues.md`.
- `inline fn` as a modifier and keyword reservation.
