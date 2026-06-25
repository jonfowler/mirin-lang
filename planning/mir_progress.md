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
  `Projection = Field | Index` (BitRange in S4). `MStmt::Equation.lhs` is a
  `Place`. Aligns with the backend's `backend_root_local`.
- [x] **S2b — Connection unification.** One `Conn { In(MExprId) | Out(Place) }`
  for every connection site (positional args, named args, record fields),
  replacing the `out: bool` + value-expr pair. Out-connections (`=> target`)
  are places; in-connections are values — the in/out split *is* the place/value
  split, lowered in one `lower_conn`. Validated via dump:
  `stream8 { valid = …, data = …, ready => l5 }`. This retires the backend's
  per-site direction re-derivation when S3 lands. **slice-set BitRange** (S4) pending.
- [ ] **S3 — Retarget emission onto MIR.** `sv_module`/`build_module` read `mir_of`
  instead of `body`+`infer`. Parity gate: `golden_sv_snapshot` (built, 89 cases).
  Planning-reviewed; ordered sub-steps + invariants in the design note below.
  - [x] S3.0 — golden-SV byte-for-byte gate (`tests/golden/`).
  - [x] S3.1 — MIR debug dump (`mir/pretty.rs` + `--emit mir`); first real
    consumer. Validated: `value + 3` → `l0.call add<8, D0>(3)` (operator unified,
    substs baked, `:uint(8)@D0` types on every node).
  - [x] S3.2a — HIR↔MIR bridge (`Mir.of_hir`), the retarget seam.
  - [x] S3.2b — type-source swap: the backend's expr-type reads now source from
    MIR (`mir_expr_type` via `of_hir`), incl. the central `expr_type`,
    `expr_type_width`, `index_bounds_assert`, reg-clock typing, leaf-typing, and
    `actual_type`. Golden-SV byte-for-byte unchanged → MIR is load-bearing for
    types. (Local-type reads deferred — `self`-param + `Option`/`Error` subtlety.)
  - [ ] S3.2c..e — `as_reg`→`Builtin`, calls, control flow onto MIR. (Plan below.)
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

### S3 — Retarget emission onto MIR (planning-reviewed 2026-06-25)

`backend/lower.rs` is ~4.2k lines reading `body` + `infer` directly. Retargeting
is the high-value, high-risk step that proves MIR is correct/complete. The S3
planning review corrected two false premises and refined the order.

**Invariants to hold (do not violate these during S3/S8):**
- **Backend-time const-eval is UNCHANGED by the migration and unrelated to the
  S8 const-eval-in-infer drop.** `ground_widths`, `eval_const_cond`,
  `emit_instance` `#(.N(…))` params, and `ConstAssoc` value all call
  `const_eval::eval_const/eval_cond(self.db, self.krate, self.def, …)` at *emit*.
  The retarget changes *which node the `ConstArg`/`Type` came from*, not *who
  evaluates it*. Keep these calls as-is.
- **`MExpr.ty` is inference-recorded, NOT mono-ground.** It still carries the
  def's own generic `Param`s and ungrounded widths. The backend MUST keep
  applying `self_subst` + `ground_widths` to `mexpr.ty` (as it does to
  `inf.expr_type(e)` today via `expr_type`/`expr_type_width`). A retarget that
  trusts `mexpr.ty` as a final width miscompiles every parametric example.
- **"Drop const-eval-in-infer" (S8) means ONLY: stop dispatch-grounding /
  `const if`-folding during infer.** It must NOT drop `call_subst` recording
  (MIR copies it into `Call.substs`; the backend reads it everywhere) nor the
  `const_residuals`/`fit_residuals` side-tables (emit `initial assert`s; MIR has
  nowhere to put them yet).

**Parity gate — built (commit):** `golden_sv_snapshot` compares byte-for-byte
against `tests/golden/*.sv` (89 cases). This is the real gate; VERILATOR_CLEAN
only lints (would pass a miscompile silently). The old oracle is a bonus, not
wired up — don't block on it.

**Flatten stays type-keyed (S5 decoupled from S3).** `flatten_leaves` reads only
`Type` + `sig.fields`, which MIR carries on every node — keep calling it with
`mexpr.ty`. Do NOT move flatten onto MIR before emission reads MIR.

