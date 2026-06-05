# Q4 plan — `const_eval`, width checking, and dependent widths

Q4 turns on what Q3 deferred: **widths are checked** (`uint ~ uint` stops being
a free pass), const-generic width *arithmetic* discharges or propagates as
residuals, and arbitrary const-expression widths (`uint(cfg.get_bits())`) are
evaluated by a memoized `const_eval` query. Grounded in `planning/parametricity.md`
(the const-generic + residual design), `query_engine.md` §2.3/§3.4 (the
`const_eval` node + anon-const identity), and the old `hirt/normal_const.rs`.

## 0. The load-bearing facts

- **Two distinct mechanisms, often conflated:**
  1. **Linear width arithmetic over generic params** — `uint(N)`, `uint(M+N)`,
     `uint(2*N)`. Handled by a **sum-of-monomials normal form** + `ConstEq`
     obligations *inside* `infer`/`sig_of`. No separate query. (`parametricity.md`.)
  2. **Arbitrary const-expression widths** — `uint(cfg.get_bits())`: a width that
     is a *body* to evaluate, not linear arithmetic. This is an **anon-const**
     with its own `DefId`, evaluated by the memoized **`const_eval(def)` query**,
     which can pull `infer`/`sig_of`/`const_eval` of *other* defs (the §3.4
     sideways edge) and so must be cycle-checked. (`query_engine.md` §2.3/§3.4.)
- **The grammar gates 2-of-3.** `type_argument = type_expression | number`, so a
  width is only a literal or a (possibly parametric) type name. **`uint(N+1)`,
  `uint(M+N)`, and `uint(cfg.get_bits())` do not parse.** Arithmetic and
  dependent widths need a grammar extension; literal + single-param widths do not.
- **Nothing in `examples/working` uses arithmetic or dependent widths.** They use
  `uint(8)` and `uint(N)`. So **width *checking* (mechanism 1, literal+param
  subset) is the only part with present-day test coverage** — and the only part
  buildable without touching the grammar.
