# MIR — a typed mid-level IR

> **Status: design, not built.** This proposes introducing a mid-level IR between
> the HIR (`body` + `infer`) and SystemVerilog emission. It is foundational, so
> it gets its own doc and a planning-reviewer pass before any commitment.

## Why

Today `backend/lower.rs` reads the HIR body **plus the per-def, `ExprId`-keyed
inference side-table** directly, and does flattening, monomorphisation, and
emission against that. That is the right shape for *inference* (it matches
rust-analyzer: a stable `ExprId`-arena `body`, a separate `infer` query) but the
wrong place for *transforms*. The friction has now shown up from several
directions at once:

- **inline** (planning/inline_bodies.md) — a transform that wants to splice one
  def's body into another; awkward when it must juggle two per-def side-tables
  keyed by two `ExprId` spaces.
- **slicing** (planning/slicing.md) — wants a *type-directed* desugar of
  `x[a..b]` to a part-select primitive, at a point that has types.
- **mono / mono_check** (planning/mono_check.md) — want "instantiate with the
  recorded substs, then check the ground result."
- **`ConstArg`** — already a hand-rolled *proto-MIR for the const fragment*
  (a structured const-IR, not an `ExprId` lookup), built precisely because the
  backend needs to substitute and evaluate widths.

The unifying move is a typed IR where **types ride on the nodes** (baked from
inference once), so these transforms become local and uniform. This is rustc's
HIR→MIR step, and GHC's elaboration to typed Core: inference produces a
side-table; you then *lower* to a typed IR for the downstream work.

**A backend is not the justification.** rust-analyzer — also salsa-based, also
"live off the inference side-table" — built a MIR (`hir-ty/src/mir`) with *no
codegen backend*, for const-eval, borrow-check-style diagnostics, and closure
capture. The analysis payoff alone carries it. Mirin's "backend" (SV emission)
is a bonus consumer, not the reason.

## Shape and placement

`base → syntax → nameres → hir → **mir** → backend`.

- **HIR stays the stable, types-off input.** The `body` arena and the `infer`
  side-table are unchanged. Keeping types *off* the body is deliberate
  incrementality (salsa): editing a type must not dirty the body arena.
- **MIR is a derived, per-def salsa query, types-on.** `mir(def)` reads `body` +
  `infer` and bakes the resolved types into new nodes. Because it is *derived*
  (rebuilt, not an input), embedding types in it costs nothing incrementally —
  best of both, exactly as rustc keeps `TypeckResults` separate from MIR.
- No new HIR, no re-inference: lowering switches *which* representation carries
  types, it does not recompute them.

## The const story (the spine)

This is the part the design discussion converged on; state it once and build the
rest on it.

**Principle.** *Const-eval is deferred, per-item, and query-based. Inference
stays structural. Const **requirements** are gated on dispatch. Candidate const
sites may be over-approximated freely — false positives are free, false
negatives cannot occur — so neither dependent typing nor const-value
specialization has to be ruled out.*

Unpacking it:

- **Inference is structural.** Mirin consts are width/length *attributes* on
  leaves — they never select a different type *constructor* (no const-dependent
  type structure; `const if` is value-level, both arms unify to one type). So
  unification proceeds with consts as symbolic holes and const facts as deferred
  obligations (OutsideIn(X) with X = the const constraints). Inference never has
  to decide const arithmetic to produce a correct type.

- **Const-eval is one evaluator, per-item, query-based** — the Miri/rustc model
  (`const_eval` is a query; `const_eval_resolve_for_typeck` calls it *during*
  typeck). Each const site is its own evaluable unit (an "anon-const"), so
  evaluating it never goes through `mir(self)` — no self-cycle. `ConstArg` is
  already the lightweight version of this for widths; the question (below) is
  whether to subsume it or keep it as a projection of MIR const-operands.

