# for loops

Status: v1 landed. `for` is STRUCTURAL REPLICATION, emitted as a real,
NAMED SV generate-for — never unrolled — so the Verilog hierarchy is
recoverable: instance paths are `g_<elem>[i].<name>`.

```polar
for x in v { … }                  // elem only
for i, x in v.enumerate() { … }   // index + elem — i IS the genvar
```

- The elem binding is an ordinary per-iteration local: the block contains
  `logic … x; assign x = v[i];` — readable, hierarchical (`g_x[i].x`),
  and exactly "replace x with v[i]" without textual substitution.
- `.enumerate()` is recognised at lowering (not a real method): the pair
  form requires it, the single form forbids it. The index is REUSED as
  the genvar directly — no index vector is ever constructed. Its type is
  `integer` (elaboration-time), so it indexes without bounds asserts
  (the loop bound is the proof).
- Iterables: `Vec(N, A)` (elem `A`) and `bits(N)` (elem `bool`). The
  restricted iterable forms match if-conditions (a trailing `{` opens
  the body).
- Bodies: `let`s, element/field assignment (`out[i] = …` — counts as a
  whole-place drive: v1 has no partial-drive tracking, and the loop
  covers every index by construction), and component calls — one
  instance per iteration inside the named block.
- The loop bound renders symbolically for parametric lengths
  (`for (genvar i = 0; i < n; …)`).

Later: ranges (`for i in 0..n` without a vector), early termination is
deliberately NEVER (hardware), reductions/comprehension form
(`let y = for … { … }`), partial-drive tracking for disjoint loops.
