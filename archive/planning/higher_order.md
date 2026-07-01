# Higher-order functions

> **Status: design / thoughts.** Nothing implemented. This doc works the design,
> with the **domain-checking interaction** (`planning/domain_checking.md`) as the
> centrepiece — that is the hard part. Companion reading: `planning/traits.md`
> (the obligation/solver machinery this reuses wholesale), `domain_checking.md`
> (the lift rule, the `Clock ⊑ Domain` sorts, and the `@const` lattice this
> generalises).

## What an HOF is in Mirin

A component parameterised by another component:

```mirin
impl {dom D, type A} DF(A) {
    fn map {type B} (self @D, f: Fn(A) -> B) -> DF(B) @D { … }
}
```

`map` takes a transform `f` and applies it to a stream's payload. The same shape
covers `Vec::map`, `fold`, a generic pipeline combinator, a generic
arbiter parameterised by its scheduling function, etc.

Two stances are fixed by the rest of the language and make this *much* smaller
than it looks:

- **Everything monomorphises; no `dyn`.** `traits.md` already rules out trait
  objects. A function argument is therefore never a runtime value — it is a
  statically-known callee, specialised away at mono time. This is rust's
  **fn-item-type** path (`TyKind::FnDef` — a distinct zero-sized type per `fn`,
  carrying a `DefId`, satisfying the `Fn` traits via compiler-synthesised
  impls), **not** `dyn Fn`. `map(s, g)` infers the fn-item type of `g`, checks
  it against the `Fn` bound, and emits a specialised `map` whose body calls `g`
  directly.
- **No closures / no captures in v1.** The argument is a named `fn` (or method).
  Capturing a signal into a function value is a large separate feature
  (it turns the ZST fn-item into a struct of captured leaves — rust's closure
  desugaring) and is deferred. v1 HOFs are Verilog/Chisel-style *module
  generics*: structural parameterisation by a named component.

So the surface feature is "pass a named `fn`"; the whole weight of the design is
**what domain discipline the `Fn` bound imposes**, and that is where it touches
`domain_checking.md`.

## The rustc analogy

`Fn(A) -> B` is rust's parenthesised sugar for a trait bound
(`Fn<(A,), Output = B>` — args as a tuple, return as an associated type). The
function param is a Type-kind generic `F: Fn(A)->B`; a call `f(x)` resolves
through that bound like `a.add(b)` resolves through `Add` today. All of this is
already-built `traits.md` machinery — `Fn` is "just another trait" whose impls
are auto-generated per fn-item.

The **one genuinely new mechanism** is the domain quantifier on the bound, and
its rust analogy is exact:

| Mirin | rust |
|---|---|
| pure function (works at any domain) | **higher-ranked** `for<'a> Fn(&'a A)->&'a B` |
| clocked function (fixed to a clock in scope) | region-fixed `Fn(&'x A)->&'x B` |
| a domain ≈ a region | the doc already states domains ≈ regions |
| instantiating `f`'s domain where it's applied | **late-bound region instantiation** (fresh region var per call) |
| rejecting a clocked `f` against a pure bound | **leak check / universe placeholder** discipline |

That last row is the soundness crux and is spelled out below.

## Functions are already domain-quantified

The lift rule (`domain_checking.md` §"Lifting pure signatures") says a signature
mentioning no domains is quantified over one shared domain parameter. **That rule
*is* the function's domain quantifier** — we don't invent a new one, we read the
existing one off the function being passed:

```mirin
fn inc(x: uint(8)) -> uint(8)            // lifts to:
fn inc {dom Df: Domain}(x: uint(8) @Df) -> uint(8) @Df
```

`inc` is **domain-polymorphic** — a "pure function". A function that names a
clock, or that uses `.reg` (which forces its domain to sort `Clock`), is not:

```mirin
fn stage {dom clk: Clock}(x: uint(8) @clk) -> uint(8) @clk { x.reg(rstn, 0) }
```