- **Current state:** the new compiler's width is `ConstArg ∈ {Lit, Param,
  Deferred}` (`hir/types.rs`), and `infer` treats `uint ~ uint` as always equal —
  so `uint(8) = uint(16)` is silently accepted today. That is the bug Q4a fixes.

## 1. rustc analogy

- **Const generics** (`fn f<const N: usize>`) = Polar `{ param N: usize }`.
  Instantiation is `EarlyBinder::instantiate` over a positional `GenericArgs`
  (already mirrored in `infer`'s `substitute` + `fresh_subst`).
- **`ConstKind`**: `Param` / `Infer(ct-var)` / `Value` / `Unevaluated{def, args}`
  / `Error`. Polar's `NormalConst` covers `Param`+`Infer`+`Value` (linear);
  `Unevaluated` is the **anon-const** referenced by a `DefId`, the `const_eval`
  case.
- **Anon consts**: rustc gives array-length / generic-arg const expressions their
  own `DefKind::AnonConst` + body, evaluated by `tcx.const_eval_*` (memoized,
  cycle-checked). Polar's `uint(<expr>)` is exactly this; the `DefPathSegment::
  AnonConst` role baked in at Q2d is the identity hook.
- **OutsideIn(X) residuals**: wanted constraints simplify; the residual rides on
  the signature and is re-checked at call sites. Polar's `ConstEq` obligations +
  `sig_residuals` are the same.

## 2. The pieces

### Width representation — adopt `NormalConst`
Replace `ConstArg {Lit, Param, Deferred}` with a linear normal form (port of
`normal_const.rs`):

```
NormalConst { constant: i64, terms: Vec<(i64, ConstTerm)> }   // sorted, deduped
ConstTerm = Param(u32) | Infer(u32) | AnonConst(DefId)        // + Local later
```

`uint(8)` → `{8, []}`; `uint(N)` → `{0,[(1,Param i)]}`; `uint(M+N)` →
`{0,[(1,M),(1,N)]}`; `uint(cfg.bits())` → `{0,[(1, AnonConst(def))]}` (an opaque
term whose value comes from `const_eval`). Equality is structural after
normalisation, so `M+N` ≡ `N+M`, `N+N` ≡ `2*N`. Non-linear (`M*N`) stays an
opaque term. This single rep serves Q4a/b/c.

### Const inference variables
A `const_vars` pool on the infer context parallel to `type_vars`/`domain_vars`.
At a call site a Const-kind generic param instantiates to a fresh `Infer` term
(replacing today's `ConstArg::Deferred` stub in `fresh_subst`). `unify_widths`:
both ground → equality check (mismatch diagnostic); one a single var → bind;
otherwise → defer to an obligation (Q4b).

### Residual obligations + `sig_residuals`
`ConstEq{lhs, rhs: NormalConst}` obligations queued during inference, discharged
to fixpoint at end of `infer(def)`; whatever survives is **attached to the
signature** (a `sig_residuals(def)` output, so callers depend on the *signature*,
not the body — firewall-preserving). At a call site the callee's residuals are
substituted through the call's `GenericArgs` and re-queued: empty → discharged,
ground-false → error, still-symbolic → propagates to the caller's residuals.
Recursive const-generic calls with non-identity args are rejected (per
`parametricity.md`).

### `const_eval(def)` query + anon-const defs
A width that is an arbitrary const expression (a postfix/method call,
field access, …) is lowered to an **anon-const**: a `DefId` minted with a
`DefPathSegmentKind::AnonConst(role)` segment (role = ReturnTypeWidth /
ParamWidth(i) / FieldWidth(i), already defined). `const_eval(def)` evaluates it to
a `NormalConst`/value, demanding `infer`/`sig_of`/`const_eval` of whatever defs the
expression references — the §3.4 sideways pull. **Cycle** (`add`'s width needs
`add`'s width) is caught by salsa's cycle machinery (`cycle_fn`/`cycle_initial`)
and surfaced as a diagnostic. Width checking in `infer` *demands* `const_eval` to
ground an `AnonConst` term before comparing.

## 3. What changes in existing queries

| Query | Change |
|---|---|
| `sig_of` | widths lower to `NormalConst`; a non-linear/call width mints an anon-const `DefId` and stores an `AnonConst` term. New output `sig_residuals(def)` (or a field). |
| `infer` | `unify_kind` for `UInt` compares widths (was a no-op); `const_vars` pool; `ConstEq` obligations + fixpoint; demand `const_eval` for `AnonConst` terms; propagate callee residuals at call sites. |
| (new) `const_eval(def)` | evaluate an anon-const to a `NormalConst`; recurses into other defs; cycle-checked. |
| `crate_def_map` (Q2d table) | mint `DefPath`s for anon-const defs using the `AnonConst` segment. |

## 4. Grammar prerequisite (`Q4-grammar`)

`type_argument` must admit width *expressions*. Two steps, matching the two
features:
- **Arithmetic** (`uint(N+1)`, `uint(M+N)`): allow `+`/`*`/parens over names and
  numbers in width position. Smallest: a dedicated `width_expression` rule
  (literal / name / `+` / `*` / parens) rather than reusing full `expression`
  (avoids the `{`/record ambiguity). Needed for Q4b.
- **Dependent** (`uint(cfg.get_bits())`): allow a postfix/method-call expression
  in width position. Needed for Q4c. Regenerate the parser, update the corpus,
  and patch the old compiler's lowering (it already models widths as `HirExpr`,
  so its surface lowering may already accept more than the grammar emits).

Literal + single-param widths (Q4a) need **no** grammar change.

## 5. Sub-slices

- **Q4a — width checking (no grammar change).** `NormalConst` width rep +
  `const_vars` + turn on `unify_widths` (ground equality → mismatch; single-var →
  bind) + instantiate Const generics to fresh const-vars. Catches `uint(8) ~
  uint(16)`; makes `add{N}(uint(N),uint(N))->uint(N)` genuinely check; tightens
  `parametric_width_fn`/`_port` from unchecked-pass to checked. **Highest value,
  lowest cost — the core of Q4.**
- **Q4-grammar — width expressions.** Extend the grammar (arithmetic; then
  postfix for dependent). Regenerate + corpus + old-compiler patch.
- **Q4b — residual obligations (parametricity Phase D).** `ConstEq` + fixpoint +
  `sig_residuals` + call-site propagation + recursive-const rejection. Makes
  `concat{M,N}->uint(M+N)` check. Depends on Q4-grammar (arithmetic).
- **Q4c — `const_eval` query + anon-const defs + dependent widths.** Anon-const
  `DefId` minting, the `const_eval(def)` node, the sideways pull, cycle
  detection. Depends on Q4-grammar (postfix). The `query_engine.md` §3.4 headline.
- **Q4d (Phase D′, really Q5) — Verilog assertions from residuals.** A surviving
  residual becomes an SV `initial assert (M + N == K)`. Belongs with the back end.

## 6. Decisions (resolved)

1. **Scope: Q4a only.** Ship width *checking* — the correctness win, and the one
   part the example corpus exercises. Defer arithmetic widths (Q4b) and dependent
   widths / `const_eval` (Q4c) until a motivating example appears. §2's
   `NormalConst` and §4's grammar extension and the `const_eval` query are
   **documented future work**, not built now.
2. **Width representation: the simple thing now.** Keep `ConstArg` and add an
   `Infer(u32)` variant (a const inference variable) — enough for call-site
   width inference + ground equality checking. Do **not** introduce `NormalConst`
   yet; adopt it when Q4b (arithmetic) lands. `ConstArg::Deferred` stays the
   "not-yet-representable" wildcard (arithmetic/anon-const widths), unified
   leniently so it never produces a false mismatch.

Still open, but only when their slice arrives:
3. **Cycle handling for `const_eval`** (Q4c): salsa `cycle_fn`/`cycle_initial`
   vs. a manual query-stack guard.
4. **`sig_residuals`** (Q4b): a separate query vs. a field on `sig_of` (RA-style
   leans separate — residual-only changes invalidate less).
