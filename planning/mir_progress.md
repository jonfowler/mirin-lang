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
- [~] **S3 — Retarget emission onto MIR.** `build_module` walks `mir_of` for the
  defs the walker covers (parallel-entry behind a coverage predicate), the rest
  on HIR. Parity gate: `golden_sv_snapshot` (89 cases). The walker now lowers
  scalars/aggregates/inline-calls + Let/Equation(bare-local)/Return statements
  end-to-end, **golden byte-for-byte green**. Widening the predicate (instances,
  reg, when/if/for, projections) toward full coverage, then delete the HIR core.
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
- [~] **S4 — Slicing on MIR.** Reads: two-endpoint / offset / elision, over
  bits + Vec, literal / runtime / **const-param** endpoints — all end-to-end,
  verilator-clean. Slice-set (lvalue): bits `word[hi..lo] = …` via a
  `Projection::BitRange` + a distinct partial-drive path in completeness.
  Remaining: zero-width const-if guard, vec slice-set range coverage.
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

- 2026-06-25: **S4 step 6 — const-param slice endpoints (symbolic widths).**
  `slice_literal`/`sliced_ty` now build a `ConstArg` width (literals fold to
  `Lit`, a const generic param yields a symbolic `Op(Sub,…)`); the walker's
  `slice_range_sv` builds endpoints as `ConstArg`s and renders each via
  `render_const` (ground through `self_subst`/promoted, then fold-or-render). So
  `x[n..1]` → `bits(n-1)` → `x[(n - 1):1]` against the module's `#(parameter int
  n)`. Golden-stable for literal slices; `slice_param.mrn` verilator-clean
  (-Gn=8), in CLEAN+VERILATOR_CLEAN+golden. **Slicing now covers literal /
  runtime / parametric endpoints, both directions, read + set.** Remaining S4:
  zero-width const-if guard, vec slice-set range coverage (both niche).
- 2026-06-25: **S4 step 5 — slice-set (lvalue, bits).** `word[8..0] = lo;
  word[16..8] = hi;` on a `var bits(16)` → `word[7:0]=…; word[15:8]=…`,
  verilator-clean. `Projection::BitRange` (lvalue dual of the read Slice); the
  shared `slice_range_sv` emits the part-select target; `place_of` gives a
  DISTINCT partial-drive path per range (`[8..0]`), so tiling slices don't
  false-conflict and (for bits) completeness imposes no range coverage (deferred).
  `slice_set.mrn` in CLEAN+VERILATOR_CLEAN+golden. **Both halves of the original
  "slice and slice setting" ask are now delivered.** Remaining S4: zero-width
  const-if guard, param/const-expr endpoints, vec slice-set range coverage.
- 2026-06-25: **S4 step 4 — slice elision.** An elided endpoint defaults from
  the base length `N` (literal): bits `x[8..]`→`x[7:0]`, `x[..4]`→`x[15:4]`; vec
  dual; bare `x[..]` rejected. infer + walker read `N` from the base type;
  predicate admits None-or-literal endpoints (not both elided). `slice_elide.mrn`
  verilator-clean, in CLEAN+VERILATOR_CLEAN+golden. The slice *read* syntax is
  now complete (two-endpoint, offset, elision; bits + vec; literal + runtime
  base). Next S4: zero-width const-if guard, param/const-expr endpoints,
  slice-set (lvalue).
- 2026-06-25: **S4 step 3 — offset form / runtime-base slices.** `x[off..+w]`
  (width const, base may be RUNTIME) → the SV indexed part-select `x[off +: w]`,
  uniform for bits (packed) and Vec (unpacked) — no direction branch. infer:
  `slice_literal` offset branch + `sliced_ty` helper; walker: offset branch in
  the Slice arm; predicate: offset shape (walkable base + literal width). This is
  the "fixed-size synthesisable runtime slice" from the original ask.
  `slice_offset.mrn` (`x[i..+4]`→`x[i +: 4]`) verilator-clean, in
  CLEAN+VERILATOR_CLEAN+golden. Next S4: elision defaults, zero-width const-if
  guard, param/const-expr endpoints (const-eval), slice-set.
- 2026-06-25: **S4 step 2 — vec slices.** Extended `slice_literal` to `Vec`
  (low-first: `v[2..5]` → `Vec(3,A)`). The MIR walker Slice arm is now
  type-directed in both endpoint order AND SV range direction: `bits` packed
  `[high-1:low]` (descending), `Vec` unpacked `[0:N-1]` ascending `[low:high-1]`;
  `expr_leaves_mir` Slice is canonical (per base leaf), `expr_value_mir` reduces
  via `one_leaf_mir`. `slice_vec.mrn` (`v[2..5]`→`v[2:4]`) verilator-clean, added
  to CLEAN+VERILATOR_CLEAN+golden. Lib + clean green. Next S4: offset form
  `x[off..+w]`, param/runtime endpoints (`x[i +: w]`), elision, zero-width guard,
  slice-set.
- 2026-06-25: **S4 step 1 — first slice end-to-end (bits, literal, read).**
  `x[8..4]` on `bits` now types to `bits(4)` (infer `slice_bits_literal`,
  high-first, width≥1) and lowers through the MIR walker to the SV part-select
  `x[7:4]` (`expr_value_mir` Slice arm + `mir_lit`; predicate admits a
  literal-endpoint bits slice). `examples/working/slice_bits.mrn` added to
  CLEAN+VERILATOR_CLEAN+golden; the now-passing `fail-expected/slice-not-implemented.mrn`
  removed. The S4 pipeline is proven; wider cases still reject cleanly
  (SliceNotImplemented): vec, offset `..+w`, param/runtime endpoints, elision,
  zero-width (needs the const-if guard), slice-set. Next S4: vec slices + offset
  form + param/runtime endpoints + zero-width guard.
- 2026-06-25: cleanup — dropped the 43 stale `#[allow(dead_code)]` on the MIR
  walker twins (all now live in the call graph; compiles clean). Wrote the S4
  (slicing) implementation plan + the HIR-core-deletion deferred-cleanup note
  (both below). S3 (emission on MIR) is corpus-complete; next MIR work is S4.
