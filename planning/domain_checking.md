# Domain checking

Polar requires that two signals share a clock domain before they can be connected. This catches a large class of hardware design errors — accidentally feeding a signal from one clock into logic that runs on another — at the type level rather than at simulation or synthesis time.

Domain checking also gives clock-associated signals access to their clock for register construction: a value of type `T @clk` can be passed to `.reg(...)`, which uses the embedded `@clk` to clock the resulting register.

## Scope

For the first pass, "domain" means "clock domain". The current assumption is that registers update on the rising edge of their clock. The system should leave room for future clocking styles (negative edge, dual edge, frequency multiples), but those are out of scope here.

Domain information is part of a value's type — `uint(8) @clk` is a distinct type from `uint(8)`. In the implementation it is convenient to treat domain inference as a pass that runs alongside the rest of type checking but with its own constraint set, since domains form a simple lattice rather than participating in the full structural type-equality machinery.

## The `const` domain

Every type carries a domain. During resolution, types written without an explicit `@…` annotation are given a fresh domain variable (standard Hindley–Milner style). Literals and other compile-time-constant values are given the special domain `const`.

`const` is a **supertype** of every concrete domain. A value of type `T @const` can be used anywhere a `T @<anyClock>` is expected, and no explicit cast is required. This is genuine subtyping, not unification with a free variable: a single constant may flow into two different clock domains in the same expression, and both uses must be accepted independently. Modelling `const` as a unifiable free variable would force it to pick one domain and reject the second use, which is wrong.

## Domain kinds: `Clock` vs `const`

The subtyping lattice above governs where domains sit relative to each other in expressions. A separate concern is *which* domains a given operation will accept at all. Polar handles this with a kind distinction: `Clock` is the kind inhabited by every concrete clock domain (`@clk`, `@clk1`, ...). `@const` does **not** inhabit `Clock`.

This is bounded polymorphism, in the System F-sub tradition. A signature like

```
fn reg_no_reset{dom clk: Clock}(self @clk) -> uint(N) @clk
```

requires its domain parameter to inhabit `Clock`, so a fully-`const` call such as `1.reg_no_reset()` is rejected at the type level. `@const` is still a supertype in the value lattice — it just doesn't satisfy the `Clock` kind. Clash uses the same scheme via its `KnownDomain dom` constraint.

For the first-pass `.reg(rst, reset_val)` the kind constraint is implicit: `rst: Reset @clk` already forces `clk` to a concrete clock domain. The kind distinction only becomes load-bearing for register-like operations that would otherwise have no clock anchor.

## Inference

When two signals are connected, their domains must unify (or one must be `const`, in which case it is accepted as a subtype of the other).

Inference from siblings:

```rust
var a: uint(8) @clk;
var b: uint(8) @clk;
var c = a + b;
// c is inferred to be uint(8) @clk
```

Inference anchored by a reset:

```rust
var a: uint(8) @clk;
var b = 1.reg(rstn, 0);
var c = a + b;
// b is inferred to be uint(8) @clk
```

In the second example, the literal `1` has type `uint(8) @const`. The `.reg` signature is

```
fn reg{dom clk}(self @clk, rst: Reset @clk, reset_val: uint(N)) -> uint(N) @clk
```

so the call's `@clk` is anchored by `rstn: Reset @clk`. The `self` argument (`1`) is `@const`, which is compatible with `@clk` via the subtyping rule. The result is `uint(8) @clk`, which then unifies with `a`'s domain when computing `c`.

Inference involving a constant on both sides:

```rust
var a: uint(8) @clk;
var b: uint(8) = 1;
var c = a + b;
```

`b` is `uint(8) @const`. The `+` operator requires both operands to share a domain; `const` is below every concrete domain in the lattice, so this is accepted and `c` is `uint(8) @clk`.

### Defaulting

Pure-constant subexpressions type as `@const` directly: `3 + 4` is `@const + @const → @const`, with no inference variable involved. A polymorphic domain variable only arises when a constant flows into a context whose domain isn't yet pinned — and the non-const side of any later use pins it.

If a domain variable does survive inference with no concrete use forcing it lower, it compacts to its upper bound (`@const`). This follows the MLsub / algebraic-subtyping treatment of inference variables in a subtyping lattice: a variable with no lower-bound constraint simplifies to top. The effect is that obviously-constant expressions stay `@const` without needing an ad-hoc defaulting rule, while operations that demand a real clock (see [Domain kinds](#domain-kinds-clock-vs-const)) still reject `@const` at the kind level.

## Mismatch errors

Domains that are concrete and distinct fail to unify:

```rust
var a: uint(8) @clk1;
var b: uint(8) @clk2;
var c = a + b;  // error: domain mismatch between @clk1 and @clk2
```

The error is reported at the connection site (here, the `+`).

## Open questions

### Cyclic `var` equations

Inferring the domain of a `var` whose equation references itself (e.g. `var count; count = (count + 1).reg(rstn, 0);`) generally needs a fixpoint pass. In practice the domain can almost always be resolved from an external anchor — most commonly the reset passed to `.reg` — without solving the cycle.

## Future work

- Support for additional clocking styles: negative edge, dual edge, mixed-edge designs. These extend the lattice with extra structure, so the unification rules need revisiting.
- Frequency-related clock relationships. A value valid on a slower clock is also valid to sample on any faster clock that is a multiple of it, so the slower clock can sit above the faster one in the domain lattice — analogous to how `const` sits above every concrete clock today.
