# Tuples

Rust-shaped structural products: `(A, B)` types, `(a, b)` expressions, `x.0`
projection, `let (a, b) = e;` destructuring, and `for (i, x) in v.enumerate()`.
Tuples are the anonymous aggregate; they may contain ports and vecs, and each
element carries its own clock domain — they are the stress test for fully
polymorphic types and the domain machinery.

## Decisions

- **Arity ≥ 2.** No unit `()` and no 1-tuples `(T,)` until something needs
  them. `(e)` stays a parenthesized expression; the comma makes the tuple.
  Trailing commas allowed.
- **Representation: `ValueKind::Tuple(Vec<Type>)`** — elements are *full*
  types, each with its own domain (unlike Vec, whose elements share one
  element type). The rust analogy is `TyKind::Tuple(&[Ty])`.
- **Domains follow the Vec/struct convention**: an element written without a
  domain is `Unspecified`, meaning "the tuple binding's own domain" — stamped
  at use sites exactly as record fields are. An element with an explicit
  domain keeps it, so `(uint(8) @a, uint(8) @b)` is a legal mixed-domain
  tuple; unify/subsume recurse element-wise and check each domain
  independently.
- **Projection reuses `Field`.** `x.0` lowers to
  `ExprKind::Field { receiver, field: "0" }` — no new HIR variant. This buys
  `place_of` paths, drive-completeness segments, and the backend's
  suffix-strip machinery for free; only typing dispatches on the receiver
  being a tuple. (rustc similarly reuses its field-access HIR with numeric
  idents.)
- **Patterns desugar in CST→HIR lowering**, not as a HIR concept: `let
  (a, b) = e;` becomes a synthetic local bound to `e` plus one `let` per
  element projecting `.0`, `.1`, … recursively (nested patterns allowed).
  HIR keeps single-name `Stmt::Let`; there is no `Pat` IR. Identifiers only —
  no `_`, no literals in patterns (yet).
- **`for` binders are patterns** now: `for x in v`, `for (i, x) in
  v.enumerate()`, `for (a, b) in vec_of_pairs`. The old two-identifier
  `for i, x in …` form is removed. `for (i, x) in v.enumerate()` keeps its
  special lowering — `i` *is* the genvar (`LocalKind::ForBound`), `x` binds
  `v[i]` — so the generate hierarchy is unchanged. Any other tuple binder
  desugars like `let`: elem local + projection lets in the loop body.
- **`enumerate` is a real method** with a *proper* type:
  `enumerate : {dom D} (Vec(N, A) @D) -> Vec(N, (integer @const, A @D)) @D`.
  The index element is `@const` (it is `0..N-1`, structurally independent of
  the clocked data); the data element keeps the receiver's domain. The
  const-ness lives in enumerate's signature, NOT in a special rule on the
  `integer` type — `integer`'s domain follows annotation/data-flow like any
  type, so a testbench `integer @clk` is perfectly legal. Inside a `for`,
  enumerate is also *recognised* so the index reuses the genvar.
- **Flattening**: a tuple leaf is its element index as a name segment —
  `x.0.valid` flattens to `x__0__valid`, a tuple-typed result port to
  `result__0`, `result__1`. Port elements fold direction exactly as port
  fields of structs do today.

## Mixed-domain aggregates and the lift

A *pure* (unannotated) function lifts every domain slot — including the
elements of a returned tuple/vec — onto its single shared `__Dom`
(domain_checking_redux.md). So a pure function cannot directly **return** an
aggregate whose elements live on *different* domains: e.g.
`fn f(v: Vec(3, uint(8))) -> Vec(3, (integer, uint(8))) { v.enumerate() }`
is rejected — the lift makes the written index element `@__Dom`, but
enumerate's index is `@const`, and aggregate elements are invariant (no
nested coercion). This is consistent with "drop to explicit domains when the
relationship between domains *is* the point": the index-is-const / data-is-
clocked split is exactly such a relationship, so write the explicit
signature. The common usages need none of this — a `for`-loop consumes the
index as a genvar, and a `let` binding takes enumerate's honest type
directly (a `let` is not lifted).

The principled general solution (noted, not built): **dependency-aware
lifting** — split the lifted domains by data dependency, so an output
independent of every clocked input keeps `@const` while a dependent output
shares the input's domain. That would let the pure-return form above
typecheck without explicit annotation. It needs domain flow from the body,
which the signature firewall (`sig_of`) does not see today.

## Non-goals (this pass)

- Tuple structs / named tuples (structs already exist).
- `_` and literal patterns; match.
- Tuples as trait `Self` (no operator impls on tuples).
- Tuple equality — needs derived `Eq`; comes with trait derive work.
- Dependency-aware domain lifting (above) — pure-return of a mixed-domain
  aggregate needs explicit domains for now.

## Returned ports are bidirectional

A function may RETURN a port (bare, or as a tuple element). A returned
port's `out` fields are module outputs, but its `in` fields are module
INPUTS — the downstream's backpressure — folded exactly as for an `out`
port parameter. `drive_result` drives the `out` leaves forward
(`result__x = …`) and the `in` leaves in reverse (`… = result__x`), the
same split as a record `field => target` binding. See
`examples/working/dataflow_stage.mrn` (a pipeline register returning its
downstream `Stream`) and `tests/rtl/test_dataflow_stage.py`.
