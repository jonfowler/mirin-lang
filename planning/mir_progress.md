# MIR implementation — progress & loop state

> Living checklist for the MIR migration (planning/mir.md is the design).
> Updated as each slice lands. This is the durable hand-off state across
> context compaction: read this + `git log` to resume.

## Migration plan (from mir.md §"Incremental migration")

1. Introduce `mir(def)` + HIR→MIR lowering, types baked in; nothing consumes it. ← **in progress**
2. Make emission read MIR instead of HIR+side-table (parity gate vs current backend + old oracle).
3. Move passes onto MIR one at a time: slice desugar → flatten → mono+mono_check → inline.
4. Keep the during-infer `ConstArg` path throughout; revisit subsume-vs-keep last.

## Decisions taken for this run (per Jon, 2026-06-24)

- **Work in a loop**: implement → commit → review → plan next → repeat.
- **Tests may go dark** temporarily if it moves things forward.
- **Const-eval-in-infer may be dropped** during the transition (re-add with the
  specced anon-const design later). It is extra work orthogonal to landing MIR.
- **Negative-space style**: make assumptions explicit in code — `panic!`/`todo!`
  on shapes we assert don't occur / aren't handled yet, rather than silent
  fallthrough.

## Slices

- [x] **S1 — MIR skeleton.** `src/mir/` module, `Mir` IR (arena, types-on-node),
  `mir_of(def)` derived query, HIR→MIR lowering as a faithful typed mirror.
  Unifications: `TypedLiteral`→`Number` (node carries the type);
  `Call`/`MethodCall`/`TypePathCall`/operator-call → one resolved `Call`
  (callee `DefId` + recorded substs + optional receiver); builtins
  (reg/posedge/replace/enumerate) as a closed `Builtin` node. Corpus smoke test.
  Reviewed (fresh context): no blockers; fixes applied (below). Nothing consumes it.
- [x] **S2 — Places (equation LHS).** `Place { base: LocalId, projections }`,
  `Projection = Field | Index` (BitRange in S4). `MStmt::Equation.lhs` is now a
  `Place`. Aligns with the backend's `backend_root_local`. **S2b** (out-conn /
  out-record / out-arg targets → places; the connection-unification payoff) and
  **slice-set BitRange** (S4) still pending.
- [ ] **S3 — Retarget emission onto MIR.** `sv_module`/`build_module` read `mir_of`
  instead of `body`+`infer`. Parity gate against current backend + `mirin-compiler-old`.
- [ ] **S4 — Slice desugar on MIR.** Type-directed `x[a..b]` → part-select
  primitive + zero-width `const if` guard (retires SliceNotImplemented).
- [ ] **S5 — Flatten on MIR.** Aggregates → leaves as a MIR pass.
- [ ] **S6 — Mono + mono_check on MIR.** "apply recorded substs" + ground-regime check.
- [ ] **S7 — Inline on MIR.** rustc-Integrator-style splice (subsumes inline_bodies.md).
- [ ] **S8 — Re-add const-eval during infer** via the per-item anon-const units.

## Design notes for upcoming slices

### S2 — Places (the equation-LHS / connection-target model)

In an HDL equation system the LHS of an `Equation`, an out-connection target
(`=> target`), an out record field, and the result place (`return.x = …`) are all
**places** — addressable locations, not value expressions. HIR keeps them as
exprs; the backend pattern-matches the chains (`index_uses_forbound`, etc.).

MIR model:
```
Place { base: LocalId, projections: Vec<Projection> }
Projection = Field(String)        // x.field  AND  x.0 (tuple parts are Field("0"))
           | Index(MExprId)       // v[i] — i is a genvar/const in a drive target
           | BitRange{lo,width}   // slice-set — added in S4, not S2
```
Place-ification walks a `Local`/`Field`/`Index` chain to a root `Local`. A root
that is not a `Local` is **not a valid drive target** → panic (negative space):
value-shaped LHSs cannot occur (patterns already desugared to synthetic locals
in HIR lowering; `return`/named results are locals with `result_base`).

Scope split:
- **S2**: `MStmt::Equation { lhs: Place, rhs }`. The driver/completeness checker
  (S-future) reasons over places — the clean home for slice-set completeness
  (mir.md §"Slicing on MIR — Set").
- **S2b**: place-ify out-arg / out-named / out-record targets too, unifying all
  connection targets onto one direction-carrying place model (the named-args
  discussion: one connection rule for instance + inline, value + port).

Validation worry: with no consumer yet, a place-ification bug is silent (smoke
test only checks no-panic). Mitigation: the S3 emission retarget is the real
parity gate; until then keep S2 mechanical and add a debug dump (below) so MIR
is at least inspectable.

### S3 — Retarget emission onto MIR (the parity gate)

`backend/lower.rs` is ~4.2k lines reading `body` + `infer` directly. Retargeting
is the high-value, high-risk step that proves MIR is correct/complete. Strategy:
- **Do NOT big-bang.** First add a MIR **debug dump** (`mir/pretty.rs` + a
  `--emit mir` hook mirroring `--emit cst`) — a cheap real consumer to eyeball
  structure on the corpus.
- Then move `build_module` to read `mir_of(def)` instead of `body`+`inf`,
  function by function, gating each on:
  1. the existing `examples` CLEAN/VERILATOR_CLEAN tests (byte-for-byte SV), and
  2. the `mirin-compiler-old` parity oracle.
- The call sites that read `self.inf.call_subst` / `method_resolution` /
  `expr_type` become reads off the MIR node (types-on-node) — this is where MIR
  earns its keep and where the named-args/connection logic unifies.
- Const-eval-in-infer may be dropped during this (per Jon); re-add as S8.

## Status log (newest first)

- 2026-06-24: S1 landed (commit). Typed MIR skeleton + `mir_of` + corpus smoke
  test; calls unified, builtins as closed node, TypedLiteral folded.
- 2026-06-24: S1 reviewed (fresh-context agent) — no blockers; verdict "sound
  foundation". S2 (places) implemented + review fixes applied in one commit:
  (1) negative-space panics now degrade to `Missing`/degenerate places on
  malformed bodies (`well_typed` gate = body+infer diagnostics clean), reserving
  panics for well-formed-but-unhandled — locked in by a fail-expected MIR smoke
  test; (2) cross-ref comment in `infer_method` ↔ `mir::lower::builtin_method`
  (single source of truth for the builtin set); (4) `debug_assert` in `ty_of`
  turns a missing type on a clean body from a silent `Error` into a loud failure;
  (5) reworded `Call.substs` doc — it is the inference-recorded subst, not the
  ground/mono subst (S6 resolves trait-instance overrides + fills generics).
  Next: S2b (out-targets → places) or begin S3 (emission retarget) + MIR dump.
