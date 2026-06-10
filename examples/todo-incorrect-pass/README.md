# todo-incorrect-pass

Examples that the compiler currently accepts but that **must be rejected** once
domain checking (planning/domain_checking_redux.md) is implemented. No test
consumes this directory; it is a worklist. As each file starts failing for the
documented reason, move it to `examples/fail-expected/`.

- `two-doms-fn.plr` — calling a lifted pure function with arguments on two
  different clocks (shared-domain lifting must reject it).
- `when_no_clk.plr` — explicit-`dom` signature with unannotated parameter and
  return types (explicit mode requires annotations).

`no-dom-reg.plr` used to live here under the assumption that a bare body type
meant `@const`; the elision rules settled the other way (bare types in bodies
are domain-inferred), so it moved to `working/inferred_dom_reg.plr`.
