# Open questions — compiler docs

A working list of things that were unclear, provisional, or in flux while writing
the compiler docs. Not part of the book (not in `SUMMARY.md`). The aim is to keep
the docs honest: where the design is unsettled or the code and intent disagree,
the prose says so, and the specifics land here for Jon to resolve.

Each entry: what's unclear, where it shows up, and what the docs currently say.

## Future direction (design not yet settled)

- **Two const evaluators, consolidation intended.** `hir/const_eval.rs` walks the
  HIR; `mir/const_eval.rs` walks the MIR. Both share one value/arith/projection
  core. The intent is to retire the HIR one for the MIR one, but `infer` still
  uses the HIR evaluator (`infer.rs:781`) and the type-level `ConstArg` width axis
  stays on it; `hir/const_eval.rs:17` flags the future. The const-eval chapter
  describes both and names the consolidation as unfinished. — *What exactly must
  move for `infer` to read consts off the MIR? Timing?*

- **MIR→MIR passes not yet built.** Aggregate flatten, monomorphisation, and
  inline still run in `backend/lower.rs`, though MIR was introduced to host them.
  The MIR and backend chapters must describe the *current* split (backend does
  this work) while noting the intended migration. — *Scope/timing unclear.*

- **Domain lifting is provisional.** A pure fn gets domains by appending a
  synthetic `__Dom` param and stamping unannotated slots (`infer.rs:1559`); the
  comment calls this "lenient until the backend stops reading it (Q7 phase D)" —
  the front end is lenient and the backend is the real enforcement point. The
  inference chapter flags lifting as provisional. — *When does enforcement move
  fully to the front end?*

- **Trait impls are domain-blind.** Header matching ignores domains
  (`infer.rs`/`types.rs:275`); the clock flows through the resolved method's
  signature instead. Stated in the traits chapter. — *Is domain-blind dispatch
  the permanent design, or a v1 simplification?*

## Known-incomplete features (deferred, expected to land)

These are real today as diagnostics/guards; the docs should not present them as
finished. Most belong in the user reference and the MIR/backend chapters.

- **Slicing** — only literal endpoints on `bits`/`Vec` are handled; non-literal
  endpoints are rejected (`infer.rs:54`, `2114`).
- **`const if` with a symbolic condition — gate vs. capability may be out of
  sync.** The front end (`infer.rs:859`) rejects a symbolic `const if` in an
  ordinary (non-inline) fn, accepting it only in inline bodies (where it grounds
  at the splice). But the backend *does* emit `generate if` for a symbolic
  condition (`backend/lower.rs:~2546`) — and a comment near there (`~2219`) still
  calls generate-if "not yet built" while the code emits it. So the non-inline
  rejection and the backend capability disagree. — *Is the rejection still
  needed, or can ordinary fns allow it now?*
- **Non-`reg` builtins in scalar position** — `replace`/`enumerate` used in scalar
  (value) position hit a `todo!` (`backend/lower.rs:2192`).
- **Named-argument defaults** — only scalar defaults are implemented; multi-leaf
  and broadcast defaults are marked `TODO(named-args)` (`backend/lower.rs:3044`,
  `3459`, `3475`).
- **Inline-body scope** — `#[inline]` rejects clocked state, `var`, `out` params,
  integer params, and recursion (`check.rs:785+`). v1 splices only combinational,
  value-returning bodies.
- **`for` over const vecs** — only `range(n)` drives the genvar; other const vecs
  are rejected (`infer.rs:1748`).
- **Port-field completeness / direction pairing** — deferred to flatten-time;
  per-def checks can't resolve which leaves a partially-driven port owes
  (`check.rs:9`, `425`).
- **Inline recursion guard is narrow** — `inline_check`'s recursion check follows
  only `Call{Def}` edges, not method-dispatched inline callees, which are left to
  the backend's depth-guard backstop (`check.rs:948`). The checks chapter says
  "rejects recursion" without this caveat — fine for now, but note it here.

## Known gap (a constraint that escapes both checks)

- **Compound symbolic width residuals are dropped.** The backend's `initial
  assert` fallback fires only for bare-`Param` (and `Param`-vs-`Param`) width and
  fit residuals (`backend/lower.rs:216`); `mono_check` only decides *ground* ones.
  A residual that is neither — a compound symbolic width like `uint(n+1)` vs
  `uint(m)` that never grounds — is checked by neither path (a Q4c gap). The
  monomorphisation chapter says this plainly; closing it is open work.

## To state plainly (verified, just needs saying)

- **`mono_check` does not gate emission.** It reports ground-instance diagnostics
  alongside the front end but does not block `sv_file`; symbolic cases fall back
  to a runtime `initial assert` (per `ir_pipeline.md`). The MIR/backend chapters
  should say this.

## Code vs. planning (docs may predate MIR)

- Several `planning/` docs predate the MIR stage and may describe a pre-MIR design
  MIR now supersedes or reshapes. Treat the code as primary; note divergences
  rather than copying. — *Flag specific ones here as they surface.*