- **Candidate-finding feeds dispatch, not deferral.** A call-arg's const-ness is
  *dispatch-determined* (you don't know `replicate{N}`'s `N` is const until you
  resolve which `replicate`). So: over-approximate — across all methods matching
  (name, arity[, known receiver head]), mark every position *some* candidate
  makes const as a const-eval candidate, and form it as a per-item unit. This is
  a **sound over-approximation**: the method that actually resolves is, by
  construction, in the candidate set, so every genuine const position is covered
  (no false negatives); spurious candidates are dropped once dispatch resolves
  the position to a runtime value (false positives cost only tracking, never
  soundness). Precision (tightening with known type info) is a performance/UX
  knob, not a correctness one.

  Two care-points: (1) a candidate is a *tag / const-eval view* over the
  already-inferred expression, **not** a duplicate inference root — infer the
  expr once as a value, evaluate conditionally; (2) dropping a spurious candidate
  must discard its obligation cleanly (key the "must be const + evaluate"
  obligation to the *resolved* position).

So const-eval runs **during infer** (per-item MIR eval of the formed units,
driving dispatch and grounding what it can) *and* **after** (the
specialise-and-check pass below) — one evaluator core, invoked at two phases.
The during-infer invocation is sound because the units are separate items, not
`mir(self)`.

## Dispatch

`owner_of` already keys dispatch on the type **head** (`uint`/`bits`/`Vec`/
`Tuple`/`<port-def>`) plus structural tuple arity — never on a const *value*. So
the common cases (a generic impl `impl {const n} … for uint(n)`, a single
`impl … for Vec(N, A)`) resolve **structurally**, with `N`/`A` symbolic, needing
no const-eval.

We do **not** forbid value-specialized impls (`impl … for uint(8)`) or
dependent-style dispatch. Instead the boundary falls out of typeability (cf. the
Rust analogue `impl SomeType<8>` / `impl SomeType<16>`, which is an `E0599` at a
`SomeType<N>` caller):

- **Concrete const at the call** (literal, or evaluates to one like `uint(4+4)`)
  — evaluate it *during infer* (per-item) and select the matching impl. Const-eval
  drives dispatch, as in rustc.
- **Single generic impl** — resolves structurally on the head; no value needed.
- **Generic const + differing value-specialized candidates + no generic
  fallback** — the **one principled error**: the call cannot be typed (you can't
  know which signature `foo` has without the value, and you can't know a generic
  value), and deferring to mono can't fix that. This is the exact Rust outcome,
  surfaced as a clean diagnostic.
- **Genuinely cyclic dependent dispatch** (dispatch needs a const that needs that
  dispatch) — a salsa **cycle error**, not a hang.

So dispatch is "as general as possible": structural by default, value-driven when
the const is concrete, and erroring only where a value-specialized choice is
needed but the value is irreducibly generic.

## Slicing on MIR

Reconciles planning/slicing.md and planning/comptime_if.md:

- **Read — opaque to the backend.** `x[a..b]` desugars *during HIR→MIR lowering*
  (the first stage that has types) into a part-select primitive — bits-high-first
  vs vec-low-first is a type-directed choice, so it must happen here, not at HIR
  lowering. MIR has no "slice" node. The zero-width guard rides a `const if`
  (already a construct) emitted around the primitive at desugar time. Slicing's
  whole footprint becomes: one type-directed desugar rule + a primitive.
- **Set — a place projection.** A slice-set assigns to a *place* with a bit-range
  projection (rustc MIR `ProjectionElem`). The completeness/driver checker reasons
  over places-with-projections ("drives bits [lo, lo+w) of x") — strictly cleaner
  than the current `index_uses_forbound` HIR pattern-match. Special **only** in
  the checker; emission is the same primitive.

So the guards are MIR/backend-synthesised (where the bounds are in hand), **not**
delegated to inline-Mirin primitives — which supersedes the open tension noted in
both docs.

## Monomorphisation and mono_check on MIR

Mono becomes **"apply the recorded substs to the MIR"** — the "trivial
specialisation" pass: infer already fixed the *types* (the call substs are
recorded), so specialisation substitutes *knowns* (types and consts) and grounds
the const holes; no new type variables are invented. Folded into the same pass is
the ground-regime check of planning/mono_check.md (evaluate now-concrete
residuals, width positivity, completeness on known lengths, `const if` folding).

