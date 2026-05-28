# `impl` design: first pass

This document proposes a first `impl` design for Polar.

The goal is to get the ergonomics of methods and associated functions without introducing object-oriented runtime behavior. `impl` is surface syntax and should lower to ordinary declarations during elaboration.

## Goals

- feel familiar to Rust users
- allow methods on nominal types such as `struct` and `port`
- preserve explicit typing and clock information
- keep lowering straightforward

## Proposed shape

Use Rust-like `impl TypeName { ... }` blocks.

For parameterized types, reuse the type's parameter section on the `impl` block.

```rust
impl Packet {
  fn idle() -> Packet { ... }
  fn clear_valid(self: Packet) -> Packet { ... }
}

impl Stream8
  { dom clk: Clock }
  {
    fn connect(self: Stream8{clk}, out downstream: Stream8{clk}) { ... }
  }
```

## Design choices

### 1. `fn` inside `impl`

Use `fn` for methods and associated functions.

Reason:

- it matches Rust expectations
- it makes member declarations visually distinct from top-level hardware components
- methods can still lower to ordinary functions, intrinsics, or helper components later

This does not require runtime dispatch or object identity.

## 2. Explicit `self` typing

Treat `self` as a normal first parameter with a reserved name.

Use explicit types:

- `self: Packet`
- `self: Packet @clk`
- `self: Stream8{clk}`

This keeps domain information and parameterization visible in the signature instead of hiding it in method lookup rules.

## 3. Associated functions and method calls

Use:

- `Packet::idle()` for associated functions
- `packet.clear_valid()` for methods
- `stream.connect(out other)` for port wiring helpers

During lowering:

- associated functions become namespaced ordinary functions
- methods become ordinary functions with `self` as the first argument

## 4. Ports may have methods

Ports are first-class interface types, so they should be allowed to carry helper methods in `impl` blocks.

This is useful for:

- wiring helpers
- handshake helpers
- common direction-safe adapters

Direction still matters. If a method drives another port argument, that argument should remain explicitly annotated.

## 5. Clocked methods stay explicit

Do not hide clock information inside `impl`.

Prefer:

```rust
fn register(self: Packet @clk, rstn: Reset @clk) -> Packet @clk
```

over any form where the clock is inferred from the `impl` block itself.

This keeps clock semantics local to the signature and consistent with the rest of the language.

## Struct example

```rust
struct Packet {
  valid: bool,
  payload: uint[8],
}

impl Packet {
  fn idle() -> Packet {
    return Packet {
      valid: false,
      payload: 0,
    };
  }

  fn with_payload(payload: uint[8]) -> Packet {
    return Packet {
      valid: true,
      payload,
    };
  }

  fn clear_valid(self: Packet) -> Packet {
    return Packet {
      valid: false,
      payload: self.payload,
    };
  }

  fn register(self: Packet @clk, rstn: Reset @clk) -> Packet @clk {
    return self.reg{
      rstn,
      reset_val = Packet::idle(),
    }();
  }
}
```

## Port example

```rust
port Stream8
  { dom clk: Clock }
  {
    out valid: bool @clk,
    out data: uint[8] @clk,
    in ready: bool @clk,
  }

impl Stream8
  { dom clk: Clock }
  {
    fn connect(self: Stream8{clk}, out downstream: Stream8{clk}) {
      downstream.valid = self.valid;
      downstream.data = self.data;
      self.ready = downstream.ready;
    }
  }
```

## Lowering model

The recommended lowering model is:

- parse `impl` blocks into AST nodes
- during elaboration, lift methods into namespaced declarations
- desugar `value.method(args...)` into `TypeName::method(value, args...)`

This gives the surface language method ergonomics without complicating later IRs.

## Deferred features

- traits or interfaces for shared behavior
- method overloading
- specialization by generic constraints
- visibility rules beyond basic namespacing

## Open questions

- Can `var` be used inside an `impl` method body? Methods lower to namespaced functions with `self` as an explicit argument. In the lowered form there is no enclosing component body that gives a `var` signal its RTL lifetime. The safest initial rule is to disallow `var` in `impl` method bodies: stateful logic must live in a component. This can be revisited if a coherent semantics for method-local signal nodes is later designed.
- The `fn register{dom clk}(...)` example in `impl_examples.plr` uses a named-parameter section on a method. This is syntactically identical to a top-level component declaration. While it is distinguishable by context (inside `impl` body vs top-level), the elaboration pass should emit a clear error if a function that looks like a component appears outside an `impl` block or a top-level `fn` with `self` appears at the top level.