- 2026-06-25: **S3.2s — const-fn localparams + `replace`; CORPUS-COMPLETE.**
  Ported the integer/symbolic-const let (localparam promotion via
  `const_rhs_mir`/`emit_const_call_mir`; the const-*function* emission stays on
  its own `build_const_function` path) and `v.replace(i,x)` in `expr_leaves_mir`.
  Predicate widened (Let allows integer/const locals; Builtin allows Replace).
  **Diagnostic: with the predicate forced true (`MIR_FORCE`), the golden gate is
  byte-for-byte green over the whole corpus** — every construct the corpus uses
  now lowers identically through the native MIR walker. Reverted the force hack;
  the predicate-gated path is green too (89), 127 lib.
  Remaining for full HIR-core deletion (untested edges, kept on HIR by the
  predicate): runtime-index *writes* (projected-place bounds-assert), symbolic
  `const if` (generate-if, unbuilt), slice (S4). These never occur in the clean
  corpus. Next: decide HIR-core deletion (handle the edges, or keep the predicate
  as a permanent fallback) — then S4 slicing on the MIR walker. Refactored
  `index_bounds_assert` into a type-taking core + `index_bounds_assert_mir`;
  the Index read arms now emit the bounds-assert, so the static-index restriction
  is lifted for *reads* (`v[sel]` with a uint `sel`). Place *writes* stay
  static-index (the projected-place bounds-assert is the only remaining tail).
  Golden green (89), 127 lib. Next: confirm corpus coverage (how many defs still
  fall to HIR) → then delete the HIR lowering core.