**Ordered S3 sub-steps (from the review):**
1. `mir/pretty.rs` + `--emit mir` (eyeball aid; NOT the gate).
2. First subtarget: route `expr_value`'s scalar cases (Number/Bool/Local/
   ConstParam/ConstAssoc/Index) + the unified operator `Call` through the MIR
   node. Validate on `add_constant` (Local read + operator-Call + let + tail; no
   flatten/instance/places). This exercises the riskiest representational change
   (four-call-shapes → one `Call`) against a golden.
3. Statement lowering: use `Place.base` (S2) to replace `backend_root_local`;
   translate `as_reg` MethodCall-match → `Builtin::Reg` (carries receiver+args,
   indices line up).
4. `expr_leaves`/flatten callers read `MExpr.ty` (flatten unchanged).
5. Call emission (`emit_instance`/`call_value_leaves`) read `Call.substs`/
   `receiver`/`args`/`named` off MIR; keep `resolve_trait_instance` + inline on
   HIR/`crate_def_map` (inline is S7 — do not MIR-ify it yet).
6. **Then S2b** (out-arg/out-named/out-record → places) to retire
   `place_leaves_dir`/`value_leaves_dir` HIR matches + the emit_instance
   direction TODOs.
7. Drop S8 only after the retarget is green, and only the dispatch/`const if`
   grounding (never `call_subst`/residuals).

Highest risk per the review: the *absence* of a byte-for-byte gate — now retired
by `golden_sv_snapshot`. Next-subtlest: `resolve_trait_instance` re-selection
(keep it reading the recorded subst off the MIR `Call`; `df_example_poly`/
`trait_*` goldens catch mistakes) and trusting `MExpr.ty` as ground.

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
- 2026-06-25: S3 planning-reviewed (fresh context). Corrected two false
  premises: (1) no byte-for-byte SV gate existed — BUILT it (`golden_sv_snapshot`,
  89 cases, committed); (2) backend const-eval is backend-*time*, not infer-time,
  so the S8 drop doesn't break emission. Folded the reviewed invariants +
  ordered S3 sub-steps into the S3 design note above. Next loop iteration:
  S3.1 — `mir/pretty.rs` + `--emit mir`, then S3.2 first scalar subtarget on
  `add_constant` behind the golden gate.
- 2026-06-25: S3.1 landed — `mir/pretty.rs` + `--emit mir` + a fast dump test.
  The dump confirms S1/S2 produce correct structure (unified call, baked types,
  places). **BLOCKER for S3.2+**: the actual emission retarget edits
  `backend/lower.rs`, which has uncommitted user WIP — cannot touch it without
  clobbering. S3.2+ is gated on that WIP landing/clearing. Until then, available
  MIR work is in `src/mir/` only: S2b (out-targets → places), cleanup, design.
- 2026-06-25: S2b landed — `Conn { In | Out(Place) }` unifies all connection
  sites. Out-connections place-ified (dump-validated on `record_out_conn`).
- 2026-06-25: **Correction** — `backend/lower.rs` is NOT blocked. Its earlier
  dirty state was my own pre-MIR commit `3076994` (named-arg TODOs), already
  committed. The only uncommitted files are user WIP elsewhere (`prelude.mrn`,
  `planning/{domain_checking,pack_resize,todo-list}.md`, `proposals/*` deletions)
  — none of which the retarget touches. **S3.2 (emission retarget) is unblocked.**
- 2026-06-25: S3.2a landed — the HIR↔MIR bridge. `Mir.of_hir(ExprId) ->
  Option<MExprId>` (populated in `push`) lets the backend, which keys on HIR ids,
  read MIR nodes incrementally before it walks MIR natively. Holds 1:1 at birth;
  retires once S4/S7 add nodes and emission reads MIR natively.

- 2026-06-25: S3.2b landed — backend expr-type reads source from MIR
  (`mir_expr_type` via `of_hir`); golden byte-for-byte unchanged, 127 lib green.
  MIR is now load-bearing for types. Realized the `of_hir` bridge only covers
  value-position exprs, so the rest (recognition/places/call-children) needs a
  native MIR walker — revised the plan to build `_mir` lowering twins as
  committable dead code (S3.2c→f), flipping the entry point last. Next: S3.2c
  `expr_value_mir`.

