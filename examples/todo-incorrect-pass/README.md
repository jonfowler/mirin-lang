# todo-incorrect-pass

Examples that the compiler currently accepts but that **must be rejected**
once the corresponding checker lands. No test consumes this directory; it is
a worklist. As each file starts failing for the documented reason, move it to
`examples/fail-expected/` (covered by the
`fail_expected_examples_produce_diagnostics` harness).

The original Q7 phase-C worklist has fully flipped (`two-doms-fn`,
`when_no_clk`, `cross-reset`, `clocked-width`, `mixed-struct-clocks` — all in
`fail-expected/` now; `no-dom-reg` became `working/inferred_dom_reg.plr` when
the elision rules settled bare body types as domain-inferred).

The current entries come from the post-Q7 compiler review (2026-06), grouped
by the checker that needs to land:

**Call/record shape checking (infer):**

- `extra-args.plr` — surplus positional args silently dropped.
- `missing-args.plr` — missing required args; instance port left floating.
- `record-bad-field.plr` — unknown field name in a record constructor.
- `record-missing-field.plr` — omitted field; output leaf left undriven.
- `dup-record-field.plr` — duplicate field; first write silently wins.
- `field-on-scalar.plr` — field access on a scalar; silent `Type::Error`,
  undriven output.

**Lowering diagnostics (sig/body):**

- `unresolved-type.plr` — unknown type name lowers to `Type::Error` with no
  diagnostic; emitted as a 1-bit port.
- `num-overflow.plr` — literal beyond u64 parses to 0 (`unwrap_or(0)`).
- `named-dom-cross.plr` — named type args (`DF{c1}(...)`) are never lowered,
  so a clock-domain crossing type-checks. The domain-checking hole with the
  highest stakes here.

**Driver checking (check_drivers):**

- `double-drive-field.plr` — field-LHS equations aren't counted as drivers,
  so a whole-var drive plus a field drive emits a multi-driven net.
  Reject-side twin of `examples/todo/field-drivers.plr` (per-field driving
  falsely rejected).
