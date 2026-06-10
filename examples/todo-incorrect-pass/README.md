# todo-incorrect-pass

Examples that the compiler currently accepts but that **must be rejected** once
domain checking (planning/domain_checking_redux.md) is implemented. No test
consumes this directory; it is a worklist. As each file starts failing for the
documented reason, move it to `examples/fail-expected/`.

- `cross-reset.plr` — `.reg` with a reset on a different clock than the data
  (reg's one `dom D: Clock` covers both). [Q7 phase C2]
- `clocked-width.plr` — a clocked value in `uint(...)` width position (const
  position requires domain @const). [Q7 phase C2]
- `mixed-struct-clocks.plr` — one lifted single-domain struct constructed with
  fields from two clocks. [Q7 phase C3]

Each entry's passing twin, where one exists, lives in `examples/working/`
(`reg_const_input`, `struct_two_clocks`, `dual_clock_lift`,
`const_then_clocked`) — those exercise the same machinery from the legal side
and must stay green while these flip.

`no-dom-reg.plr` used to live here under the assumption that a bare body type
meant `@const`; the elision rules settled the other way (bare types in bodies
are domain-inferred), so it moved to `working/inferred_dom_reg.plr`.

Flipped so far (now in `fail-expected/`, covered by the
`fail_expected_examples_produce_diagnostics` harness): `two-doms-fn.plr` and
`when_no_clk.plr` (Q7 phase C1: shared-domain lifting + explicit-mode
annotation requirement).