- 2026-06-25: S3.2c started — `expr_value_mir` dead-code twin: leaf arms
  (Number/Bool/Local/ConstParam/ConstAssoc/Missing) ported faithfully off the MIR
  node; cross-method arms (Call/Builtin/Index/When/If/ConstIf/Block/aggregates)
  are explicit `todo!`s naming their sub-step. Extracted id-agnostic
  `width_of_ty` (cleanup) shared by `expr_type_width` and the walker. Compiles
  (dead code); live path provably identity (refactor only) — lib green +
  add_constant emit byte-identical. Next: `expr_value_mir` Call/Index + the
  call/inline machinery on MIR (S3.2d).

## S3.2 entry plan (next fire)

The backend keys every read on a HIR `ExprId`; MIR has its own arena. The bridge
(`of_hir`) is the migration seam. Do the retarget as type-source-first, then
control-flow, each gated by `golden_sv_snapshot` (regenerate only on an
*intended* change, reviewing the diff):

1. **S3.2b — type-source swap.** In `build_module`, fetch `let mir = mir_of(db,
   krate, def)` and store it on `SvLower`. Replace `self.inf.expr_type(e)` /
   `local_type` reads with `self.mir.of_hir(e)` → `mexpr.ty` (and MIR local ty).
   **Keep** `self_subst` + `ground_widths` (MIR ty is inference-recorded, not
   ground — see invariants). Everything else stays on HIR. Golden must stay
   byte-for-byte. This proves types-on-node end-to-end with no control-flow
   churn. Watch: exprs with no `of_hir` entry (callee sub-exprs) — those reads
   should not have needed a type anyway; assert/fallback.
**Realization (2026-06-25, after S3.2b):** the `of_hir` bridge only covers
*value-position exprs* — types are leaf data sourced cleanly. But `as_reg`
recognition, `backend_root_local`→`Place.base`, and call children need
*statements/places*, which the bridge does NOT expose: an equation-LHS root
(`Local`/`Field`) is lowered via `lower_place`, not `lower_expr`, so it has no
`of_hir` entry; statements aren't keyed at all. And the consumers
(`emit_registers`, `expr_value`, `lower_stmts`) all take HIR `ExprId`, whereas
MIR children are `MExprId`. So S3.2c/d are NOT clean isolated swaps. The type
swap (S3.2b) was the one clean leaf-level win the bridge enables.

**Revised path — a native MIR walker, built as committable dead code:**
The backend lowering core (`lower_stmts`, `drive_result`, `expr_value`,
`expr_leaves`, `block_leaves`, …) is structurally near-identical to the MIR it
would walk — porting is mechanical: `ExprKind::X`→`MExprKind::X`,
`ExprId`→`MExprId`, `self.body.expr(e)`→`self.mir.expr(e)`,
`self.inf.expr_type(e)`→`self.mir.expr(e).ty`; the four call arms collapse to one
`Call`; builtins via `Builtin`; equation LHS via `Place`. Build the `_mir`
twins one at a time as `#[allow(dead_code)]` (compiles, golden untouched since
the HIR path stays wired), each its own commit:
- S3.2c — `expr_value_mir(MExprId)` (scalar + call + index + field). [started:
  leaf arms done — Number/Bool/Local/ConstParam/ConstAssoc/Missing; cross-method
  arms `todo!`. Extracted id-agnostic `width_of_ty` shared with `expr_type_width`.]
- S3.2d — `expr_leaves_mir` / `block_leaves_mir` (aggregates, calls-as-values).
- S3.2e — `lower_stmts_mir` / `drive_result_mir` (Let/Equation(Place)/When/For,
  `Builtin::Reg` for registers).
- S3.2f — **wire-up**: `lower_top_block` calls the `_mir` twins; delete the HIR
  lowering core and `mir_expr_type`'s inf-fallback becomes native. Golden must
  stay byte-for-byte at the flip. `resolve_trait_instance` + inline stay on
  `crate_def_map`/HIR until S7.
Once emission walks MIR natively, S4 (slice desugar) / S5 / S6 / S7 follow.
