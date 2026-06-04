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
  { dom clk: Clock }
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

- named parameters are good for inferable or configuration-style parameters such as `dom clk`
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
  { dom clk: Clock }
  ( A: Type )
  = df {
    in ready: bool @clk,
    out valid: bool @clk,
    out data: A @clk,
  }
```

## Domains are part of the type

A value's clock domain is part of its type, not a separate annotation: `uint(8) @clk` is a distinct type from `uint(8)`, the same way `OnDom clk (uint 8)` differs from `uint 8` in Haskell. `OnDom clk` behaves like an applicative: pure values (compile- or run-time constants) are lifted into a domain automatically, with no explicit lifting operator in the surface language. That auto-lift is exactly the `const`-subtyping rule — `T @const` is usable wherever `T @clk` is expected. See `planning/domain_checking.md` for the scalar lattice.

Aggregates carry their domain in one of two forms; the dividing line is whether the declaration takes an explicit `dom` parameter.

1. **Domain elided — one domain on the whole structure.** This is the common case and the default for both structs and ports. The declaration takes no `dom` parameter and the fields carry no `@`-annotation; the single domain is attached to the *whole* structure and supplied at the use site (`Bus @clk`, `DF @clk`). Constructing the aggregate requires every field to be on that one domain, and reading a field yields a value on the aggregate's domain. Because the domain is just left polymorphic, such an aggregate can also be used *purely* — at `@const`, e.g. evaluated at compile time — when nothing pins it to a clock.

   ```rust
   struct Bus(A: Type) = bus {
     valid: bool,
     data: A,
   }
   // bus_val : Bus(uint(8)) @clk  ⟹  bus_val.data : uint(8) @clk

   // A port in the same elided form — fields carry direction but no @:
   port DF(A: Type) = df {
     in  ready: bool,
     out valid: bool,
     out data:  A,
   }
   // used as `DF @clk`
   ```

   ("Monodomain" is a slightly misleading name for this form: its one domain need not be a clock — it can be `@const`. The point is that there is a single domain, attached to the whole structure, not that it is necessarily clocked.)

2. **Explicit domains — driven by a `dom` parameter.** When a declaration takes one or more `dom` parameters, its fields are annotated with domains drawn from those parameters. The `DF` example under [Port semantics](#port-semantics) is this form: its `{ dom clk: Clock }` parameter is referenced by each field's `@clk`. This is what is needed when a port genuinely spans more than one domain (several `dom` params), and the per-field `@`-annotations are load-bearing. Here the domain lives on the parameters, not as a single top-level domain on the aggregate.

   A port that takes a *single* `dom` parameter and applies it uniformly to every field (exactly the `DF` example) is the degenerate explicit case: it expresses, verbosely, the same thing form 1 expresses by eliding the domain. The compiler should accept it but **emit a warning suggesting it be simplified to the elided form** — drop the `dom clk` parameter and the per-field `@clk`, recovering form 1.

## Parameters as fields

Fields may also be *parameters* — values fixed at elaboration time that travel with the structure. The motivating case is a credit line whose static starting credit count is carried within the port from module to module.

This is not a port-only feature. A struct's type parameter is already, implicitly, a value-level parameter field. Writing

```rust
struct Bus(A: Type) = bus { valid: bool, data: A }
```

is equivalent to giving `bus` an implicit leading `param A: Type` field:

```rust
struct Bus(A: Type) = bus { param A: Type, valid: bool, data: A }
```

so the value carries everything needed to recover its type — exactly like a Haskell constructor whose existential/phantom arguments are recoverable from the payload. Most of the time `A` is trivially derivable from the other fields and is elided. The same machinery — generic parameters threaded down to the value level (`ValueKind::Param` / `GenericArgs` in the HIR) — is what an explicit `param`-typed credit-count field would reuse.

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