`.reg` needs a real edge, so `stage`'s quantifier is over `Clock`, not `Domain`
(`domain_checking.md` §Sorts). This gives **three tiers**, which are just the
sort lattice — and they, not a binary, are the real taxonomy:

| Tier | Quantifier | Means | Example |
|---|---|---|---|
| **const-pure** | `for<dom D: Domain>` | combinational; works even at `@const` | `inc`, `a + b` |
| **clock-generic** | `for<dom D: Clock>` | combinational *or* registered; needs an edge, any clock | `stage` above with `clk` abstract |
| **clock-fixed** | a specific `clk` in scope | tied to one clock | `f: Fn(A @clk) -> B @clk` |

The user's "pure vs clocked" is tiers (const-pure) vs (clock-fixed); clock-generic
is the useful middle the sort system hands us for free.

## The Fn bound: bare = higher-ranked, named = pinned

The surface rule is **identical to top-level elision**, applied per function
argument:

- `f: Fn(A) -> B` — bare domains → lifted → a fresh **higher-ranked** binder
  local to `f`: `for<dom Df: Domain> Fn(A @Df) -> B @Df`. The HOF accepts only
  functions that work at *every* domain — i.e. const-pure ones.
- `f: Fn(A @D) -> B @D` — names the HOF's own `{dom D}` param → `f` is **pinned**
  to `D` (rank-1). The HOF accepts any function usable *at `D`*: a pure one
  (instantiated at `D`) or one clocked at `D`.

