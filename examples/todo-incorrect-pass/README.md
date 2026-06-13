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

Flipped at Stage 1 (aggregate `@D` now propagates into element slots at
lowering, so a write meets a concrete element domain): `cdc-launder-vec`,
`cdc-launder-tuple` → `fail-expected/` (DomainMismatch).

Still open:

- `vec-domain-drift` — aggregate `@a` and an explicit element `@b` silently
  disagree (the stamp fills only *unspecified* slots, so a conflicting
  explicit element domain isn't caught). Flips at Stage 3, when domains live
  on leaves and the aggregate has no domain to drift from.
