# Structs and ports

This document records the current design direction for structs and ports in Polar.

It focuses on:

- the shared declaration shape
- parameterization
- the distinction between type constructors and term-level constructors
- use-site syntax
- current deferred features

## Core principle

Structs and ports should share the same overall declaration structure.

This keeps the language regular:

- components use named and positional parameter sections
- parameterized structs use the same structure
- parameterized ports use the same structure

That common shape is preferred over a more Rust-like approach where structs and ports have a different parameterization form from components.

## Shared declaration shape

The intended declaration form is:

1. the declaration keyword and type constructor name
2. an optional named parameter section in braces
3. an optional positional parameter section in parentheses
4. `=`
5. a distinct constructor name
6. a braced body

In other words, the parameterized section is syntactically separated from the body.

## Distinct type and constructor names

Structs and ports should use different names for:

- the type constructor
- the term-level constructor

This is a deliberate design choice and is considered good practice.

Examples:

```rust
struct Bus(A: Type) = bus {
  valid: bool,
  data: A,
}
```

```rust
port DF
  { #clk: Clock }
  ( A: Type )
  = df {
    in ready: bool @clk,
    out valid: bool @clk,
    out data: A @clk,
  }
```

The semantic split is:

- `Bus` / `DF` are type-level constructors
- `bus` / `df` are term-level constructors

## Named and positional parameters

Structs and ports should support the same parameter split already used elsewhere in the language:

- named parameters in braces
- positional parameters in parentheses

Current intended roles:

- named parameters are good for inferable or configuration-style parameters such as `#clk`
- positional parameters are good for type-level parameters such as `A: Type`

That makes this layering natural:

1. choose named parameters
2. choose positional type parameters
3. refer to the resulting type
4. construct a value or port with the distinct constructor

## Struct semantics

Structs remain:

- positive data types
- aggregate values
- free of per-field direction annotations

A parameterized struct example:

```rust
struct Bus(A: Type) = bus {
  valid: bool,
  data: A,
}
```

Construction uses the term-level constructor:

```rust
bus {
  valid: true,
  data: payload,
}
```

## Port semantics

Ports use the same outer declaration structure as structs, but their fields carry direction.

Important port rules that still apply:

- data should stay in positive positions where sensible
- a write input is an explicit exception
- when a port is passed in argument position but is driven by the callee, that direction should be written explicitly

A parameterized port example:

```rust
port DF
  { #clk: Clock }
  ( A: Type )
  = df {
    in ready: bool @clk,
    out valid: bool @clk,
    out data: A @clk,
  }
```

## Use-site syntax

There is now a deliberate distinction between:

- type instantiation
- construction

### Type instantiation

Type instantiation should look like:

```rust
DF{clk}(A)
```

This applies named parameters first, then positional parameters.

### Construction

Construction should use the separate constructor name:

```rust
df {
  ready: ready,
  valid: valid,
  data: data,
}
```

So the intended mental model is:

1. instantiate the type constructor
2. use the term-level constructor to build a value or port

## Deferred feature

One missing feature is explicit constructor-side type application when inference is insufficient.

The motivating case is similar to Haskell constructor application, where a constructor may need explicit type arguments at the construction site.

For now, this should remain deferred.

The current design should **not** add extra syntax for explicit constructor-side type application yet.

## Current open questions

- exact surface syntax for constructing parameterized values when some parameters cannot be inferred
- how constructor-side explicit type application should look when it is eventually added
- how this split should appear in diagnostics and pretty-printing