The HOF's body applies `f` by ordinary unification. `f(self.data)` with
`self.data : A @D` unifies `f`'s input domain with `D`; this is exactly rust's
**late-bound instantiation** — replace the bound `Df` with a fresh domain var,
solve it from the flowing clock. Applying `f` at more than one domain inside one
HOF is what genuinely needs the higher-ranked binder (a rank-1 pin couldn't).

## Surface syntax (settled)

**The `Fn` bound is positional and Rust-shaped — no named section.**

```mirin
Fn(A) -> B          // pure:   for<dom Df: Domain> (A @Df) -> B @Df
Fn(A @D) -> B @D    // pinned to a domain D already in scope
Fn(A) -> (B, C)     // multi-result via a tuple result
```

A function passed to an HOF is **applied positionally**. Mirin's named sections
(`{dom clk}`, `{by = 4}`) carry generics and named const/value args; replicating
that whole call-section grammar inside a *type* buys little, and two cheaper
routes cover what it would have:

- **Adapt with a lambda** — a function with named args/generics is wrapped to a
  clean positional shape: `|x| gainstage{ gain = 2 }(x)`.
- **Pin the clock in the lambda's type annotation** rather than a named `{dom}`
  section — `|x: A @clk| …`. The `@` stays in *type* position (it always
  constrains a type, never a binder). This removes the only real reason the `Fn`
  type would have needed a named section, so it doesn't get one.

**Lambdas are Rust-shaped.** Their domains elide/lift exactly like a top-level
signature — and like a `let`, an omitted domain is **inferred from the body**, so
most lambdas need no annotation at all:

```mirin
|x| x + x                                   // domain inferred (pure if nothing pins it)
|x: uint(8)| x + x                          // type-annotated param
|x: uint(8) @clk| x + x                     // domain pinned in TYPE position (cf. rust |x: &'a T|)
|x: uint(8) @clk| -> uint(8) @clk { … }     // fully annotated, block body
```

There is **no value-position domain syntax** (`|x @clk|` is rejected): `@`
constrains a type, and a bare binder has no type to constrain. Annotate the type
(`|x: A @clk|`) or — far more often — let the body infer it: a use of a clocked
signal pins the domain exactly as in a `let` (e.g. `rstn: Reset @clk` flowing
into `.reg`). A *registering* lambda takes its reset as a **parameter** in v1
(`|x, r| x.reg(r, 0)`); closing over an ambient clock/reset is the deferred
capture follow-up. Named functions are passed by name: `map(v, double)`.

## The three cases, with types (and the rejected fourth)

### 1. pure → pure  (combinational map of a combinational function)
```mirin
fn double(x: uint(8)) -> uint(8) { x + x }
//  double : Fn(uint(8)) -> uint(8)
//         ≡ for<dom Df: Domain> (uint(8) @Df) -> uint(8) @Df          // pure

fn map {param N: integer, type A, type B}
    (v: Vec(N, A), f: Fn(A) -> B) -> Vec(N, B) { … }
//  f : Fn(A) -> B                                                     // bare ⇒ higher-ranked
//  map lifts to  {dom __Dom: Domain} (Vec(N,A) @__Dom, f …) -> Vec(N,B) @__Dom

let w = map(v, double);
//  v : Vec(N, uint(8)) @__Dom    ⟹    w : Vec(N, uint(8)) @__Dom
//  double's Df := __Dom. Nothing forces a clock — works at @const too.
```

### 2. pure → clocked  (a pipelined map; f is the combinational transform)
```mirin
//  double : Fn(uint(8)) -> uint(8)                                    // pure, as above

fn pipe_map {dom clk: Clock, type A, type B}
    (s: DF(A) @clk, f: Fn(A) -> B, rstn: Reset @clk) -> DF(B) @clk { … }
//  f : Fn(A) -> B                                                     // bare ⇒ higher-ranked

let out = pipe_map(stream, double, rstn);
//  stream : DF(uint(8)) @clk    ⟹    out : DF(uint(8)) @clk
//  double's Df := clk. A Domain-polymorphic fn instantiates at a Clock for free
//  (Clock ⊑ Domain) — no CDC obligation.
```

### 3. clocked → clocked  (f is itself a registered stage; passed as a clocked lambda)
```mirin
fn pipe_map {dom clk: Clock, type A, type B}
    (s: DF(A) @clk, rstn: Reset @clk, f: Fn(A @clk, Reset @clk) -> B @clk) -> DF(B) @clk { … }
//  f : Fn(A @clk, Reset @clk) -> B @clk        // pinned to the HOF's clock; @ in TYPE position
//  the reset is an ARGUMENT, not a capture (v1 lambdas don't close over data signals)

let out = pipe_map(stream, rstn, |x, r| x.reg(r, 0));
//  |x, r| x.reg(r, 0)  :  Fn(uint(8) @clk, Reset @clk) -> uint(8) @clk    // clocked (it registers)
//  binders are bare; their types — including @clk — are inferred from the expected bound
//  stream : DF(uint(8)) @clk    ⟹    out : DF(uint(8)) @clk
//  the lambda's clock must equal clk; clocked elsewhere ⇒ CDC error.
```
> With ambient clock/reset capture (deferred), this would read `f: Fn(A @clk) ->
> B @clk` and `|x| x.reg(rstn, 0)` — the reset captured rather than threaded.
> Either way, no domain ever appears in value position.

### 4. clocked → pure — **rejected**, and this is the soundness boundary
```mirin
fn map {dom D: Domain, …}(self: Vec(N,A) @D, f: Fn(A)->B) -> …
map(v, stage)        // ✗
```
`map`'s bound is `for<dom Df: Domain>`: `f` must work at *every* domain including
`@const`. `stage` registers, so it needs `Df: Clock`. There is no edge to give it
in a context that must also typecheck at `@const`. **This is the dual of
`1.flipflop()` being rejected** (`domain_checking.md` §Sorts) — lifted to
function arguments. It must fail *at `map`'s call site*, not inside a specialised
body, per the `traits.md` stance (a contract fails at the boundary).

## How the rejection is checked (the new mechanism)

This is the only part not already in the compiler, and it borrows rust's
higher-ranked machinery directly:

- **Representation.** A predicate gains an optional domain binder:
  `for<dom D: sort> Predicate::Fn(FnRef { args, ret })`, the analogue of
  rust's `Binder<TraitRef>` with a bound domain var. Bare-domain `Fn(...)`
  bounds lower to this; pinned ones are ordinary rank-1 predicates over a domain
  already in scope.
- **Satisfaction.** To check that a candidate function satisfies a
  `for<dom D: S>` bound, **instantiate `D` with a placeholder domain in a fresh
  universe** and unify the candidate's (lifted) signature against the bound. A
  **leak-check-style** rule rejects any solution that pins the placeholder to a
  specific lower-universe domain. A const-pure function imposes no such pin and
  passes; a clock-fixed or registering function forces the placeholder toward a
  `Clock`/specific clock and **leaks** → rejected. The `Clock ⊑ Domain` sort on
  the binder does the rest: a `for<dom D: Domain>` placeholder cannot satisfy a
  candidate's `D: Clock` demand.
- **Application.** At `f(x)` inside the HOF body, instantiate the binder with a
  **fresh domain inference var** (late-bound), solved from `x`'s domain — no new
  mechanism beyond what unification already does.

This rides the existing obligation queue and end-of-body fixpoint
(`traits.md` §"Obligations and the solver"); `Fn` is one more goal kind, and the
placeholder/universe bookkeeping is the single new addition to the solver.

## Combinational vs registered falls out for free

Note there is **no separate "is this function pure/combinational?" predicate**.
A function that registers necessarily quantifies over `Clock` (because `.reg`
demands an edge), so it automatically fails a `for<dom D: Domain>` bound. "Pure"
= "domain-polymorphic over `Domain`" = "combinational" are the *same fact* in
three vocabularies. The sort system already drew this line; HOFs just read it.

## Spatial vs temporal application (a hardware note, not a domain one)

The domain story is identical across containers, but the *cost* is not:

- `Vec(N,A)::map(f)` instantiates **N copies** of `f` (spatial unroll) — N
  combinational blocks, or N pipeline stages if `f` registers.
- `DF(A)::map(f)` instantiates **one** copy of `f` (temporal/serial — the stream
  carries successive values through a single instance).

Same bound, same domain checks; the lowering differs (loop-instantiate vs
single-instantiate). Worth stating so the `bit_size`-style mono-cost questions
(`planning/mono_check.md`) are anticipated: a `Vec::map` of a registering `f`
multiplies hardware by N.

## Representation & pipeline placement

- **No first-class function `Type` needed in v1.** The HOF param is a Type-kind
  generic with an `Fn` predicate; the argument is a fn-item (a `DefId` + substs),
  represented like rust's `FnDef` — a ZST. A `Type::FnItem(DefId, Vec<Term>)`
  variant is the natural carrier; it never reaches the backend as a value (it's
  mono'd into a direct call), so the SV IR is untouched.
- **`sig_of`** lowers `Fn(...)` bounds to (possibly higher-ranked) predicates;
  **`infer`** solves them in the fixpoint; **`Instance::resolve`** at mono time
  maps the `Fn` obligation to the concrete callee (exactly the trait-method
  re-selection already implemented for `unpack` et al.).
- **Backend.** A specialised HOF body emits with `f` replaced by the concrete
  callee — inline (`#[inline]` f) or as an instance, by the same rules as any
  other call. Nothing new in `backend/ir.rs`.

## Capture (closure conversion) — the follow-up, designed

v1 forbids capture: a lambda's free signals must be threaded as parameters
(`|x, r| x.reg(r, 0)`, the programmer doing the lambda-lifting). The natural
follow-up is to allow the lexically obvious form:

```mirin
let out = pipe_map(stream, |x| x.reg(rstn, 0));   // rstn captured from scope
```

This is classic **closure conversion / lambda-lifting**, and the rust analogy is
exact: rust desugars a closure to a **struct of its upvars** plus an `Fn` impl.
Mirin has no runtime function values, so that "environment struct" is a **bundle
of wires**: *captured signals become extra input ports on the monomorphised
module.* Two facts make this tractable:

- **Capture is type-transparent and CDC-safe for free.** The closure's *type*
  never mentions `rstn` (as in rust, upvars don't appear in the `Fn` signature).
  But inside the body `rstn` resolves to the in-scope `rstn: Reset @clk`, so
  ordinary inference pins the lambda to `clk` and gives `Fn(uint(8) @clk) ->
  uint(8) @clk`. A captured signal on a *different* clock than the HOF applies it
  at clashes at the application site — the existing CDC error, no new machinery.
  Capture cannot launder a clock; the domain rides the wire. The type layer needs
  to know **nothing** about capture — only lowering does.
- **If the HOF inlines, capture is free.** The env-as-ports cost appears *only*
  when the HOF is emitted as a separate module. Inlined (the `#[inline]` /
  operator path), the body is spliced where `rstn` is already in scope — no port,
  no threading. This is the payoff of full-mono + no runtime values: rust *must*
  closure-convert (separate compilation, runtime dispatch); Mirin can *choose*
  inlining, and capture pushes toward it.

So the two lowerings:

```
inline   ⟹  closure body spliced at the call site; captures resolve in scope. Zero env-threading.
module   ⟹  pipe_map__<closure> gains one input port per capture (each with its
            captured domain); the caller wires them. Plain closure conversion.
```

The module path adds one **capture-analysis** step (per lambda, the free-signal
set — reads; *driving* a captured signal is an output capture, heavier, restrict
in v1) and augments the mono'd module's port list. No new *checking* (CDC and
drive-completeness reuse existing machinery once captures are ports); the mono
key already distinguishes closures (each is a distinct fn-item type). For a
spatially-replicated `Vec::map`, one capture port fans out to all N instances.

## Open questions

1. **Surface syntax — settled** (see §"Surface syntax"): positional Rust-shaped
   `Fn(A) -> B`, **no named section** (adapt with a lambda; pin clocks in a
   lambda's *type* annotation, never value position); Rust-shaped lambdas
   `|x: A @clk| …`. Still open: the multi-result spelling's interaction with
   named results (`planning/return_variable.md`) — `Fn(A) -> (B, C)` vs named
   result parts.
2. **Pinned-domain ergonomics.** Is bare `Fn(A)->B` (higher-ranked, pure-only)
   the right default, or should the common "same clock as the HOF" case be the
   default and purity be opt-in? Defaulting to higher-ranked is the *safer*
   (more-restrictive-to-pass) choice and matches the elision rule, so lean that
   way; revisit if real combinators find it noisy.
3. **Multi-domain function arguments** (synchronisers / dual-clock FIFOs as HOF
   args): `f: Fn(A @a) -> B @b`. The binder generalises to several domain vars;
   the relationship between them is the point, exactly as for explicit `{dom a,
   dom b}` signatures. Probably just works, but unproven.
4. **Do we need the full universe/leak-check machinery in v1**, or is the common
   single-application HOF servable by "instantiate `f`'s domain to a fresh var at
   the one application site, check the candidate at the call"? The latter avoids
   universes but cannot express "applied at two domains". Start rank-1 +
   call-site check; add the binder when a real multi-application combinator needs
   it.
5. **Closures / capture** — designed above (§"Capture"): closure conversion with
   captures as extra module ports, or free under inlining. Out of v1, but the
   shape is settled. Remaining open: output captures (driving a captured signal),
   and the inline-vs-module heuristic when captures are present.

## Staging (sketch)

- **H1 — fn-item types + concrete `Fn` calls.** `Type::FnItem`, the `Fn`
  trait family, pass a named fn to a fn parameter, call it; pinned-domain bounds
  only (rank-1). Customer: `Vec::map` at a single domain.
- **H2 — higher-ranked (pure) bounds.** The `for<dom D>` binder, placeholder +
  leak-check satisfaction, the three cases above as examples + the rejected
  fourth as a fail-example. Customer: a `map` that accepts any pure `f` and is
  reused at both `@const` and a clock.
- **H3 — multi-domain & method-form** function args; ergonomics pass on the
  default.
- **H4 — capture (closure conversion).** Free-signal analysis per lambda;
  captures as extra ports on a modular HOF, or resolved in scope when inlined
  (§"Capture"). Read captures first; output captures later.
- **Later.** First-class function values if a real customer for runtime dispatch
  ever appears (it shouldn't, given full mono).