This **single specialise-and-check pass over MIR is the one home of the const
evaluator and obligation discharge**, retiring today's scattered `ground_widths`
/ `check_widths` / `eval_const_cond` calls. mono_check's hard core (per-def
assertion-map summaries, support factoring, family dedup) is IR-agnostic and
unchanged — MIR is a cleaner *substrate* to build the summaries from (uniform
calls/loops/consts/types), and makes each ground per-instantiation check trivial;
it is an enabler, not a replacement for the factoring.

**On-ramp:** the backend already has the rustc-shaped bones — a deduped mono
**collector** (the `MonoReq` worklist in `sv_file`) and **lazy-on-read**
substitution (`ground_widths`, never cloning MIR per instance). So S6 is additive,
not a rewrite: add the post-mono **check** first (the naive ground regime),
*then* optionally consolidate the grounding calls. The executable first slice —
`mono_check(krate)` reusing the worklist, the two architecture decisions it
settles, and the test on-ramp — is in `planning/mono_check.md` ("Implementation
plan"). The retarget-grounding consolidation is the readability win that follows.

## Inline on MIR

Inlining becomes an ordinary MIR transform (the rustc Integrator: clone the
callee MIR, substitute, remap locals/names, splice) — the natural home it lacked.
The sub-lowering scheme in planning/inline_bodies.md was the "no MIR yet" path;
with a MIR it is the standard inline, and the const-folding-of-a-spliced-`const
if` falls out of the same per-item const-eval used everywhere else. inline_bodies
becomes a section of this, not a separate mechanism.

## Relationship to `ConstArg` and const_eval.md

`ConstArg` (`Lit/Param/Op/Field/Assoc/Local`) is the existing proto-MIR for
consts. Two coherent options, to settle in const_eval.md:

1. **Keep it** as a lightweight const-IR and treat it as a *projection of MIR
   const-operands* — the during-infer evaluator runs on `ConstArg`, the MIR
   evaluator shares the same arithmetic core. Less churn.
2. **Subsume it** — in-type and in-arg consts become anon-const MIR items
   uniformly (full rustc). More uniform, more machinery; the `ConstArg::Local`
   duality case (a const that reaches into the enclosing body — Mirin's
   const/net duality) is the one that resists this and needs a decision.

Either way it is **one evaluator core**; (1) vs (2) is a uniformity-vs-weight
call, not a soundness one.

## Incremental migration (do not big-bang)

1. Introduce `mir(def)` + HIR→MIR lowering, types baked in; nothing consumes it
   yet.
2. Make emission read MIR instead of HIR+side-table (parity gate against the
   current backend and `mirin-compiler-old` oracle).
3. Move passes onto MIR one at a time: slice desugar → flatten → mono+mono_check
   → inline.
4. Keep the during-infer `ConstArg` path throughout; revisit subsume-vs-keep last.

Update planning/ir_pipeline.md with the new stage **only as each piece lands**,
not up front.

## Open questions (for the reviewer)

- **ConstArg: subsume or keep?** (above) — and the fate of the `ConstArg::Local`
  const/net duality under subsumption.
- **Anon-const granularity:** per-item DefId-style units (rustc — uniform eval,
  crisp per-item cycle errors) vs body-internal inference roots (rust-analyzer —
  fewer keys, inner-const inference piggybacks). For an HDL where a const cycle
  should be a crisp localized diagnostic, the rustc model looks the better fit,
  and `ConstArg` already leans that way.
- **How much dependent dispatch to actually support** now vs the minimal
  structural + concrete-const-driven subset (the rest erroring as above).
- **Where slice-set completeness lives** on MIR (the projection-aware driver
  check) and how it composes with the existing partial-drive machinery.

## Prior art

rustc HIR→MIR (`TypeckResults` side-table → typed MIR), `mir_for_ctfe` +
`const_eval_resolve_for_typeck` (const-eval as a query callable during typeck),
the MIR inliner's Integrator, post-mono checks. rust-analyzer's `hir-ty/src/mir`
(a MIR with no backend, for const-eval/borrowck/closures). GHC elaboration to
typed Core (System FC, Core Lint). See planning/mono_check.md, planning/slicing.md,
planning/comptime_if.md, planning/inline_bodies.md, planning/parametricity.md,
and planning/const_eval.md for the pieces this unifies.
