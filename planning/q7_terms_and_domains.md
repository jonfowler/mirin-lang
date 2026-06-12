# Q7 — unified terms, then domain checking

The implementation slice that brings `planning/domain_checking_redux.md` into the
query-based compiler. It is deliberately two jobs in one doc, in order: first a
**representation refactor** (collapse the three inference-variable buckets into
one kinded term language), then **domain checking** built on top of it. The
refactor is not a detour — every piece of the domain design lands more simply on
the unified representation, and so does everything after it (const eval,
struct-valued config params).

(This repurposes the `Q7` slot from `query_engine.md` §8 — the old "Q7
(deferred)" entry was thin-store red-green work that salsa already provides.)

## 1. Why the representation refactor comes first

`hir/types.rs` + `hir/infer.rs` currently run three parallel vocabularies:

| | type | const (width) | domain |
|---|---|---|---|
| term | `Type` | `ConstArg` | `Domain` |
| variable | `Type::Infer(u32)` | `ConstArg::Infer(u32)` | `Domain::Infer(u32)` |
| pool | `type_vars` | `const_vars` | `domain_vars` |
| resolve | `resolve_ty` | `resolve_const` | `resolve_domain` |
| unify | `unify`/`unify_kind` | `unify_width` | `unify_domain` |
| subst | `substitute`/`subst_kind` | `subst_const` | `subst_domain` |
| deep-resolve | `deep_resolve` | `deep_resolve_const` | `resolve_domain_default` |

Three near-identical families, plus `GenericArg`'s cross-kind dispatch arms.
Tolerable at today's size; every upcoming feature multiplies it:

- **Const eval (Q4c)** must evaluate widths, struct-valued configs, and (later)
  delay indices — one evaluator over one const representation, not per-bucket.
- **Struct-valued type parameters** (`uint(cfg.bit_size)`): a const whose type
  is a user struct, projected in type position. Needs consts that *carry their
  type* — impossible while a width is a bare `ConstArg` with no type.
- **The domain term language** (redux doc, future work): `Delayed(clk, n)`
  embeds a **const inside a domain**. With separate variable spaces, every
  cross-kind structure needs glue; with one space it's just a term.
- **`T @ D` obligations** relate a type variable and a domain term — again
  cross-kind.

So: merge, and annotate each variable with the kind of thing it is. Domains stay
in the merged space (the "maybe domains stay separate" option loses exactly the
`Delayed(c, n)` and `T @ D` cases that motivate the merge).

### Prior art (verified)

chalk — the solver lineage rust-analyzer used — is this design exactly:

- `chalk_ir::InferenceVar` is **one u32 index space** for type, lifetime, and
  const variables.
- `chalk_ir::VariableKind` = `Ty(TyVariableKind) | Lifetime | Const(Ty)` — the
  kind annotation, and **a const variable carries its type**.
- `chalk_ir::ConstData { ty: Ty, value: ConstValue }` — consts carry their type.
- `chalk_ir::GenericArgData` = `Ty | Lifetime | Const` — the uniform union used
  in substitutions.
- `chalk_solve::infer::InferenceTable` is **one ena union-find table** whose
  binding value is `InferenceValue::Bound(GenericArg)` — kind-agnostic storage,
  kind-specific views on top.

rustc is uniform at the substitution layer (`GenericArg`, `Term`) but keeps
per-kind tables inside `InferCtxt`; chalk's single table is the simpler design
and the one we follow. rustc's valtrees / `adt_const_params` are the model for
struct-valued consts when they land.

## 2. Target representation (`hir/types.rs`)

```rust
/// One inference-variable space (chalk's InferenceVar).
struct InferVar(u32);

/// The uniform term: what a generic arg is, what a variable binds to.
enum Term<'db> { Type(Type<'db>), Const(Const<'db>), Domain(Domain<'db>) }

/// Kind annotation on a variable / generic param (chalk's VariableKind).
enum TermKind<'db> {
    Type,
    Const(Type<'db>),    // a const var knows its type: usize today, Config later
    Domain(DomainSort),  // a domain var knows its sort
}

enum DomainSort { Domain, Clock }   // Clock ⊑ Domain; @const inhabits only Domain
```

- **`Type`**: as today, with `Infer(u32)` → `Infer(InferVar)`.
- **`Const` replaces `ConstArg`** and carries its type:
  ```rust
  struct Const<'db> { ty: Type<'db>, kind: ConstKind<'db> }
  enum ConstKind<'db> {
      Lit(u64),               // grows a structured Value payload with valtrees
      Param(u32),
      Infer(InferVar),
      Unevaluated(/* body ref */),  // arithmetic / anon-const, for Q4c const_eval
      Error,
  }
  ```
  **`Deferred` dies.** It is a silent-loss hole — anything symbolic unifies
  leniently, which is exactly how `uint(n)` with `n` a clocked local compiles
  today. Symbolic widths become `Unevaluated` + an equality **obligation**;
  undecided is recorded, never discarded.
- **`Domain` becomes a term** with room for constructors (the redux doc's term
  language):
  ```rust
  enum Domain<'db> {
      Const,
      Clock(ClockRef),        // today: a LocalId binding
      Param(u32),
      Infer(InferVar),
      Error,
      // later, additively: WithReset(Box<Domain>, ResetRef),
      //                    Delayed(Box<Domain>, Const<'db>)  ← const inside a domain
  }
  ```
  **`Unspecified` dies.** A missing `@` is a *surface* fact, not a semantic
  domain; today it leaks into the backend (flatten stamps it; `subst_domain`
  mints fresh vars mid-substitution). It is resolved at lowering: `sig_of`
  turns it into lifting (§4.1), body lowering turns it into a fresh var.
- **`GenericArg` = `Term`** (drop the separate enum). `GenericParam.kind`
  becomes `TermKind` — so `param N: usize` records `Const(usize)` and a future
  `param cfg: Config` records `Const(Config)` with no new machinery.

## 3. The table, one unifier, obligations

- **`InferenceTable`**: one store keyed by `InferVar`, entries
  `(TermKind, Option<Term>)`, backed by **ena union-find from the start** (as
  chalk does). Not speculative robustness: the `Vec<Option<_>>` buckets
  already produced a real bug — `v + v` unified a domain var with itself,
  the bind arm wrote `Infer(v) := Infer(v)`, and `resolve_*` hung forever
  (found via `const_then_clocked.mrn`; band-aided with same-term early-outs
  in all three unifiers + a regression test). Union-find makes `unify(v, v)`
  a no-op structurally. All `fresh_*` become `fresh(kind)`.
- **One `unify(Term, Term)`** dispatching structurally. Kind mismatch is an ICE,
  not a diagnostic — terms are well-kinded by construction from lowering.
- **`subsume(actual, expected)` distinct from `unify`.** Domain subtyping is
  directional: `@const ≤ @D` holds, the reverse does not. Today
  `unify_domain` is symmetric (`(Const, _) | (_, Const) => {}`), so a clocked
  value flowing into a const slot passes. `subsume` is applied at coercion
  sites (argument positions, ascribed `let`, `return` — the rustc coercion-site
  set); `unify` stays strict equality everywhere else.
- **One `TermFolder`** (super-fold over the three categories) replaces the
  `substitute`/`subst_kind`/`subst_args`/`subst_const`/`subst_domain` family
  and the `deep_resolve` family. A `Substitution` is just `&[Term]`.
- **An obligation queue on `InferCtx`**, discharged by fixpoint at end of body
  (the OutsideIn split; the old compiler's `ConstEq` machinery generalised):
  ```rust
  enum Obligation<'db> {
      TermEq(Term<'db>, Term<'db>),       // undecided equalities (subsumes width_residuals)
      DomainAll(Type<'db>, Domain<'db>),  // `T @ D` on an opaque type
      Sort(Domain<'db>, DomainSort),      // e.g. reg requires Clock
  }
  ```
  Survivors become signature residuals, propagated to callers / emitted as
  `initial assert` by the backend (replacing the ad-hoc
  `width_residuals: Vec<(u32, u32)>`).

## 4. Domain checking on top (the redux doc, in pass order)

### 4.1 Lifting in `sig_of`
A def with **no domain syntax anywhere** (no `dom`, no `@`, no `param`-field)
gets an implicit `__Dom` generic param of sort `Domain`, **appended last** so
user `Param(i)` indices are untouched (index stability matters for
incrementality). Every bare field/param/return domain becomes
`Domain::Param(__Dom)`; bare opaque type params get a `DomainAll(A, __Dom)`
obligation attached to the signature. A def **with** domain syntax is in
explicit mode: a bare field/param/return domain is a
`MissingDomainAnnotation` diagnostic. Pure `fn`s lift all args + result onto
one shared `__Dom` (shared-variable lifting — see redux doc for why per-arg
freshness is uninhabitable).

### 4.2 Sorts + real prelude signatures
`dom clk: Clock` declares sort `Clock`; lifted `__Dom` is sort `Domain` (so
const folding survives: `add(2, 3)` stays `@const`). `reg` gets a genuine
prelude signature —

```
reg : {dom D: Clock} (self: T @ D, rstn: Reset @ D, init: T @const) -> T @ D
```

— replacing the `"reg" => recv` hardcode in `infer_method`. That hardcode is
the root of the silent-miscompile class found while sorting the examples
(unrecognised `.reg` forms type-check and then emit as plain `assign`); with a
real signature, arity/shape mismatches become type errors. `when
clk.posedge()` connects the block's result domain to `clk`'s domain (`When` is
currently domain-blind in `infer`).

### 4.3 `T @ D` obligations
Emitted by: lifting over polymorphic structs (§4.1), inline `A @clk` on opaque
field/param types (already lowers today — the obligation is the new part), and
instantiation propagating a callee's signature obligations to the call site.
Discharge: structural for head-known types (unify every domain slot with `D`),
deferred for opaque heads; `@const` components satisfy trivially.

### 4.4 Coercions, defaulting, diagnostics
`subsume` at the coercion sites (§3). Unconstrained domain vars already default
to `@const` in `finish()` — keep, now justified by the elision rules.
Diagnostics name both domains (`expected @clk, found @clk2`), which needs a
`Domain` renderer; `RequiresClock` for sort violations ("`reg` requires a clock
domain, but the inferred domain is `@const`").

### 4.5 Backend alignment
Flatten's `apply_struct_domain` / `apply_port_domain` stamping becomes ordinary
substitution of the lifted `__Dom` arg — Domain-kind args flow like every other
generic, and the `Unspecified`-stamping path is deleted. `clock_name` resolves
`Domain::Param` through instantiated args as it already does.

## 4.6 As built (implementation notes)

Phases A-D landed; deviations from the sketch above, chosen during
implementation:

- **Structs are not given a materialised `__Dom` generic arg.** A pure
  struct's "lifted domain" is represented by the aggregate's existing
  top-level domain slot (`Type::Value { domain }` / `Type::Port { domain }`),
  which the redux semantics make equivalent for single-domain aggregates.
  Field access and record construction *stamp* that domain over the declared
  field types' `Unspecified` slots (a `Substituter` policy, reaching inside
  substituted-in args without re-substituting their `Param`s) — the
  head-known discharge of `Ty @ D`. The backend's flatten stamping is the
  same operation at emission time, so `Domain::Unspecified` survives as the
  aggregate-internal "slot" marker until a future backend rework; `unify`
  keeps one lenient `Unspecified` arm for exactly that flow.
- **Fn lifting appends `__Dom` last** (`GenericParam::is_lifted_dom`); the
  backend skips it when mapping Domain generics to clock ports/wiring, so a
  pure fn stays combinational and emitted SV is unchanged.
- **`subsume` with an unresolved expected side merges** (actual's kind at a
  fresh join domain) rather than unifying — generic args are invariant, so a
  `@const` actual must not pin an inference variable const.
- **Eager-unification limitation:** a *variable* receiver domain flowing into
  `reg` unifies with the register's clock rather than coercing (only a
  resolved `@const` coerces). Consequence: `let one: uint(8) = 1;
  one.reg(...)` infers `one` at the clock, not `@const`. Harmless for SV;
  revisit if a const context ever needs such a binding.
- **`when` arity:** the event's clock is recovered by mapping the `posedge`
  receiver local back to its `dom` generic (`event_clock`).
- **`ConstArg::Deferred` survives** (arithmetic / anon-const widths in
  signatures still lower to it pending Q4c's `Unevaluated`), but the silent
  lenient-unify hole is closed: any symbolic width pair now queues a `ConstEq`
  obligation instead of being accepted.
- The `ConstDomain` obligation (width locals must be `@const`) currently
  carries a default span (def start) — span it at the ascription when sig/body
  lowering grows type spans.

## 5. Staging

- **Phase A — mechanical merge, zero behaviour change.** `Term`/`InferVar`/one
  table; port unify/resolve/subst onto the folder; `GenericArg = Term`;
  `Unspecified` survives temporarily. Whole corpus stays green. Also: rewrite
  `planning/ir_pipeline.md`, which still documents `mirin-compiler-old`'s pass
  list, to describe the query pipeline (CLAUDE.md requires it kept in sync).
- **Phase B — obligations + const groundwork.** Obligation queue with the
  end-of-body fixpoint (`ConstEq` first); `width_residuals: Vec<(u32, u32)>`
  generalises to `const_residuals: Vec<(ConstArg, ConstArg)>` (backend still
  asserts Param-Param pairs); `ConstArg::Local(LocalId)` so a body ascription
  width (`uint(n)`) keeps naming its local instead of collapsing to the
  lenient `Deferred`. *Scope decision:* the `Const { ty }` payload is deferred
  to Q4c — `ConstArg` inside `Type` with a `Type` payload needs a `Box` cycle,
  and until struct-valued consts land every const is implicitly `usize`;
  `TermKind::Const` grows the payload when const_eval needs it.
- **Phase C — domain checking** in the order §4.1 → §4.4. Each sub-phase flips
  specific examples: C1 makes `when_no_clk` fail; C2 makes `cross-reset` and
  `clocked-width` fail and keeps `reg_const_input` passing; C3 makes
  `mixed-struct-clocks` fail; lifting keeps `two-doms-fn` failing at the call
  site. (Files in `examples/todo-incorrect-pass/` — move each to
  `fail-expected/` as it starts failing for the documented reason.)
- **Phase D — backend alignment** (§4.5), then delete the dead code.
  *As built:* less was deletable than sketched — `ConstArg` stays until the
  `Const { ty }` payload lands (Q4c), `Domain::Unspecified` + the backend
  stamping path stay until the backend rework (§4.6), and `freshen_domains`
  stays in top-level-only form. What did die: `freshen_kind_domains`,
  `subst_domain`'s fresh-var minting, and the three per-kind variable pools.

## 6. Explicitly out of scope for Q7

- ~~`const_eval` (Q4c)~~ — **landed** (`planning/const_eval.md`): demand-driven evaluator over body HIR; `ConstArg` grew the `Op`/`Field` tree instead of a separate `Unevaluated` body ref; `Deferred` remains only for calls in width position.
- Struct-valued consts / valtrees — the representation is ready
  (`Const.ty` can name a struct; `ConstKind::Lit` grows a `Value` payload).
- `Retag` / CDC primitives, `WithReset` / `Delayed` — additive constructors.
- The `@_` anonymous domain and the visibility lint — needs grammar + a lint
  channel.
- `where`-clause surface syntax for `T @ D` — inline `A @clk` covers the
  current corpus; grammar work decides the rest.
- ~~**Grammar gap found while writing examples**: `let_statement` has no type
  ascription.~~ **Fixed before Phase A**: the grammar now accepts
  `let <name> (: <type>)? = <expr>` (tree-sitter, mirin-fmt, body lowering all
  updated); `clocked-width.mrn` and `reg_const_input.mrn` exercise it.
