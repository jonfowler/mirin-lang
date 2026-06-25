# Monomorphisation-time checking (`mono_check`)

> **Status: first slice BUILT (2026-06-25); scaling design not yet.** The ground
> direct-call check is live in `backend/mono_check.rs` (the `mono_check(krate)`
> query) — a literal-arg call that grounds a callee residual false is now a
> compile-time diagnostic. The scaling design below (assertion maps, support
> factoring, family dedup) and cross-module composition are the *destination*,
> not yet built. **See "## Implementation plan" at the end** for what landed and
> what remains.

## Why it exists

The HIR checks are deliberately *loose* on anything that depends on const
parameters (`planning/parametricity.md`). Widths emit **symbolically**
(`w`, `(w-8)`, `sum_to(N)`); drive completeness on a parametric-length port is
partial; precise integer math is left to the SV elaborator. `infer` records the
unproven obligations as `const_residuals`, and `sv_module` drops width residuals
as `initial assert`. This keeps the type layer simple and total — it never has
to decide undecidable const arithmetic — at the cost of *under-checking at the
polymorphic boundary*.

`mono_check` is where that debt is paid: once a module is instantiated with
**ground** consts (every param a literal — via a literal call arg or a top-level
`-G` override), every residual becomes a concrete predicate we can simply
evaluate. Width positivity, non-overflow, exact completeness on now-known
lengths, and surviving `const_residuals` all decide true/false here, *per
instantiation*, so a bad `-G N=0` is caught at the instantiation rather than
surfacing as a cryptic elaborator error or a silent `initial assert` at sim
time.

The hazard this doc is really about is **cost**. Naively, "check every residual
at every concrete instantiation" unrolls the whole generate tree:

```mirin
for i in range(M) {
  for j in range(N) {
    foo(i, j)
  }
}
```

`foo` has some assertions; checking each of the `M·N` instances of `foo`
against all of them is `O(M · N · |assertions|)`, and it nests with loop depth.
That is the explosion to avoid.

## Core object: the per-def assertion map (module summary)

Each def exports an **assertion map** — a salsa-cached summary, expressed in
terms of *that def's own const params*, of everything that must hold for an
instantiation to be legal. This is a function summary in the
interprocedural-analysis sense: it lets a caller account for a callee's
obligations without re-walking the callee's body.

An entry is a **quantified assertion**:

```
∀ (x₁ ∈ R₁, …, xₖ ∈ Rₖ) .  P(params, x₁, …, xₖ)
```

- `P` is a predicate over the def's const params and some bound loop variables
  — e.g. `width(params, i) ≥ 1`, `lhs_leaves(i) == rhs_leaves(i)`, or a residual
  trait/where obligation.
- Each `xₘ` is a loop variable introduced by a `for xₘ in range(Rₘ)` that
  encloses the obligation, with range expression `Rₘ` over params (and outer
  bound vars).
- The entry carries its **support**: `supp(P) = ` the set of bound vars `P`
  actually mentions. This is the whole game — see below.

The kinds of `P` we summarise:

| Assertion | Source today |
|---|---|
| width strictly positive (`w ≥ 1`) and non-overflowing | `infer` neg-width reject + symbolic widths |
| drive completeness on a concrete-length port / vec | `completeness` (deferred for parametric lengths) |
| surviving const obligation | `const_residuals` |

## The key idea: factor by support

Checking cost for one quantified assertion is the product of the ranges of the
variables **in its support**, not of all enclosing loops:

```
cost(P) = ∏_{xₘ ∈ supp(P)} |Rₘ|
```

Variables an assertion does not mention collapse to a single representative
(any value; pick the range endpoints — see *Future*). So for the double loop,
partition `foo`'s assertions by support:

- `{i}`-dependent (a of them) → `a · M` checks total
- `{j}`-dependent (b of them) → `b · N` checks total
- `{}`-dependent / constant (c) → `c` checks
- `{i, j}`-dependent (d of them) → `d · M · N` checks — **the fallback**

