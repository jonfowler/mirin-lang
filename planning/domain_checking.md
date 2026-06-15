# Domain checking

Mirin makes a value's **clock domain** part of its type: `uint(8) @clk` is a
distinct type from `uint(8)`. Connecting two signals requires them to share a
domain, so feeding logic on one clock from another clock is a *type* error,
caught at compile time rather than in simulation or synthesis. The embedded
domain also gives clocked values their clock: a `T @clk` can be passed to
`.reg(…)`, which clocks the resulting register off `@clk`.

Scope: "domain" means "clock domain", registers update on the rising edge.
The design leaves room for richer domains (reset-carrying, delay-indexed) —
see Future work — but only `@const` and clock atoms exist today.

The closest prior art is Rust's lifetime/region system: domains ≈ regions,
`@const` ≈ `'static`, `T @ D` ≈ the outlives bound `T: 'a`. Where Mirin
diverges from Rust it says so and why.

## `@` is a constraint, not a field

`Ty @ D` is the constraint **"every clock-domain slot in `Ty` is `D`"**, not a
domain stapled onto every type. It is discharged by the head of `Ty`, and the
head is always known where `@` is written (`uint`, `Vec`, a tuple, a struct, a
port, or a type parameter), so most discharge happens at *lowering*:

- **Leaf** (`uint(8) @clk`, `bool @clk`): the domain lives **on the leaf**.
  This is the `&'a T`-carries-a-region analogue — and unlike Rust, where only
  references carry a region, in hardware *every* signal is intrinsically
  clocked, so a leaf is exactly where a domain belongs.
- **Nominal** (`Packet @clk`, a struct/port): the domain stamps the type's
  fields. A struct/port is homogeneous — one domain, applied to every field.
- **Aggregate** (`Vec(N, A) @clk`, `(A, B) @clk`): an aggregate has **no
  domain of its own**; `@clk` propagates into the elements' unspecified slots
  and is forgotten. A `Vec`/tuple is domain-bearing only through its elements.
