# Inference

`infer(def)` walks a definition's body and works out a type and a clock domain for
every expression, resolves each method call, and checks the constraints it can.
What it cannot yet decide, it records as an obligation and settles later. This
chapter covers the inference engine:
the variable table, the eager-unify-then-defer strategy, and domain checking.
Dispatch and trait solving lean on the same machinery and get their own
[chapter](traits.md); grounding a symbolic width is [constant evaluation](const-eval.md).

## The inference table

Inference runs over a table of **inference variables** — placeholders for types,
consts, and domains not yet known. They share one index space (chalk's shape, from
[the type system](type-system.md)); the table records each variable's kind when it
mints it. A union-find structure merges variables as they unify, and resolution
chases a variable to whatever term it has been bound to. Two folders walk terms
through this table: a substituter, which replaces a definition's positional
generic parameters with concrete arguments, and a resolver, which replaces
inference variables with their bound terms.

When inference finishes, a variable that nothing constrained takes a default: an
unconstrained type becomes `Error`, a const becomes `Deferred`, and a domain
becomes `@const` — the top of the domain lattice, so an unconstrained value is
treated as compile-time-constant rather than tied to a clock.

## Unify eagerly, defer the rest

Inference unifies structurally and immediately. Two types unify by their heads —
a `uint` width against a `uint` width, a `Port` against the same `Port`'s
arguments, a `Vec` element-wise — and a mismatch at any leaf is a diagnostic on
the spot. Ground facts are decided here: two different literal widths, `uint(8)`
against `uint(16)`, fail at once.

What inference cannot decide structurally becomes an **obligation**, queued and
discharged at a fixpoint once the whole body has been walked. There are four:

- **a width equality** — two const expressions that must be equal, but contain a
  symbolic part (a generic parameter, body-local, or arithmetic);
- **a literal fit** — a numeric literal must fit the width of its type;
- **a trait bound** — a callee's `where` clause, instantiated at the call site
  (solved as the [traits](traits.md) chapter describes);
- **a const domain** — a body-local used in a width must be `@const`, not clocked.

At the fixpoint, [constant evaluation](const-eval.md) grounds the const
expressions it can, and the width and fit obligations are decided on the results.
Any that are still symbolic — because a width depends on a generic the call site
hasn't pinned — survive as **residuals**, which the backend emits as an
`initial assert`. This is the optimistic checking the [Overview](../architecture/overview.md)
named: inference never has to do const arithmetic to produce a type, and a
constraint it cannot settle here is carried forward rather than rejected.

## Domains: one subtyping edge

A domain is part of a value's type, and the domain lattice has a single edge:
`@const`, the compile-time constant, sits below every concrete clock. Inference
applies that edge through three operations, each at its own kind of site:

- **Unify** is strict and used where two domains must be *equal* — the operands of
  an equation, a field against its driver. Two concrete clocks must be identical;
  `@const` against a clock is a mismatch here, not a coercion.
- **Subsume** is the coercion, used where a value flows into an expected domain — a
  call argument, a `let` ascription, a `return`. `@const` subsumes into any
  domain; otherwise it falls back to unify. This is the only place the lattice
  edge is crossed.
- **Join** merges the arms of a conditional and the operands of an operator into a
  fresh domain, so a `@const` arm does not pin the whole result to `@const`.

The register builtin shows these rules in one signature. `x.reg(rstn, init)`
takes a single clock-sorted domain `D` covering *both* the data and the reset, an
`init` that must be `@const`, and produces a result on `D` — so a register can
never silently mix the clock of its data with the clock of its reset.

A pure function — one with no clock annotations — acquires its domains by
*lifting*: inference appends a synthetic domain parameter and stamps it onto every
unannotated slot, so each call instantiates the function in the caller's domain.
Lifting is provisional — a surface-level transform the front end is lenient about
and the backend ultimately enforces.

The result of `infer` is a side-table — a type and domain for every expression,
every method call resolved, and a handful of residual obligations the front end
could not close. Those residuals, and the dispatch decisions, are what the MIR and
the backend build on. How a method call is resolved is the next chapter.
