# todo-incorrect-pass

Examples that the compiler currently accepts but that **must be rejected**
once the corresponding checker lands. No test consumes this directory; it is
a worklist. As each file starts failing for the documented reason, move it to
`examples/fail-expected/` (covered by the
`fail_expected_examples_produce_diagnostics` harness).

The original Q7 phase-C worklist has fully flipped (`two-doms-fn`,
`when_no_clk`, `cross-reset`, `clocked-width`, `mixed-struct-clocks` — all in
`fail-expected/` now; `no-dom-reg` became `working/inferred_dom_reg.mrn` when
the elision rules settled bare body types as domain-inferred).

The post-Q7 review worklist (2026-06) has also fully flipped:

- **Call/record shape checking (infer):** `extra-args`, `missing-args`,
  `record-bad-field`, `record-missing-field`, `dup-record-field`,
  `field-on-scalar` — infer checks positional arity and record/field shape.
- **Lowering diagnostics (sig/body):** `unresolved-type` (UnresolvedType),
  `num-overflow` (NumberTooLarge), `named-dom-cross` (named type args lower
  to real Domain args; the CDC is a DomainMismatch).
- **Driver checking:** `double-drive-field` (per-leaf drive paths; overlap =
  MultipleDrivers). Its passing twin `working/field_drivers.mrn` wires a
  struct field-by-field.

Open worklist — **aggregate domains** (2026-06, planning/aggregate_domains.md).
A domain stored on an aggregate type (rather than on its leaves) is reconciled
with its element domains only lazily and one-directionally, so `@` on a Vec or
tuple launders clock domains:

- `cdc-launder-vec`, `cdc-launder-tuple` — `@b` on an aggregate stamps its
  element on reads but never unifies with the incoming `@a` data on writes, so
  `@a` exits as `@b`. Flip = DomainMismatch once aggregate `@` propagates into
  element slots at lowering (Stage 1).
- `vec-domain-drift` — aggregate `@a` and an explicit element `@b` silently
  disagree. Flip = DomainMismatch / unrepresentable once domains live on
  leaves (Stage 3).

The element-level (`Vec(N, uint(8) @b)`), struct-field, and direct `@a`→`@b`
crossings already reject correctly and guard against regressions.