Total `≈ aM + bN + c + dMN`. The design's bet, which the user's example states,
is that `d ≈ 0`: an obligation that genuinely couples *both* loop indices is
rare. We do not forbid it — we *isolate* it, so only the coupled assertions pay
the product, and the common single-axis ones stay linear.

This is the same move as rustc **polymorphization** (detect which generic params
actually affect a body, and avoid specialising on the rest): we compute, per
*assertion*, which loop axes actually affect it, and avoid enumerating the rest.

## Cross-module composition

The double loop need not live in one module — the inner loop is often a separate
module taking `N` and `i`:

```mirin
fn outer {const M, const N} () { for i in range(M) { inner(N, i) } }
fn inner {const N, const i} () { for j in range(N) { foo(i, j) } }
```

So the summary must **compose** bottom-up. To build `assertion_map(D)`:

1. Start with `D`'s own quantified assertions (its widths, completeness,
   residuals), supports computed over `D`'s loop vars.
2. For each call `C(args)` in `D` (possibly inside `D`'s loops), take
   `assertion_map(C)` and **substitute** `C`'s formal params with `args`. Each
   resulting predicate is re-expressed in `D`'s frame.
3. If the call sits inside `for x in range(R)` in `D`, wrap the imported
   assertions in `∀ x ∈ R`. A callee param bound to `x` (e.g. `inner`'s `i`)
   thus turns a callee-frame param into a `D`-frame **bound var**, and its
   support picks up `x`.
4. Recompute each entry's support **after** substitution. Two collapses happen
   naturally:
   - an arg that is a literal (`inner(N, 0)`) partially-evaluates `P` and drops
     that axis from the support;
   - an arg that is another param threads the support through unchanged.

The result is that `outer`, fully ground, sees `foo`'s `{i}`-assertions as
`∀ i∈M` (cost `M`), `foo`'s `{j}`-assertions as `∀ i∈M ∀ j∈N` but with support
`{j}` after factoring (cost… see below), etc. — the cross-module case factors
exactly like the single-module one.

> Subtlety: a callee `{j}`-only assertion imported under an outer `∀ i∈M` has
> support `{j}`, but it is *replicated* `M` times by the `i` quantifier. If `j`'s
> range does not depend on `i`, the replicas are identical and **dedup to one**
> (next section). If `j`'s range *does* depend on `i` (`range(g(i))`), the
> support genuinely includes `i` and we pay the product — correctly.

## Checking regimes

- **Ground** (every param in scope a literal): all `Rₘ` are concrete; run the
  factored enumeration above and decide each `P` by `const_eval`. This is the
  precise check `mono_check` is for.
- **Symbolic** (param still parametric, module emitted as `#(.N())`): ranges are
  not concrete; we cannot enumerate. Keep the status quo — emit the residual as
  `initial assert` (elaboration/sim-time), or, later, discharge it with the
  range/monotonicity reasoning below. The summary is still *built* symbolically
  so that a ground caller can discharge it.

## Caching

Three layers, smallest-blast-radius first:

1. **Summary query.** `assertion_map(def)` is a salsa query — recomputed only
   when the def (or a callee's summary) changes. Editing one module does not
   re-derive the whole tree's obligations.
2. **Family dedup.** A module instantiated many times with the *same* symbolic
   relationship contributes *one* quantified assertion, checked once — the
   analog of rustc deduplicating mono items in the collector. Dedup key is the
   substituted predicate + range expressions, modulo bound-var renaming.
3. **Point memoisation.** Evaluating a ground `P` at a support tuple is
   memoised by `(predicate-id, support-values)`, so identical points across
   replicas (the `∀ i∈M` dedup above, and shared subterms) collapse.

## Placement in the pipeline

A backend-side pass, after monomorphisation, consuming:

- `const_residuals` + width obligations from `infer`,
- the per-call const instantiations `sv_module` already records (rustc node
  substs), which give the instantiation tree and the loop ranges.

It is the principled replacement for today's "width residual → `initial assert`"
line in `sv_module`: when an instantiation is ground, `mono_check` decides the
residual and a failure is a real diagnostic; when symbolic, emission of the
`initial assert` (or a proof) remains the fallback.

## Open questions

- **Keying.** Is the ground check a salsa query keyed by concrete instantiation
  (fed by the mono collector), or a single walk over the crate's reachable
  ground roots? The former caches better; the latter is simpler to report from.
- **Reporting.** A failure needs to name the instantiation path
  (`outer{M=4,N=0} → inner → foo`), not just the leaf def. The summary should
  carry enough provenance to reconstruct that without re-walking.
- **Support computation.** Computing `supp(P)` precisely (not conservatively
  "all enclosing vars") is what buys the factoring; it needs the predicate in a
  normal form where free-variable extraction is exact after substitution.

## Future sharpening

- **Range reasoning instead of enumeration.** Most width/positivity predicates
  are *monotone* in their index (`w = base + k·i ≥ 1`). For those, checking the
  range **endpoints** discharges the whole `∀ x ∈ R` without enumerating `R` at
  all — turning the `{i}`-assertion cost from `M` to `O(1)`. Enumeration is then
  only the fallback for non-monotone predicates.
- **Symbolic discharge.** With endpoint/interval reasoning over symbolic ranges,
  some obligations can be proven for *all* `N` at summary time, removing the
  `initial assert` entirely rather than deferring it.

## Prior art

Function summaries (interprocedural dataflow); rustc **polymorphization**
(per-body unused-generic detection) as the analog of per-assertion support
factoring; rustc's **collector** mono-item dedup as the analog of family dedup;
rustc post-mono errors (e.g. transmute size mismatch) as the analog of a
mono-time, per-instantiation hard check. See `planning/parametricity.md` for the
const-kind-stays-parametric split this pass sits on top of, and
`planning/const_eval.md` for the evaluator it calls to decide ground predicates.

## Implementation plan

### What rustc actually does (and what we already have)

Researching the rustc analogy (the `mir.md` S6 sketch leans on it) clarified the
shape — and that most of the *structure* already exists in `backend/lower.rs`:

| rustc | rustc role | Mirin today (`backend/lower.rs`) |
|---|---|---|
| monomorphization **collector** (`collect_and_partition_mono_items`) | walk roots → transitive `Instance` set, **deduped** | the `MonoReq` **worklist** in `sv_file`: pop a req, `build_module(callee, subst)`, push its callees; `seen: HashSet<name>` dedups |
| `Instance` = `DefId` + `substs` | a monomorphic item | `MonoReq { callee, subst, name }`; `mono_name` is the dedup key |
| `instantiate_mir_and_normalize_erasing_regions` | **lazy** subst on read — MIR is *not* cloned per instance | `ground_widths(db, krate, def, subst_type(ty, self_subst))` at ~20 read sites — same lazy-on-read model, applied to `Type`/`ConstArg` |
| post-mono errors (`PostMonoError`, transmute size, `assert`) | per-instantiation hard checks once types are concrete | **missing** — this is what `mono_check` adds |
| `mir_for_ctfe` / `const_eval_resolve` | const-eval as a query, callable post-mono | `const_eval::eval_const/eval_cond(db, krate, def, …)` — already callable at emit |

So the big takeaway from rustc — *keep one polymorphic MIR, substitute lazily on
read, never clone per instance* — is **already how Mirin emits**. S6 is therefore
**not a structural rewrite**. It is two additive moves:

1. **Add the post-mono check** (`mono_check`) — the genuinely missing piece.
2. *Later*, consolidate the ~20 scattered `ground_widths`/`eval_cond` read-sites
   into the single specialise-and-check entry `mir.md` envisions. This is a
   readability/locality win, not new capability — do it **after** (1) is green,
   and only if it earns its churn. Sequencing it first would be a big-bang.

### First slice: the naive ground check — LANDED (`backend/mono_check.rs`)

The precise check the doc is about is **the ground regime** (every const param in
scope a literal). This landed naively (no assertion-map factoring yet):

- A salsa query `mono_check(krate) -> Vec<MonoDiagnostic>`. **Refinement on the
  original plan:** it does *not* reuse `sv_file`'s `MonoReq` worklist. That
  worklist only collects *type*-generic copies; a *const*-generic fn emits **one**
  parametric module (`#(parameter int N)`) and grounds only at its **call sites**
  (`#(.N(8))`). So `mono_check` instead **walks MIR call sites** — every
  `MExprKind::Call { callee, substs }` in every def's `mir_of` — which is exactly
  the per-call instantiation the doc wanted, with a precise call-site span and no
  worklist needed. (`MExpr.span` gives the diagnostic location.)
- For each call, substitute the callee's residuals with the call's `substs`
  (callee generic-param order — the index `ConstArg::Param(i)` uses) and evaluate
  with `eval_const`:
  - `const_residuals` (`n == m`) → ground both sides, compare; unequal ⇒
    diagnostic.
  - `fit_residuals` (`value` fits `width` bits) → ground the width, check the fit.
  - **width positivity** → collect the width/length `ConstArg`s from the callee's
    signature (param + return types, nested through `Vec`/`Tuple`/`Port` args via a
    `Folder`), substitute, and flag any that grounds `< 1` (e.g. a `uint(n - m)`
    return with n=4, m=8 → -4, an invalid SV range). infer leaves the subtraction
    symbolic in the callee frame and does *not* reject it at the call, so this is a
    real gap mono_check closes (verified — it was reaching verilator otherwise).
    Struct/port *field* widths are not walked here yet (they resolve elsewhere).
- A residual/width that stays **symbolic** after subst (a non-literal arg) simply
  does not ground, so it does not fire — the existing `initial assert` fallback in
  `build_module` still guards equality residuals. Negative space: no silent pass.

Scope of this slice: **direct** call sites only. A *transitive* obligation (the
callee calls another generic with the caller's param) grounds only when that inner
call is itself literal — cross-module **composition** (substituting the callee's
summary into the caller's frame, per "Cross-module composition" above) is the next
step, not yet built.

Reported via `main.rs`'s `collect_diagnostics`, gated on a clean front end (an
ill-typed body's residuals would cascade). Tested by
`mono_check_decides_ground_residuals` (ground-false diagnoses, ground-true and
symbolic do not) + `examples/fail-expected/mono-width-mismatch.mrn`; folded into
the fail-expected and CLEAN ratchets via `mono_diag_count`.

This is `O(call sites × |obligations|)` — the explosion the scaling design avoids
— but it is *correct*, small, and the foundation the assertion-map/support-
factoring (above) later optimises **without changing the diagnostics**.

### Two decisions this resolves (the doc's open questions)

- **Keying / placement.** Settled on **a single walk** (`mono_check(krate)`), not
  a per-instantiation query — the call-site walk centralises enumeration and gives
  a precise per-call span. Promote to a per-instance query only if caching demands
  it (the doc's "smallest-blast-radius" caching is the destination, not the
  on-ramp).
- **Where mono diagnostics surface.** A new query, **separate from
  `crate_emittable`**: front-end diagnostics still gate emission; `mono_check`
  diagnostics are reported alongside them but do **not** block `sv_file`. Wired
  into `main.rs`'s `collect_diagnostics` and `mirin-lsp`'s `semantic::diagnostics`
  (both gated on a clean front end to avoid cascade).

### Test on-ramp

A `fail-expected` example with a ground instantiation that violates a width
(e.g. a call passing a literal that makes `uint(n-m)` non-positive, or two ports
whose widths must match but are instantiated unequal), asserting `mono_check`
emits the diagnostic naming the instantiation. A `working` counterpart with the
same shape instantiated *validly* asserts no diagnostic. These pin the ground
regime before any factoring lands.
