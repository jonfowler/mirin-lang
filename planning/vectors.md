# Vectors

Status: v1 surface landing (this doc records the decisions). The deliberate
small start: `Vec(N, A)`, whole-value construction, single-element indexing.

## The type

`Vec(N, A)` — N elements of type A. N is a const arg, A a type: both ride
the existing generic machinery, so `Vec(n, T)` is polymorphic in fns with
`param n: integer, param T: Type` today, for free.

## Construction

```polar
let v: Vec(3, uint(4)) = [1, 2, 3];   // list form
let z: Vec(n, uint(4)) = [0; n];      // repeat form — REQUIRED for parametric n
```

`[` is virgin syntax (type args use parens), so brackets are unambiguous
everywhere. The repeat form is not sugar: a parametric-length vector cannot
be written as a list. Rust-shaped on purpose.

## Indexing

```polar
v[0]      // static: bounds-checked at compile time when index and len ground
v[i]      // i: uint(k) — a hardware select (an SV mux)
b[2]      // b: bits(n) — yields bool (no separate bit type)
```

Postfix `[…]`, type-driven: `Vec(N, A)[i] → A`, `bits(N)[i] → bool`. The
index may be a literal/`integer` (static select) or a `uint` (dynamic); its
domain joins the base's (a cross-clock index is the usual CDC error).
v1 checks ground-literal indexes against ground lengths. A DYNAMIC
(uint-typed) index additionally emits a simulation-time bounds assert
(`always_comb assert (sel < 3);`) unless its width provably cannot
express an out-of-range value (2^w ≤ N) — synthesis ignores it,
simulation fires at the access. The end-goal safety story is a bounded
`Index(N)` type (Clash-shaped) once explicit conversions land — as the
OPT-IN strict form; uint access stays first-class with the assert as
its honesty layer.

## Flattening: struct-of-arrays

`Vec(3, Packet)` flattens to one ARRAY per struct leaf — `v__valid [0:2]`,
`v__payload [0:2]` (unpacked dims after the name) — never an array of
bundles. Construction assigns whole leaves with SV assignment patterns
(`assign v__valid = '{a__valid, b__valid, c__valid};`), so the existing
leaf pipeline carries vectors without new wiring concepts.

## Deliberately later

- **Element assignment** (`v[0] = x;`) and slices (`v[3:1]`) — with the
  driver-tracking design for partial drives.
- **bits slicing** (`x[7:4]`) — same machinery, planning/bits.md.
- **for constructs** over vectors — the todo-list's "for" workstream.
- **Variable-sized vectors** (testing): a separate type (`List(A)`-shaped),
  but the SYNTAX here is type-driven (literals take their type from
  context, indexing dispatches on the base type), so it transfers; when a
  second indexable type exists, indexing routes through an `Index` trait
  instead of the builtin match.
- `.reg` on whole vectors.
