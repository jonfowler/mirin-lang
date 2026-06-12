# Domain Checking Revisited

Aims:
- Clarify the semantics of domains within a type.
- View `@` as syntactic sugar for a more explicit domain annotation.
- `param/@const` clarification
- Subtyping rules
- Clear up all syntax
- Consider tuple type and its implications
- Consider applying domain to polymorphic type

Design notes: the closest prior art is Rust's lifetime/region system (domains ≈ regions,
`@const` ≈ `'static`, `T @ D` ≈ the outlives bound `T: 'a`) and Clash's domain/`DSignal`
machinery for the richer domains in the future-work section. Where this doc diverges from
Rust it says so and why.

## The general picture

The immediate idea is to incorporate domains into almost all types so they have to be provided
to construct that type. For instance:

```polar
// Note `type` is hypothetical syntax for a type declaration which isn't a struct
type uint{dom D : Clock}(N: integer) -> Type

type bool{dom D : Clock} -> Type
```

A struct also has a similar structure:

```polar
struct Bus{dom D : Clock}(N: integer) -> Type = bus {
   valid: bool{D},
   data: uint{D}(N)
}
```

So almost all types have a domain parameter. The main exception are domains themselves, which
do have a domain parameter but it's not required to construct them:

```polar
type Clock{} -> Type
```

## The problem: polymorphic types and tuples

Polymorphic types are the hard case. We want to write:

```polar
type Write{dom D: Clock}(T: Type) -> Type = write {
   valid: bool{D},
   data: T{D}
}
```

But now `T` is applied to a domain, so its declared kind must really be a *function* from
domains to types:

```polar
type Write{dom D: Clock}(T: Fn{dom D: Clock} -> Type) -> Type = ...
```

Call that kind `DomType`. Tuples make the tension concrete: we want both mixed-domain tuples
(`Tuple(uint{D1}(8), uint{D2}(8))`, which needs `T: Type` arguments) and single-domain tuples
(`Tuple(8-bit, bool) @clk`, which under this scheme needs `T: DomType` arguments) — i.e. two
definitions of `Tuple` differing only in kind.

**This route is rejected.** `DomType` parameters are higher-kinded abstraction over domains —
the exact analogue of abstracting over lifetimes (`for<'a> ... -> Type`), which Rust
deliberately never added. The problems are the canonical ones:

- *Eta problems*: what if the supplied constructor isn't literally of the form
  `Fn{dom D: Clock} -> Type` — different parameter name, extra parameters, a partially
  applied constructor?
- *Definition duplication*: every container needs a `Type` version and a `DomType` version
  (the two `Tuple`s above), and an emitted "`DomType` shadow version" of every user type.

Rust's answer, which we adopt: **type parameters stay fully applied; the relationship between
an opaque type and a domain is a *constraint*, not an *application*.** Rust never writes
`T<'a>` for an opaque `T` — a type parameter arrives with its lifetimes already inside it,
and `T: 'a` is a bound, checked structurally for ground types and deferred as an obligation
for opaque ones.

## The resolution: `@` is a constraint

There are two distinct forms, and only one of them is application:

1. **Application `{D}`** — only for constructors with declared domain parameters:
   `uint{clk}(8)`, `Bus{clk}(8)`. This is the explicit, fully elaborated form.
2. **Constraint `Ty @ D`** — "every clock-domain slot in `Ty` is `D`". One rule, two
   behaviors depending on whether the head of `Ty` is known:
   - *Head-known* (a constructor, possibly with applied arguments): discharge immediately by
     unification — fill every unapplied domain parameter with `D` and unify every unsolved
     domain metavariable under the type with `D`. So `uint(8) @clk` just elaborates to
     `uint{clk}(8)`; at annotation sites `@` really is syntactic sugar.
   - *Opaque head* (a type variable `T`): emit a deferred obligation `T @ D`, discharged at
     instantiation or against `where`-bounds in the environment (exactly rustc's handling of
     `T: 'a` param components). This fits the existing eager-unification-plus-deferred-
     obligations / OutsideIn-style setup.

