# Traits

Status: in implementation (decisions [D1]–[D6] settled 2026-06, recorded at
the end; staging below). Companion reading:
`planning/domain_checking_redux.md` (domain bounds share the
`where`/obligation infrastructure designed here),
`proposals/Whos_who_in_the_type_zoo.md` (the built-in type classes that
eventually become marker traits).

Prerequisite landed first: **binder-first impls** (`impl {dom clk: Clock}
Stream8 { … }`) for inherent blocks too, so trait and inherent impls share
one shape, plus auto-binding of a parametric owner's params into method
signatures. General syntax rule adopted (also resolves the
application-braces-vs-body-braces ambiguity): **positional argument sections
always come after named sections**; records are the one exception and never
appear in the two problematic positions (fn return type, impl header).

## Why traits, and why now

Traits are marked Optional on the todo-list, but most of the *non*-optional
items secretly depend on one mechanism: a way to say "this type supports
operation X" and resolve X to concrete code at monomorphisation. The
customers, in rough order of arrival:

1. **Operators.** `+`/`-`/`*` are special-cased prelude defs today
   (`is_prelude_op`, hand-rolled type rules in `infer_call`). Each new
   primitive family (sint, fixed-point, user numerics) would widen the
   special case. `Add`/`Sub`/`Mul` traits give one dispatch path; the
   builtin impls become ordinary (compiler-synthesized) impls.
2. **Numeric literals.** The lenient `integer ~ uint` unification arm is a
   placeholder. The real design is a literal-polymorphism trait
   (`FromInteger`-shaped) plus the existing width-fit checks — details get
   their own doc, but the *mechanism* is a trait obligation.
3. **Pack/unpack.** `derive`-able `Bits` with an associated const
   `width: integer` and `pack`/`unpack` methods is the gateway to generic
   FIFOs, memories, and CDC primitives over any packable payload.