- 2026-06-25: **S3.2q — `const if` folded at lowering.** The `mir_of` lowering
  has the HIR cond id, so it calls `eval_cond` and keeps only the taken branch as
  a `Block` (foldable case) — `ConstIf` disappears from MIR for the common case,
  and the backend walker handles the resulting `Block`. A still-symbolic cond
  (generate-if, not built) keeps the structural `ConstIf` (predicate rejects →
  HIR, as today). Golden green (89), 127 lib, MIR smoke green. const_if.mrn now
  walks MIR. **Only runtime-index bounds-asserts remain before every construct
  lowers on MIR.**
- 2026-06-25: **S3.2p — let-mut fold on MIR.** Added run-consumption to
  `lower_stmts_mir` (`let mut acc` + carrying steps → `lower_mut_fold_mir`, a
  procedural `always_comb`), plus `mir_carries`, `blocking_assigns_mir`,
  `loop_bound_var_mir`. Predicate is now run-aware (`mir_ok_stmts` +
  `mir_ok_fold_step`). Golden green (89), 127 lib. Fold defs (adder_tree,
  fold_sum, …) now lower on MIR. **Remaining: `const if` (const-eval over a MIR
  cond) + runtime-index bounds-asserts — then every construct lowers on MIR and
  the HIR lowering core can be deleted.**
- 2026-06-25: **S3.2o — unit-return call statements + out-args on MIR.** Fixed
  `emit_instance_mir` to wire `Conn::Out` args (out-target leaves via
  `projected_leaves_mir`, type for mono from a bare-local target). Added
  `declare_out_targets_mir` + `lower_call_stmt_mir`; wired `MStmt::Expr` and the
  unit-return branch of `drive_result_mir`. Predicate: `mir_ok_call_stmt` /
  `mir_ok_call_or_noop` / `mir_ok_result_value` (unit tail/return = void call or
  no-op; out-args allowed for call statements, bare-local targets). Golden green
  (89), 127 lib. Void top-modules (module_wrapped, use_across_modules) and
  instance `=> target` connections (stream_connect, dataflow_stage) now lower on
  MIR. Remaining: let-mut fold, `const if`, runtime-index bounds-assert; then
  delete the HIR core.
- 2026-06-25: **S3.2n — records on MIR.** Added `record_leaves_mir` (in-field
  leaves, declared order) + `record_out_conns_mir` (`field => target` via
  `projected_leaves_mir`). Wired `expr_leaves_mir` Record + the record handling
  in `lower_let_mir` / `lower_equation_mir` (bare-local record block) /
  `drive_result_mir`. Predicate: a Record arm (in-fields walkable, out-targets
  walkable places). Golden green (89), 127 lib. Record defs (packet_struct,
  stream_connect, record_out_conn, parametric_struct, …) now lower on MIR.
  Remaining: let-mut fold, `const if`, unit-return call statements,
  runtime-index bounds-assert; then delete the HIR core.
- 2026-06-25: **S3.2m — `when` on MIR (register / RAM).** Added
  `clock_of_event_mir`, `lower_when_mir` (value), `lower_when_leaves_mir`
  (aggregate), `lower_when_stmt_mir` + `when_body_seq_mir` (statement, guarded),
  and the when-RAM branch in `lower_equation_mir` (`mem = when E {…}`). Wired all
  four sites. Predicate: `mir_ok_event` (posedge on a local), `mir_ok_when_body`
  (Equation / guarded `Expr(If)` / let), and a `When` arm in both `mir_ok_stmt`
  (guarded body) and `mir_ok_expr` (normal body, for value/aggregate/RAM). Golden
  green (89), 127 lib. Clocked `when` defs (when_counter, ram, ram_write, …) now
  lower on MIR. Remaining: records, let-mut fold, `const if`, unit-return call
  statements, runtime-index bounds-assert; then delete the HIR core.
- 2026-06-25: **S3.2l — indexing on MIR (place projections + reads).**
  `place_leaves_dir_mir` now handles projected places (`projected_leaves_mir`
  applies Field/Index base→leaf; outermost-projection discriminator mirrors HIR:
  Index→multi-leaf, Field→single-leaf). Wired the `Index` read arm in
  `expr_value_mir`/`expr_leaves_mir`. Predicate: `mir_ok_place` + an `Index` arm,
  both gated on `mir_static_index` (integer/genvar) — runtime (uint) indices stay
  on HIR (no bounds-assert replicated). `v[i] = x`, `s.field = x`, `v[i]` reads,
  and genvar-indexed for-bodies now lower on MIR. Golden green (89), 127 lib.
  Remaining: `when` (register; several sites), when-RAM, `const if`, records,
  let-mut fold, unit-return call statements, runtime-index bounds-assert; then
  delete the HIR core.
