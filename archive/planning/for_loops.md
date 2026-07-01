# for loops

Status: v1 landed. `for` is STRUCTURAL REPLICATION, emitted as a real,
NAMED SV generate-for — never unrolled — so the Verilog hierarchy is
recoverable: instance paths are `g_<elem>[i].<name>`.

```mirin
for x in v { … }                    // elem only
for (i, x) in v.enumerate() { … }   // index + elem — i IS the genvar
for (a, b) in pairs { … }           // tuple elems destructure in the body
```

- The binder is a PATTERN (planning/tuples.md): a bare name, or a tuple
  pattern that destructures the element at the top of the body.
- The elem binding is an ordinary per-iteration local: the block contains
  `logic … x; assign x = v[i];` — readable, hierarchical (`g_x[i].x`),
  and exactly "replace x with v[i]" without textual substitution.
- `.enumerate()` IS a real method — `Vec(N, A) -> Vec(N, (integer, A))` —
  but a for-loop also recognises it so the index binder REUSES the genvar
  directly; no index vector is ever constructed. The enumerate binder is
  `(i, elem-pattern)` with `i` a bare name. The index's type is `integer`
  (elaboration-time), so it indexes without bounds asserts (the loop
  bound is the proof).
- Iterables: `Vec(N, A)` (elem `A`) and `bits(N)` (elem `bool`).
- HEADER positions (if-conditions, for-iterables, when-events) take the
  full expression grammar minus BARE record literals — Rust's
  no-struct-literal contexts; parenthesize a record literal to use one.
  Named-arg method calls work in headers because a named-argument list
  is always followed by a positional list (`x.reg{rstn}(0)`) — the GLR
  fork resolves at the `(`.
- Bodies: `let`s, element/field assignment (`out[i] = …` — counts as a
  whole-place drive: v1 has no partial-drive tracking, and the loop
  covers every index by construction), and component calls — one
  instance per iteration inside the named block.
- The loop bound renders symbolically for parametric lengths
  (`for (genvar i = 0; i < n; …)`).

`range(n)` (a prelude builtin typed `-> Vec(n, integer)`) iterates
without materialising anything: the genvar IS the element
(`for i in range(4) { shifted[i] = …; }`). Outside loops, element
assignment tracks PARTIAL drives: a ground index is its own drive path
(`"[2]"`), distinct indexes are disjoint, completeness requires every
element covered, and the same element twice (or an element plus the
whole) conflicts. A `for`-bound index drive still covers the whole
place (the loop spans every index). Dynamically-indexed drives
(`v[sel] = …`) are not drive targets.

Later: `0..n` range syntax, early termination is deliberately NEVER
(hardware), reductions/comprehension form (`let y = for … { … }`),
partial-drive tracking for disjoint LOOPS (two loops each driving half).
const_eval of `range` outside for-position (needs const vec values).