4. **Generic components.** `fn buffer {param T: Bits} (x: T @clk) -> T @clk`
   — components generic over payload type, checked *before*
   monomorphisation (the strongest argument against Zig-style post-
   instantiation checking: a library component's contract should fail at
   its own signature, not inside its body at a user's call site).
5. **The type zoo.** The hierarchy sketched in the proposal (Port ⊇
   PosType ⊇ …) wants to be a set of built-in, structurally-derived marker
   traits (`Pos`, `Packable`, …) so bounds like "no ports inside" are
   expressible — including the linearity check on `fn dup(x: T) -> (T, T)`.
6. **Domain bounds.** `domain_checking_redux.md` already commits to
   `where T @ D` obligations on opaque type params. That is a
   trait-obligation in all but name; building traits means building the
   `where`-clause and param-env infrastructure both features share.

What we explicitly do NOT need: dyn/objects (everything monomorphises),
autoderef/autoref (no references), subtyping interplay beyond the `@const`
domain lattice, specialization, HRTB, auto traits, implied bounds.

## The rustc analogy

The compiler is rustc-shaped and the trait system follows rustc's old
solver, minus its documented mistakes. Mapping:

| Polar | rustc |
|---|---|
| `TraitRef { trait_def, args: Vec<Term> }` | `ty::TraitRef` (kinded GenericArgs) |
| `predicates_of(def)` query | `predicates_of` |
| param env: written bounds of enclosing item | `ParamEnv` (no implied bounds) |
| `ObligationKind::Trait(TraitRef)` in the existing queue | `FulfillmentContext` / `ObligationForest` |
| end-of-body fixpoint (already runs ConstEq) | `select_where_possible` + `select_all_or_error` |
| solver: param-env candidates + impl candidates | `SelectionContext` assembly/confirmation |
| coherence: pairwise freshened-header unification | `overlap_check`, minus where-clause reasoning |
| method probe: inherent first, then trait | `probe`/`pick` minus autoderef steps |
| backend re-selection at mono time | `Instance::resolve` |
| `call_substs` per call expr | node substs |

Deliberate divergences (designing out rustc's pain):

- **No implied bounds.** A body assumes exactly the clauses written on its
  signature (plus supertrait elaboration, when supertraits land). Rustc's
  struct-WF leakage into ParamEnv is a long-running soundness sore.
- **Param-env vs impl candidate overlap is an error**, not a preference
  rule. Rustc's "where-clause shadows impl" winnowing rule produced years
  of surprising selections; we reject the ambiguity instead.
- **Coherence ignores where-clauses.** Two impls whose headers unify
  conflict, full stop — even if their bounds are disjoint. ~50 lines,
  sound, predictable. (Rustc's negative-reasoning overlap checks are where
  the complexity lives.)
- **No literal defaulting pass.** Rustc's integer fallback (`{integer}` →
  `i32`) interleaves with trait solving and breeds order-dependence.
  Polar literals take width from context or error.
- **No associated types in the core.** Operators return `Self` (widths
  are value-level consts, so `uint(n) + uint(n) -> uint(n)` needs no
  `Output`). Associated *consts* are in scope; associated types wait for
  a real customer.
- **All cycles are errors** at a depth limit (~64). No coinduction.

## Surface syntax

### Trait declarations

```polar
trait Add {
    fn add(self, other: Self) -> Self;
}

trait Bits {
    const width: integer;
    fn pack(self) -> uint(width);        // bare assoc-const name in trait scope
    fn unpack(b: uint(width)) -> Self;   // no receiver: a "static" trait fn
}
```

- `Self` is a new keyword: the implementing type, usable in signatures
  (receiver, params, return). Inside a trait, `Self` is an opaque type
  parameter with the trait's own bound.
- Method signatures follow ordinary `fn` syntax including named sections —
  a trait method may bind its own `{dom clk: Clock, param n: integer}`
  generics, and `self @clk` works as in inherent impls today.
- Associated consts are declared `const name: integer;` (Const kind only
  for now). Referenced bare within the trait/impl, as `T::width` outside
  (path syntax already exists from the module system).
- v1 has **no default method bodies and no supertraits** — both are cheap
  and compatible, but they're follow-ups, not core. **[D5]**

### Bounds on generic params

One rule, extending the existing ascription-determines-kind scheme
(`dom x: Clock` → Domain, `param n: integer` → Const, `param T: Type` →
Type): **a trait name in a `param` ascription is a Type-kind param with
that bound**.

```polar
fn sum  {param T: Add}        (a: T, b: T) -> T { a.add(b) }
fn wide {param T: Add + Bits} (x: T) -> uint(T::width) { x.pack() }
```

`+` combines bounds. `param T: Type` stays the unbounded spelling. **[D1]**

Bounds that don't fit the inline form go in a `where` clause after the
signature — the same clause that `domain_checking_redux.md` needs for
`T @ D`:

```polar
fn delay {dom clk: Clock, param T: Bits} (x: T) -> T
    where T @ clk
{ ... }
```

Grammar-wise `where` takes a comma list of either `Ty : Bound (+ Bound)*`
or `Ty @ Dom`. Both lower to obligations; they differ only in goal kind.

### Impls

Inherent impls keep today's syntax (`impl Option { … }`,
`impl Stream8 {dom clk: Clock} { … }` — braces after the owner are
impl-level generics implicitly applied to the owner). Trait impls need
the binder *before* the self type, since the self type mentions it:

```polar
impl Add for uint8 { ... }                          // concrete (T2)

impl {param n: integer} Add for uint(n) {           // generic (T3)
    fn add(self, other: Self) -> Self = ...;
}

impl {param T: Bits} Bits for Pair(T) {             // bounded impl: the
    const width: integer = 2 * T::width;            // bound is the impl's
    fn pack(self) -> uint(width) { ... }            // where-clause, checked
    fn unpack(b: uint(width)) -> Self { ... }       // recursively by the
}                                                   // solver
```

So: `impl {binders} Trait for SelfType {sections?} where ...? { items }`.
The binder braces are distinguishable from the trait's argument braces by
position (immediately after the `impl` keyword) and content (`param`/`dom`
keywords). **[D2]** — alternative: keep binders after the self type as in
inherent impls, at the cost of `impl Add for uint(n) {param n: integer}`
reading right-to-left.

## Semantics

### Predicates and param envs

- `sig_of` grows a `predicates: Vec<Predicate>` on signatures
  (`Predicate::Trait(TraitRef)` now, `Predicate::Domain(Type, Domain)`
  when the redux work lands). A `TraitRef`'s args are kinded `Term`s —
  Type/Const/Domain — so traits over parametric types (`uint(n): Add`)
  need nothing special.
- The param env of a body is exactly the predicates written on its
  signature (and its enclosing impl). A new salsa query `param_env(def)`.
- The backend always solves in an **empty** env (everything concrete).

### Obligations and the solver

`ObligationKind` gains `Trait { goal: TraitRef, depth: u32 }`, queued:

- at every call/path instantiation: after `fresh_subst`, instantiate the
  callee's `predicates_of` with the same subst and enqueue (this rides the
  exact spot where `call_substs` is recorded today);