No `DomType` kind, no shadow definitions, no name mangling. The polymorphic struct becomes:

```polar
// provisional `where` syntax for domain bounds
struct Write{dom D: Clock}(T: Type) where T @ D = write {
   valid: bool{D},
   data: T,          // not T{D} — T is already applied; the where-clause pins it
}
```

A caller passes a fully applied type: explicitly `Write{clk}(uint{clk}(8))`, or with
inference just `Write(uint(8)) @clk`, which solves both domain slots at once.

### Tuples, once

```polar
struct Tuple(T: Type, U: Type) -> Type = tuple {
   _1: T,
   _2: U,
}
```

Mixed domains fall out for free: `Tuple(uint{D1}(8), uint{D2}(8))` is fine because the
arguments are just types. The single-domain case is not a second definition but a use-site
constraint: `Tuple(uint(8), bool) @clk` elaborates the arguments with fresh domain
metavariables, then `@clk` propagates structurally through the fields and unification solves
them all to `clk`.

### What is lost, and the escape hatch

Constraints cannot express "instantiate `T` at a *different* domain than the caller used".
The one real customer is CDC primitives — "same shape, different domain":

```polar
fn sync{dom A: Clock, dom B: Clock}(T: Type)(x: T @ A) -> Retag(T, B)
```

`Retag(T, B)` (surface spelling TBD, perhaps `T @! B`) is a *built-in* type-level operator:
`T` with every clock-domain slot substituted by `B`. `Retag(T, B) @ B` holds by construction,
so it composes with the constraint system. This is the same move rustc makes internally
(region folding/substitution exists in the compiler without surfacing higher-kinded regions
to users). If first-class `DomType` is ever truly needed, the rustc-shaped path is the GAT
move: a second-class kind that only built-ins and explicit declarations inhabit, never
inferred or eta-converted from ordinary constructors.

## Lifting

Two ways of writing a type, as before:

1. **Implicit domains / pure types.** The definition contains no domain annotations:

   ```polar
   struct Bus(N: integer) -> Type = bus {
      valid: bool,
      data: uint(N)
   }
   ```

   This is lifted to a single shared domain parameter applied to every field:

   ```polar
   struct Bus{dom __Dom: Domain}(N: integer) -> Type = bus {
      valid: bool{__Dom},
      data: uint{__Dom}(N)
   }
   ```

   For a *polymorphic* pure struct, lifting cannot apply `__Dom` to an opaque parameter —
   instead it imposes the constraint:

   ```polar
   struct Pair(T: Type) = pair { a: T, b: T }
   // lifts to
   struct Pair{dom __Dom: Domain}(T: Type) where T @ __Dom = pair { a: T, b: T }
   ```

2. **Explicit domains.** The definition contains explicit domain structure. Triggered by any
   of:
   1. Using the `dom` keyword / introducing a domain
   2. Using `param`/`const` for a field (which can be viewed as speccing the field as
      `@const`) (Q: should we just use `@const`?)
   3. Using the `@` syntax

   In the explicit case, all fields must carry a domain annotation or a `const` declaration.
   This supports multiple domains and mixtures of clocked and `@const` fields. No lifting
   happens.

Lifting is purely surface sugar: the lifted form *is* the real signature — it is what
elaboration, diagnostics, and LSP hovers operate on and display, so users can learn what the
sugar means by inspection (same stance as Rust's lifetime elision).

### Lifting functions

A pure function lifts all arguments and the result onto **one shared domain variable**:

```polar
fn add(x: uint(8), y: uint(8)) -> uint(8)
// lifts to
fn add{dom D: Domain}(x: uint{D}(8), y: uint{D}(8)) -> uint{D}(8)
```

This deliberately diverges from Rust's elision (which gives each argument a *distinct* fresh
lifetime). Rust can afford per-argument freshness because lifetimes have subtyping
(`&'long T <: &'short T` — a shared `'a` is a lower bound, not an equation, and references
with unrelated lifetimes can still interact inside a body). Clock domains have neither
property: distinct clocks never unify, and the operations themselves are domain-sensitive —
`x + y` across domains is exactly the CDC error the system exists to catch. A per-argument
lifting of `add` would be uninhabitable; the shared variable is the only signature that
typechecks. As in Rust, you drop to explicit `{dom A, dom B}` precisely when the relationship
between domains *is* the point (synchronizers, dual-clock FIFOs).