- 2026-06-25: **S3.2k — `for` (generate) on MIR.** Added `lower_for_mir`
  (bound from the iterable's MIR-node type; genvar or `assign x = v[i]` binding),
  wired `MStmt::For`, predicate admits a for whose iter + body are walkable.
  Golden green (89), 127 lib. (Indexed-body drives `out[i] = …` still rejected —
  they need place projections, next.) Remaining: `when` (register; several
  sites), when-RAM, `const if`, place projections, records, let-mut fold,
  unit-return call statements; then delete the HIR core.
- 2026-06-25: **S3.2j — `if`/`Block` on MIR.** Added `block_value_mir`/
  `block_leaves_mir`/`expr_leaf_types_mir`/`lower_if_mir`/`lower_if_leaves_mir`;
  wired the `If` and `Block` arms in `expr_value_mir`/`expr_leaves_mir`; predicate
  admits `If` (cond + both blocks walkable) and `Block` via a new `mir_ok_block`.
  Golden byte-for-byte green (89), 127 lib green. Remaining control flow: `when`
  (register; needs `clock_of_event_mir`), `for` (generate), when-RAM, `const if`
  (needs const-eval on the MIR cond), plus place projections, records, let-mut
  fold, unit-return call statements; then delete the HIR core.
- 2026-06-25: **S3.2i — registers on MIR.** Split `emit_registers` into a
  resolved core (`emit_registers_parts`) + `emit_registers_mir`; extracted
  `sv_type_of` and `expr_type_leaves_mir`. Wired the reg branch of `lower_let_mir`
  (typed by D-input), `lower_equation_mir` (typed by target local), and
  `expr_value_mir` (value-position into a fresh `__block_N`). Predicate: dropped
  the `as_reg` rejections from Let/Equation and added a `Builtin::Reg` arm to
  `mir_ok_expr` (D/reset/init must be walkable). Clocked defs (counter, delay,
  accumulator, shift_register, …) now lower through MIR.
- 2026-06-25: **S3.2h — value-position instances on MIR.** Added
  `call_value_leaves_mir` (instantiate into `__call_N`, return leaves), wired the
  non-inline `Call` arm in `expr_value_mir`/`expr_leaves_mir`, and widened the
  predicate's `Call` arm to admit instances (in-only connections). The gate caught
  an over-admission — a unit-return def whose tail is a void call hit
  `drive_result_mir`'s unported branch; fixed by keeping unit-return tails/returns
  on HIR. Golden byte-for-byte green (89), 127 lib green. Nested/value-position
  user calls (e.g. `g(f(x))`) now lower on MIR. (The incremental-validation safety
  net working as intended.) Remaining: reg/when-RAM, when/if/for, place
  projections, records, let-mut fold, unit-return call statements; then delete HIR core.
- 2026-06-25: **S3.2g — instances on MIR.** Refactored `emit_instance` into a
  `CallSlot` (resolved caller leaves + actual type) + an id-agnostic
  `emit_instance_core` (pure extraction, golden-validated identity). Added
  `emit_instance_mir`/`emit_instance_from_mir`/`actual_type_mir` and wired the
  instance branch of `lower_let_mir`/`lower_equation_mir`/`drive_result_mir`.
  Widened the predicate (`mir_ok_value`) to admit top-level (statement-position)
  module-instance calls whose connections are walkable. Golden byte-for-byte
  green (89 cases) + 127 lib — defs with `let x = f(args)` / `place = f(args)` /
  `return f(args)` now lower through MIR. Remaining: value-position instances
  (`call_value_leaves` on MIR), reg/when/if/for, place projections, records,
  let-mut fold — then delete the HIR core.
- 2026-06-25: **S3.2e+f — first validated flip.** Built the statement twins
  (`lower_top_block_mir`/`lower_stmts_mir`/`lower_one_stmt_mir`/`lower_let_mir`/
  `lower_equation_mir`/`drive_result_mir` + `value_leaves_dir_mir`/
  `place_leaves_dir_mir`/`as_reg_mir`/`is_instance_call_mir`), simple paths real,
  complex sub-cases `todo!`. Added a strict coverage predicate
  (`mir_walk_supported`) and flipped `build_module` to walk MIR for covered defs.
  **The whole corpus emits byte-for-byte identical** (golden 89 cases green, 127
  lib green) — `add_constant`-class defs now lower through the native MIR walker,
  the rest stay on HIR. Emission walks MIR for real; the migration is validated
  incrementally as the predicate widens. Next: widen coverage — instances
  (`emit_instance` on MIR, the big one), then reg/when/if/for/projections, then
  delete the HIR lowering core.
- 2026-06-25: S3.2d cont(2) — inline call machinery on MIR. Added
  `resolve_trait_instance_with` (substs-taking, id-agnostic) + `mir_call_target`
  + `render_inline_mir` (prep from a MIR `Call` node, shares `render_inline_spliced`).
  Wired the `Call` arm in both `expr_value_mir` and `expr_leaves_mir`: inline
  callees splice via `render_inline_mir`; non-inline (instance) is `todo!`
  (needs `emit_instance`/`call_value_leaves` on MIR). Dead code; live
  `resolve_trait_instance` refactor is identity — golden green (93s), 127 lib green.
  **Inline calls (operators) now lower on MIR** — the bottleneck arm is done for
  the common case. Next (S3.2e): `lower_stmts_mir` + `drive_result_mir`, then a
  parallel-entry flip of `add_constant` (inline-only) for the first real
  byte-for-byte validation of the walker.
- 2026-06-25: S3.2d cont. — refactored `render_inline` to extract the
  **id-agnostic** `render_inline_spliced(template, val_map, node_subst)` (the
  SV-building half). The HIR path now builds `val_map`/`node_subst` then calls
  it; the MIR path will do the same from a MIR `Call` node, *sharing* the splice
  rather than duplicating it. Pure extraction — golden byte-for-byte green. This
  is the "push id-resolution to the edges" cleanup that shrinks the call-machinery
  port. Next: `render_inline_mir` prep (val_map from Conn args via
  `expr_value_mir`, node_subst from `Call.substs`; trait re-selection via a
  substs-taking `resolve_trait_instance` variant) + wire `expr_value_mir`/
  `expr_leaves_mir` Call arm for the inline case.
- 2026-06-25: S3.2d started — `expr_leaves_mir` (aggregate arms ported off the
  MIR node, reusing id-agnostic `local_leaves`/`strip_field`/`eval_const`;
  Record/Index/Call/control-flow `todo!`) + `one_leaf_mir`; `expr_value_mir`
  aggregate arm now reduces via `one_leaf_mir`. All dead code (golden untouched),
  127 lib green. The big remaining piece is the **call/inline machinery on MIR**
  (`inline_call`/`as_user_call`/`render_inline`/`call_value_leaves`/
  `emit_instance` + `UserCall` carrying MIR data) — that unblocks the Call arms
  in both value/leaves twins and is the bulk of S3.2d. Then `block_leaves_mir`,
  `index_bounds_assert`/`record_leaves` on MIR, then S3.2e statements + flip.

## S4 — slicing on the MIR walker (implementation plan)

Now that emission walks MIR (S3 corpus-complete), slicing rides it. S4 is a
**coupled** feature: do NOT relax infer's `SliceNotImplemented` until emission is
ready, or a slice silently miscompiles (HIR `expr_value` has no Slice arm → `0`;
MIR walker Slice arm → `todo!`). Land infer-typing + MIR/emission together.
Design source: planning/slicing.md (+ mir.md §"Slicing on MIR").

Ordered steps (each golden-gated; add slice examples as they work):
1. **infer typing** (hir/infer.rs, replace `SliceNotImplemented`): base must be
   `bits(N)` (high-first) or `Vec(N,A)` (low-first). Endpoints `lo`/`hi`/`width`
   → const args; `width = high - low` must fold to a **constant** (the one hard
   SV rule); base/offset may be runtime. Result `bits(w)` / `Vec(w,A)`. Enforce
   direction (ascending-bits / descending-vec = error w/ hint). Elision defaults
   (bits `x[hi..]`→`x[hi..0]`, `x[..lo]`→`x[N..lo]`; vec dual). Zero width is
   allowed (not an error).
2. **MIR**: keep `MExprKind::Slice` (already lowered structurally) — handle it in
   the walker, type-directed: `expr_value_mir`/`expr_leaves_mir` Slice arm emits
   the part-select. Compute `(low, width)` from the endpoints (const-eval the
   width). Emit `base[msb:lo]` when `low` is const, else `base[low +: width]`
   (slicing.md table). bits vs vec only differs in which endpoint is low.
3. **zero-width guard**: wrap the part-select primitive in a `const if width > 0`
   (folds at lowering — S3.2q machinery): width 0 → emit nothing / a `[-1:0]`
   effective-0-bit, never a reversed `[lo-1:lo]`. (Concat zero-width guard is
   separate, only if a concat primitive needs it.)
4. **predicate**: admit Slice in `mir_ok_expr` (base walkable, width const).
5. **slice-set** (lvalue `x[a..b] = y`): a `Place` with a `BitRange` projection
   (the `Projection::BitRange` reserved in S2). `place_leaves_dir_mir` emits the
   part-select target; rides the partial-drive completeness machinery
   (`index_uses_forbound`) like a compound index drive. Do LAST (its own step).
6. tests: examples/working slices (bits two-endpoint, offset form, vec, elision,
   zero-width via a parameter at its limit); fail-expected (ascending-bits,
   descending-vec, non-const width). Promote into CLEAN/VERILATOR_CLEAN + golden.

Hardest part: step 1 (const-width derivation + direction). Step 5 (slice-set) is
separable. The whole feature is the payoff of the MIR migration — it lands as one
clean MIR-walker desugar instead of touching two backends.

## HIR-core deletion (deferred cleanup)

The HIR lowering core still coexists with the MIR walker (reached only for
predicate-rejected defs + the const-fn `build_const_function` path, which uses
HIR `expr_value`/`const_stmts`). Deleting it requires: (a) handle the 3 untested
edges in MIR (runtime-index *write* bounds-assert; symbolic `const if` is
unbuilt in both; slice = S4), (b) port the const-fn SV-function builder off HIR
`expr_value`, (c) flip the predicate always-on, (d) delete ~25 HIR methods +
`UserCall`/`RegCall`. Big, mostly untested edges — lower priority than S4. Track
here; tackle after S4 or leave the predicate as a permanent fallback.

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
  [started: `expr_leaves_mir` aggregate arms (Local/Field/VecLit/TupleLit/
  VecRepeat/scalar-fallback) + `one_leaf_mir` done; Record/Index/Call/control-flow
  arms `todo!`. `expr_value_mir` aggregate arm wired to `one_leaf_mir`. Still
  needs: call/inline machinery on MIR (the big piece), `block_leaves_mir`,
  `record_leaves`/`index_bounds_assert` on MIR.]
- S3.2e — `lower_stmts_mir` / `drive_result_mir` (Let/Equation(Place)/When/For,
  `Builtin::Reg` for registers).
- S3.2f — **wire-up**: `lower_top_block` calls the `_mir` twins; delete the HIR
  lowering core and `mir_expr_type`'s inf-fallback becomes native. Golden must
  stay byte-for-byte at the flip. `resolve_trait_instance` + inline stay on
  `crate_def_map`/HIR until S7.
Once emission walks MIR natively, S4 (slice desugar) / S5 / S6 / S7 follow.
