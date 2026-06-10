# todo-incorrect-pass

Examples that the compiler currently accepts but that **must be rejected**
once the corresponding checker lands (see `planning/q7_terms_and_domains.md`
and `planning/domain_checking_redux.md`). No test consumes this directory; it
is a worklist. As each file starts failing for the documented reason, move it
to `examples/fail-expected/` (covered by the
`fail_expected_examples_produce_diagnostics` harness).

Currently empty — the whole Q7 phase-C worklist has flipped:

- `two-doms-fn.plr`, `when_no_clk.plr` — C1 (shared-domain lifting,
  explicit-mode annotation requirement).
- `cross-reset.plr`, `clocked-width.plr` — C2 (reg's one Clock-sorted domain
  for data + reset; const-position domain check).
- `mixed-struct-clocks.plr` — C3 (single-domain record stamping).

Each fail's passing twin lives in `examples/working/` (`reg_const_input`,
`struct_two_clocks`, `dual_clock_lift`, `const_then_clocked`) and must stay
green.

`no-dom-reg.plr` used to live here under the assumption that a bare body type
meant `@const`; the elision rules settled the other way (bare types in bodies
are domain-inferred), so it moved to `working/inferred_dom_reg.plr`.