Note the lifted variable has sort `Domain`, not `Clock` — see below.

## Sorts of domains

`Domain` is the full sort including `@const`; `Clock` is the sub-sort of edge-bearing
domains. `@const` does not inhabit `Clock`.

```polar
fn flipflop{dom D: Clock}(x: T @ D) -> T @ D     // registers genuinely need an edge
fn add{dom D: Domain}(...)                        // pure logic works at @const too
```

Quantifying lifted functions over `Domain` is what keeps constant folding: `add(2, 3)` stays
`uint(8) @const`. If lifting quantified over `Clock`, the const arguments would coerce up to
some clock and the result would silently stop being const. Meanwhile `flipflop` statically
refuses `D = @const` because const doesn't inhabit the sort — no ordering constraint
(`D > @const`) needed, and the error message falls out: "`flipflop` requires a clock domain,
but the inferred domain is `@const`".

(This is a deliberate divergence from Rust, where `'static` is an ordinary region that can
instantiate any `'a` — fine there because no operation *requires* non-static; registers do
require an edge.)

## Subtyping

The lattice is `@const` at the bottom and **nothing else**: `@const <: @D` for every `D`
(≈ `T: 'static` trivially satisfying any outlives bound). A const value is available in any
domain; the coercion is silent because it is always sound and never informative. Consequences:

- `add(x, 5)` with `x` on `clk` works: the literal is `@const` and coerces; `D` is inferred
  from `x`.
- A struct mixing `param` fields with clocked fields needs no special-casing: `@const`
  components trivially satisfy any `T @ D` obligation.
- Multi-domain types behave correctly by default: a dual-clock type fails `T @ D` (it cannot
  be single-stamped) but still passes anywhere a bare `T: Type` is expected.

Coercions are inserted at the usual sites (argument positions, ascribed `let`, `return`),
with bidirectional checking. Worked example:

```polar
let x: bool @clk = true.flipflop();
```

The expected type `bool @clk` flows backward through the call, instantiating flipflop's
`D = clk`; then the argument check `true : bool @const ≤ bool @clk` succeeds by the coercion.
The lift happens at the argument, *before* the instantiation is complete — flipflop is
instantiated at `clk`, never at `@const`.

Every new lattice edge degrades domain inference (pure unification becomes ≤-constraints
needing lub/glb reasoning). Treat any proposed edge beyond `@const` with suspicion; prefer
explicit coercion operations (see future work).

## Elision, defaults, and the visibility lint

Where a type is written with no domain (`let x: uint(8) = ..`):

- **In bodies**: the domain is a fresh inference variable, solved from the RHS. If still
  unconstrained at the end of the binding (e.g. RHS is a literal — `@const <: ?d` constrains
  nothing), it **defaults to `@const`**. So constants come out const and clocked expressions
  come out clocked, without annotations.
- **In `param`/const/top-level contexts**: bare types default to `@const` directly
  (≈ Rust RFC 1623: elided lifetimes in `static`/`const` items default to `'static`).

Forcing bare types to mean `@const` everywhere was considered and rejected: it would error on
`let x: uint(8) = a + b` with `a, b` on `clk` and push `@clk` onto every ascription, and it
would be inconsistent with function lifting, where a bare type means "shared inferred domain".

Readability is handled by the surface, not the semantics. Three levels of annotation:

```polar
let x: uint(8) = a + b          // domain fully elided, inferred
let x: uint(8) @_ = a + b       // "this is clocked, inferred which" — cf. Rust's '_
let x: uint(8) @clk = a + b     // fully explicit
```