- on impl-candidate confirmation: the impl's own predicates, instantiated
  with the impl's inferred args, at `depth + 1`.

The solver is one function run from the existing fixpoint:

1. **Assemble** in a union-find snapshot: param-env clauses that unify
   with the goal; impls of the trait whose freshened header (self type +
   trait args, all kinds) unifies with the goal. Header unification only.
2. **Decide**: 0 candidates → "trait not satisfied" (if the goal still
   contains inference vars, stay pending — it may resolve later in the
   fixpoint; unresolved at the end → ambiguity error, like residual
   ConstEq handling). 2+ candidates → ambiguity error naming both. Param
   env + impl both applicable → error (no preference rule).
3. **Confirm** the unique candidate: replay unification for real, enqueue
   nested predicates. Depth > limit → overflow diagnostic with the
   obligation chain.

No caching in v1. If solving ever shows up in profiles, the
salsa-friendly shape is the new-solver/r-a one: canonicalize the goal,
cache per canonical goal — never cache inference-var-laden goals.

### Method dispatch

Extends the existing probe (`owner_of(recv)` → `impl_method` table):

1. Inherent methods on the resolved receiver win, unchanged.
2. Otherwise assemble trait-method candidates: trait methods named
   `method` from (a) bounds in the param env whose self type unifies with
   the receiver — this is how `a.add(b)` works when `a: T`, `T: Add` —
   and (b) impls whose self type unifies, checked applicable in a
   snapshot. Two applicable trait candidates → error naming both traits.
3. The pick is recorded in `method_resolutions`/`call_substs` as the
   *trait's* method def + substs (there may be no impl yet, e.g. bound-
   sourced picks). Receiver with unresolved type at probe time: defer to
   end-of-body and re-probe once, then error — same spirit as today's
   `Type::Error` bail-out, but with a diagnostic.

