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

- [ ] **S1 — MIR skeleton.** `src/mir/` module, `Mir` IR (arena, types-on-node),
  `mir_of(def)` derived query, HIR→MIR lowering as a faithful typed mirror.
  Unifications already in S1: `TypedLiteral`→`Number` (node carries the type);
  `Call`/`MethodCall`/`TypePathCall`/operator-call → one resolved `Call`
  (callee `DefId` + baked substs + optional receiver). Smoke test builds MIR
  over the working corpus (loud panics on unhandled shapes). Nothing consumes it.
- [ ] **S2 — Places.** Introduce `Place` (local + projections: field, index,
  tuple-field; bit-range later) for equation LHS / connection targets / returns.
  Unifies slice-set, out-connections, named-port args, returns onto one
  direction-carrying place model (see mir.md and the named-args discussion).
- [ ] **S3 — Retarget emission onto MIR.** `sv_module`/`build_module` read `mir_of`
  instead of `body`+`infer`. Parity gate against current backend + `mirin-compiler-old`.
- [ ] **S4 — Slice desugar on MIR.** Type-directed `x[a..b]` → part-select
  primitive + zero-width `const if` guard (retires SliceNotImplemented).
- [ ] **S5 — Flatten on MIR.** Aggregates → leaves as a MIR pass.
- [ ] **S6 — Mono + mono_check on MIR.** "apply recorded substs" + ground-regime check.
- [ ] **S7 — Inline on MIR.** rustc-Integrator-style splice (subsumes inline_bodies.md).
- [ ] **S8 — Re-add const-eval during infer** via the per-item anon-const units.

## Status log (newest first)

- 2026-06-24: starting S1.