plus a lint: *a binding whose inferred domain is a real clock (not `@const`) must carry at
least `@_`*. Severity scales with ambient domain count, which is manifest in signatures:

- exactly one clock domain in scope in the enclosing item → lint silent (naming it adds
  nothing);
- two or more domains in scope → warn/deny, and arguably `@_` stops being enough and the
  domain must be named.

This is stricter than Rust's allow-by-default `elided_lifetimes_in_paths`, deliberately:
confusing two lifetimes eventually fails borrow-check; confusing two clocks is a silent CDC
hazard. The lint applies to *bindings and signatures only*, never to intermediate
expressions.

## Future work: richer domains

Later versions want clock+reset domains, and delay-indexed domains (cf. Clash's
`DSignal dom n a`). Three mechanisms, doing different jobs:

1. **The lattice** is for *silent coercions* — only ever-sound, never-informative forgetting.
   It stays `@const`-only. Forgetting a reset or a delay is precisely the information those
   domains exist to track, so it must be a visible operation (`x.forget_delay()`, explicit
   cast), not a subtyping edge.
2. **Sorts** are for *capabilities* — what an operation requires to exist. Small hierarchy:
   `ClockReset ⊑ Clock ⊑ Domain`. Subsumption only where *every* operation of the supersort
   remains correct unchanged: a reset-carrying domain is honestly a clock domain
   (`flipflop{dom D: Clock}` instantiates at it; the reset rides along), so `ClockReset ⊑
   Clock` holds. Sort subsumption is resolved at domain-variable instantiation — cheap,
   local, and separate from value-level subtyping.
3. **Indices** are for structure that operations *transform*. Delay is an index, not a sort
   or lattice point — domains become a small term language with constructors:

   ```polar
   fn dreg{dom C: Clock, N: integer}(x: T @ Delayed(C, N)) -> T @ Delayed(C, N + 1)
   ```

   The clock component passes through; the index does arithmetic, solved by the same
   unification-plus-integer-obligations machinery as `uint(N)` widths. **No subtyping between
   delays**: `Delayed(c, 3)` ≠ `Delayed(c, 4)`, and mixing them is the timing error the
   feature catches (as in Clash: equal `n` to combine, `delayedRegister : n → n+d`, explicit
   `toSignal` to forget). `Delayed` is *not* `⊑ Clock` — plain `reg : T@D -> T@D` would be
   wrong on a delayed domain (it must bump the index) — so the underlying clock is reached by
   projection, `clock_of(Delayed(c, n)) = c`, not subsumption. (Clash hangs the delay index
   on the signal type and keeps one domain kind; Polar folds it into the domain term because
   `@` is the single annotation slot. The behavioral lessons carry over regardless.)

**Implementation note for the current pass**: represent domains in the checker as a term
language with a sort function — `Const | ClockAtom(c) | ...` — not a flat atom set, even
though only those two constructors exist today. Then `WithReset(c, r)` and `Delayed(c, n)`
are later additions of constructors + obligations, not a solver rework, and `@const` remains
the only lattice edge throughout.

## Open questions

- **Mixed-domain opt-out for lifted polymorphic structs.** Should lifting impose
  `T @ __Dom` on type parameters by default? Recommendation: yes — single-domain is
  overwhelmingly the common case in RTL — with a cheap escape hatch (e.g. `_1: T @ any`) for
  user-defined mixed-domain containers. The alternative (no constraint by default) leaves
  `Pair(uint(8)) @clk` under-constrained.
- **`param` vs `@const`** as the field-level spelling (carried over from the original aims).
- **`WithReset(c, r)` vs bare `c` interop**: physically sound (same wires, same clock) but
  the system sees distinct domains. Start strict (fully distinct, explicit coercion) and let
  usage decide.
- **Surface syntax**: the `where T @ D` bound form, and the spelling of `Retag` (`T @! B`?).