Trait-in-scope filtering (Rust's `use`-the-trait rule) is **skipped in
v1** — single crate, few traits; every crate trait is a candidate. The
module system makes it easy to add later if name pollution bites. **[D3]**

### Coherence

New salsa query per trait: collect its impls from the def map, pairwise
freshen both headers and unify in a scratch table. Unifiable → overlap
diagnostic on the second impl. Distinct const literals don't unify
(disjoint); a const/type param unifies with anything (overlap). Also per
impl: every trait item implemented, no extras, signatures match the
trait's declaration after substituting the impl's self type and args
(rustc's `compare_impl_item`).

### Associated consts

- New `ConstArg::Assoc { item: DefId, args: Vec<Term> }` — an
  *unevaluated* const, rustc's `ConstKind::Unevaluated`.
- While generic, equality is **structural only**: same item + unifiable
  args (`T::width ~ T::width` fine). Anything non-structural
  (`T::width + 1 ~ 1 + T::width`) rides the existing ConstEq obligation
  queue and defers — discharging at mono time when `T` is concrete, the
  impl resolves, and the const evaluates. This dodges the entire
  `generic_const_exprs` swamp: we never prove symbolic assoc-const
  arithmetic polymorphically, we check it per instantiation (the residual
  → SV `initial assert` pipeline already exists as a final backstop).
- `const_eval` learns one new step: evaluating `Assoc` = resolve impl
  (solver, empty env) → evaluate the impl's const body.

### Monomorphisation (`Instance::resolve`)

The backend currently takes `UserCall.def` as final. New rule: if the
resolved def is a **trait item** (method or const), re-select with the
call's now-concrete `call_substs` in the empty env. Coherence + concrete
args guarantee a unique impl; map trait item → impl item; compute the
impl's own args by unifying the impl header against the concrete trait
ref; compose with method-own args. The backend then proceeds exactly as
today — type-kind mono machinery (`mono_name`, `match_type`,
`build_subst`) already exists and is exercised by struct/port type params.

### Operators (the migration)

`a + b` desugars to the goal `lhs_ty: Add` + method call `add` — one
dispatch path, no builtin/trait split in inference. The prelude declares
the operator traits and the compiler synthesizes the builtin impls
(`impl {param n} Add for uint(n)`, `impl Add for integer`, …); lowering
recognizes *those specific impls* and emits primitive SV ops instead of
instances (rustc does the same: builtin ops select real libcore impls,
codegen special-cases them). Lang-item discovery is by known name in the
prelude — no attribute system needed yet. `is_prelude_op` and the
hand-rolled arith path in `infer_call` are deleted at the end of this
slice. Comparisons (`==`, `<`) arrive here as `Eq`/`Ord`-shaped traits
returning `bool` — they're also wanted by const-eval for clog2-class
functions. **[D4]**

## Staging

Each slice is independently committable with examples + fail-examples.
Status (2026-06): T1-T4 landed (5e297a5, f77d67c, 4324871, 2276fa7).
T5 (operators) is open on one architectural decision: where the builtin
operator impls LIVE — synthetic defs with programmatic signatures, or a real
prelude source file compiled into every crate (rustc's `core` route; pays
off again for numeric literals and Bits derives).
Still open from those slices: signature-level impl conformance (name-level
shipped; type-level needs Self-substituted sig comparison), and
declaration-level coherence for parameterised headers (two-sided header
unification — solve-time AmbiguousImpls covers uses meanwhile).

- **T1 — items.** Grammar: `trait` item (header + `fn` sigs +
  `const` items), `Self`, bound ascriptions (`param T: Add + Bits`),
  `where` clauses, binder-first trait impls. Item tree `Trait` item +
  trait-impl fields on `ImplItem`; `DefKind::Trait`; def-map
  registration + namespacing; fmt + highlighting + LSP go-to-def. No
  semantics — bodies using traits may still error.
- **T2 — concrete traits.** Trait-impl resolution in the def map
  (`trait_impls: trait_def → Vec<impl>`), coherence overlap + impl-item
  conformance checks, method probe step 2 for *concrete* receivers,
  backend dispatch through the picked impl method. Customer: a `port`
  with `impl Handshake for Stream8`.
- **T3 — bounds + solver.** `predicates_of`/`param_env` queries,
  `Obligation::Trait`, the three-step solver in the fixpoint, generic
  fns with bounds calling trait methods on `T`, generic + bounded impls,
  backend `Instance::resolve`. Customer: `fn buffer {param T: Reg}`-style
  generic component. This is the heart; T2 ships without it only to keep
  the diff reviewable.
- **T4 — associated consts.** `ConstArg::Assoc`, structural equality +
  ConstEq deferral, const_eval resolution, `T::width` in type positions.
  Customer: hand-written `Bits` impls for a struct, generic packer over
  `T: Bits`.
- **T5 — operators as traits.** Prelude operator traits + synthesized
  impls, binop desugar, delete the special cases; comparisons. Unblocks:
  numeric-literals design (own doc, replaces the lenient `integer ~ uint`
  arm), richer const-eval.
- **Later.** Default method bodies; supertraits (elaboration into param
  env); `derive(Bits)` for structs; built-in marker traits for the type
  zoo (`Pos`, linearity); trait-in-scope visibility; associated types if
  a customer appears.

## rust-analyzer lessons (researched separately; the salsa side)

What transfers from r-a's trait stack to ours, beyond the rustc shape:

- **Inference vars never cross a query boundary.** `infer(def)` stays
  self-contained (we already obey this); if a solve ever needs to be its
  own query, canonicalize the goal first.
- **Do NOT memoize per-goal solves in salsa.** r-a hit this twice: salsa
  bookkeeping per canonical goal is a memory pathology. Their mature
  position: solver-internal provisional cache + (if ever needed) a
  revision-scoped side cache. For Polar's goal volume, no cache at all is
  the right v1.
- **Impl maps are a per-crate query keyed by trait, with a self-type
  fingerprint (head constructor) index** — candidate assembly is a
  fingerprint lookup, never a whole-crate impl scan.
- **Impl-header edits invalidate crate-wide inference; body edits must
  not.** Headers live in the item tree/def map; bodies behind the body
  query — our existing firewall, keep it as trait items land.
- **Obligations carry provenance from day one** (origin span, the
  instantiated bound, derivation parent). r-a couldn't report
  unsatisfied-bound errors for *years* because "no solution for canonical
  goal" has no story attached. Our Obligation already carries a span;
  trait obligations also get a cause chain.
- **Don't fork solver semantics from the reference implementation** —
  chalk's translation layer rotted. (For us: one solver, used by both
  infer and the backend's Instance::resolve, never two.)

## Decisions (settled 2026-06)

- **[D1]** Bound syntax: `param T: Add + Bits` — ascription = kind + bound.
- **[D2]** Binder-first impls, for trait AND inherent blocks (landed).
- **[D3]** v1 method dispatch considers all crate traits; no
  trait-in-scope rule yet.
- **[D4]** Operators migrate in T5.
- **[D5]** Default methods / supertraits deferred past v1.
- **[D6]** `.reg` stays builtin until type-zoo marker traits exist.