- **Opaque** (`T @clk`, `T` a type parameter): cannot be decomposed yet → a
  **deferred obligation**, discharged after substitution when `T` is concrete
  (Rust's `Component::Param` path).

Because a domain lives only on leaves (and on a struct/port as its single
stamping domain), an aggregate's domain *is* its elements' — there is no
separate stored fact to disagree with them, so a clock-domain crossing cannot
be laundered through a `Vec`/tuple wrapper, and drift is unrepresentable. An
aggregate `@D` that meets an element's *own* explicit clock ≠ `D`
(`Vec(2, uint(8) @b) @a`, `(uint(8) @a, uint(8) @b) @c`) is a
`ConflictingDomain` error — `@` may only *fill* unspecified slots, never
override one; `@const` stays compatible.

## Sorts: `Clock` vs `Domain`

A separate concern from *where* domains sit is *which* an operation accepts.
`Domain` is the full sort including `@const`; `Clock` is the sub-sort of
edge-bearing domains; **`@const` does not inhabit `Clock`** (bounded
polymorphism, System F-sub; cf. Clash's `KnownDomain`).

```mirin
fn flipflop{dom D: Clock}(x: T @ D) -> T @ D   // registers need a real edge
fn add{dom D: Domain}(…)                        // pure logic works at @const too
```

So `1.flipflop()` is rejected (a fully-`const` call has no clock), while
`add(2, 3)` stays `@const`. Quantifying pure logic over `Domain` (not `Clock`)
is what keeps constant folding: const arguments don't get coerced up to a
clock. For `.reg(rst, …)` the kind constraint is implicit — `rst: Reset @clk`
already pins a concrete clock.

## Subtyping: the `@const` lattice

The lattice has exactly one edge: **`@const <: @D` for every `D`** (≈
`T: 'static` trivially satisfying any outlives bound). A const value enters any
domain for free; the coercion is silent because it is always sound and never
informative. Consequences:

- `add(x, 5)` with `x` on `clk` works — the literal is `@const` and coerces.
- A struct/tuple mixing const and clocked components needs no special case —
  `@const` slots satisfy any `T @ D`.
- A genuinely multi-domain type fails `T @ D` (it cannot be single-stamped)
  but still passes anywhere a bare `T: Type` is expected.

Coercions are inserted at the usual sites (argument positions, ascribed `let`,
`return`), with bidirectional checking. Every new lattice edge degrades domain
inference from unification to ≤-constraints, so `@const` stays the *only* edge;
richer "forgetting" is an explicit operation, not a subtyping rule.

## Lifting pure signatures

A signature that mentions no domains is **pure** and is lifted onto a single
shared domain parameter, applied to every value param and the result:

```mirin
fn add(x: uint(8), y: uint(8)) -> uint(8)
// lifts to
fn add{dom __Dom: Domain}(x: uint(8) @__Dom, y: uint(8) @__Dom) -> uint(8) @__Dom
```

This diverges from Rust's per-argument lifetime elision deliberately: distinct
clocks never unify and cross-domain ops are exactly the CDC error the system
exists to catch, so a per-argument lift would be uninhabitable — one shared
variable is the only signature that typechecks. Drop to explicit
`{dom A, dom B}` when the *relationship* between domains is the point
(synchronisers, dual-clock FIFOs). For a polymorphic pure type the lift imposes
the constraint form (`where T @ __Dom`) rather than applying `__Dom` to an
opaque parameter.

The lift is over sort `Domain`, not `Clock`, so `add(2, 3)` stays `@const`.

## Elision and defaults

A type written with no domain:

- **In a body** (`let x: uint(8) = …`): a fresh domain variable, solved from
  the RHS; if still unconstrained at the end of the binding (e.g. the RHS is a
  literal), it **defaults to `@const`**. So constants come out const and
  clocked expressions come out clocked without annotation. (MLsub-style: a
  variable with no lower bound simplifies to the top, `@const`.)
- **In a pure signature**: lifted to the shared `__Dom` (above).

Forcing bare types to mean `@const` everywhere was rejected — it would reject
`let x: uint(8) = a + b` with `a, b` on `clk`, and contradict signature
lifting.

## Representation (as built)

Types, widths, and domains are one **kinded term language** with a single
inference-variable space (`Term::{Type, Const, Domain}`, one `InferVar`); a
generic argument list is a `Vec<Term>`, so a domain is just one arg *kind*
alongside type and const — the same "one arg list, three kinds" shape as
rustc's `GenericArg` and chalk's `Substitution`. `Domain` is itself a small
term language (`Const | Param(i) | Clock(local) | Infer | Unspecified`) so that
reset-carrying and delay-indexed domains are later *constructors*, not a solver
rewrite.

`Vec` and `Tuple` are top-level `Type` variants with **no domain field**;
leaves (and structs/ports) carry their domain. `@D` propagation into aggregate
elements happens at lowering (`stamp_domain`); the conflict check is syntactic
over signature types.

## Future work

- **Domain as an arg for structs/ports**, like `Bus{dom D}` — replacing the
  stored stamping-domain. Unifies nominal types with the leaf/aggregate model
  and unlocks heterogeneous (per-field-domain) structs. The `Term::Domain`/arg
  machinery already exists.
- **Richer domains**, three separate mechanisms: the *lattice* stays
  `@const`-only (forgetting a reset/delay is an explicit op, never an edge);
  *sorts* gain `ClockReset ⊑ Clock ⊑ Domain` (subsumption at instantiation);
  *indices* add `Delayed(C, N)` as a domain constructor doing index arithmetic
  (no subtyping between delays — `Delayed(c,3) ≠ Delayed(c,4)` is the timing
  error the feature catches; the clock is reached by projection, not
  subsumption). Cf. Clash's `DSignal dom n a`.
- **Dependency-aware lifting**: an output independent of every clocked input
  could keep `@const` instead of joining `__Dom` (lets a pure function return
  `enumerate`'s `(integer @const, A)` result directly).
- **The `@_` "clocked, inferred" form + a lint** that a binding whose inferred
  domain is a real clock must carry at least `@_`, severity scaling with the
  number of domains in scope.
- **Gaps**: `@const` as a *written* annotation does not yet lower to
  `Domain::Const` (falls to `Unspecified`, lenient); the conflict check scans
  signature types only, not body ascriptions.
