# Const/net duality: SV functions and parameters as first-class lowerings

Status: DRAFT (2026-06-20). Design only — nothing implemented yet. Item 4
(eliminating silent backend coercions) is being investigated separately in a
worktree; it is the prerequisite hygiene this design assumes.

## The problem

A value computed in Mirin must sometimes live in Verilog's **constant space**
(a `localparam`, a `#(parameter)`, a width/length/dimension) and sometimes in
**net space** (a `logic`/`assign`). The two are not interchangeable in SV: you
cannot use an `assign`-driven net where a constant is required, and you cannot
`assign` to a parameter. Today every Mirin `fn` lowers to a `module` with
`assign`/`always`, and there is no SV `function` or `localparam` in the IR at
all — so a symbolic value needed as a constant either panics (`uint(a+b)`),
miscompiles silently (`Vec(a+b)` → `[0:0]`), or errors (`let w=a+b; uint(w)`).

The situations that force this: a **parameterised argument**, a **for loop over
a symbolic bound**, or any **symbolic integer/type** that flows into both a width
and a datapath. Solving it generally — both `localparam` and SV `function`
generation — pays off broadly, so it is worth doing properly from the start.

## Principle

The two SV forms are renderings of the same pure computation. A definition or
value should be representable in **both** spaces whenever it legally can be, and
the *kind* (parameter vs net) chosen by how it is **used**, as lazily as
possible. SV is forgiving here: the synthesisable expression grammar is shared
between constant and net contexts, and **SV evaluates constant expressions at
elaboration** — so Mirin can usually *render a symbolic const expression and let
the elaborator compute it*, rather than evaluating it itself.

## Rust analogy (and where HDL diverges)

- A pure, const-usable definition is Rust's **`const fn`**; the purity check is
  `const fn`'s body check. One body, two evaluators = Rust's **CTFE (Miri) vs
  codegen** — Mirin already has this split (`const_eval` interprets bodies; the
  backend lowers them).
- "Lift a value to a parameter because a type reads it" is rustc's
  **const-qualification / promotion** — use-driven.
- Symbolic const expressions (`uint(a+b)`) are rustc's **`generic_const_exprs`**,
  which is *incomplete* in Rust because it must normalise/decide const equality.
  **Mirin's divergence and advantage:** it can leave such expressions
  unevaluated and emit them for the SV elaborator, so it needs its own evaluator
  *only where it must decide something at compile time* (type equality, bounds).
  That decision problem (item 3) is the genuinely hard part; the rendering is not.

## Item 1 — lift modules to SV functions

A Mirin `fn` lifts to a pure SV `function` iff it is **time-independent**: no
`when` clause, no `.reg`/register, and it calls nothing that is itself a module
(transitively pure). This is a cheap body scan — the same predicates the backend
already uses locally (`as_reg`, `Stmt::When`, the `UserCall` set) — there is no
existing classifier, so this is a new (small) traversal.

**Shape-preserving, per Jon:** SV functions allow a `void` return and `output`
arguments, so a SV `function` can take the *exact same form* as a Mirin fn —
out-params → function `output` args, named/multi results → `output`s, a single
return → the function return. So lifting is structural, not restricted to
single-scalar-return fns. This is the lever that makes a general solution cheap.

- **Native fns:** infer purity (the body is analysable); no annotation needed.
- **Inline `= verilog` bodies:** opaque text — the compiler *cannot* infer.
  These must be **explicit** (see "Inline verilog" below).

A pure fn then has two lowerings — SV `function` (callable in const *and* net
contexts) and, where it must instantiate, a module — chosen by use. A stateful
fn stays module-only.

## Item 2 — lift values to parameters (lazily)

Default a value to **net**; lift it to a parameter **only when a use requires
it** — i.e. it appears in a type/width/length/const-arg position. Making an
argument a parameter is *stricter* (it must be elaboration-constant) than leaving
it a net, so over-lifting needlessly constrains callers.

The trigger already exists: `infer::width_locals` collects every
`ConstArg::Local`/`ConstArg::Param` referenced in a type, and already *requires*
those locals' domain to be `@const`. That set is exactly "must be a parameter."

**Granularity is per-leaf, not per-value** (Jon's point): a Mirin aggregate can
mix const and clocked leaves — e.g. `enumerate`'s `(integer @const, A @D)` — so
a single struct/tuple may need *some* leaves as params and others as nets. The
per-leaf `@const` lives on the resolved `Type` (`Domain::Const` per
`Type::Value`) **before** flatten, but `flatten_leaves` currently discards
domain (its `Leaf` has no domain field) and `ground_widths` collapses
`ConstArg::Local` to a literal during flatten.

## Item 3 — defer the const-type-checking problem

Const-equality/validity checks (`uint(a+b)` vs `uint(b+a)`, bounds,
width-fit) should be **deferred until the instance is concrete** (monomorphised),
where the symbolic params are literals and the check is trivial.

The hard case is a **for loop over a symbolic bound**: we deliberately do *not*
unwind it (that would defeat folded loops / explode codegen), yet we must still
guarantee the body's const obligations hold for *every* iteration without
instantiating each. So the check has to be parametric over the loop variable.
The aim is a system whose checks are **exhaustive but maximally lazy**: discharge
what's concrete immediately, carry the rest as residual obligations (the existing
trait `ConstEq`/`Unevaluated` machinery is the model), and force them at the
nearest concrete instantiation — with the loop body checked symbolically once
rather than per iteration. Left for later; this doc only flags it.

## Inline verilog — what to add

Two distinct needs, both because a `= verilog { … }` body is opaque to analysis
(it is structured only as `VerilogSegment::{Text,Param,Dom,ResultPort,Const}`):

1. **An expression-form body** — kind-agnostic. Today operators are written
   `fn add(self, other: Self) -> Self = verilog { assign ${return} = ${self} + ${other}; }`,
   which hardcodes net space (the builtins only escape it because `prelude_op`
   secretly re-recognises them as inline `BinOp`s). Replace with an expression
   body, e.g.:
   ```
   fn add(self, other: Self) -> Self = verilog expr { ${self} + ${other} }
   ```
   whose RHS the backend renders as an `assign`, a `localparam`, or inline,
   depending on use. This generalises and retires the `prelude_op` special case.

2. **A purity / function marker** — for non-expression opaque bodies, an explicit
   assertion that the body is a pure function (a SV `function`, usable in
   constant contexts) rather than a module. The natural home is a per-def
   attribute alongside the existing `#[inline]` flag (`DefData.inline` is the
   precedent for a per-def lowering-strategy marker carried from the item tree).

For native fns the marker is unnecessary (purity is inferred, item 1); the marker
exists for the opaque-FFI case, and optionally as an *assertion* on a native fn
(error if the body turns out stateful — like `const fn`).

## Where the stage fits

There is **no discrete flatten pass** to slot behind — flattening is the inline
`flatten_leaves` helper and monomorphisation is a call-site worklist in
`sv_file`. So the work is not "a pass after flatten" but three threaded changes:

1. **Carry per-leaf domain through flatten.** `Leaf` must grow a notion of "this
   leaf is const (a parameter)" — read `Domain::Const` (and `ValueKind::Integer`)
   off the `Type` as `flatten_leaves` descends, instead of discarding it. This is
   the structural enabler: once leaves know their kind, port/decl emission can
   render const leaves as `#(parameter)`/`localparam` and clocked leaves as nets,
   *per leaf*, which is exactly what mixed aggregates (`enumerate`) need.

2. **Mark "must be a parameter" before `ground_widths`.** The `ConstArg::Local`
   set from `width_locals` must be consumed before grounding collapses it to a
   literal — that is the point a body local gets promoted to a `localparam`
   (today there is no path from a body local to an SV parameter at all).

3. **Classify fns pure/stateful before module-building.** A small body scan
   (item 1) decides `function` vs `module` lowering; pure fns gain a `function`
   emission path (a new `SvItem`/`SvFunction` in the IR, which does not exist
   yet).

So conceptually it is a post-type/pre-emission **kind-qualification** over leaves
and defs, but mechanically it lands *inside* flatten (carrying domain) and at the
`width_locals`/`ground_widths` boundary, not as a separate pass.

## Suggested staging

0. **(prereq, in flight)** Eliminate silent backend coercions — render symbolic
   const widths/lengths via `render_const_sv`, fail loudly otherwise. (item 4)
1. `verilog expr { … }` body form + retire `prelude_op` → operators kind-agnostic.
2. Carry per-leaf domain through `flatten_leaves`; emit `@const` leaves as
   parameters where a use requires it (consume `width_locals` pre-grounding).
3. SV `function` IR + purity classifier; lift pure native fns; add the FFI purity
   marker.
4. `localparam` emission for named/shared symbolic consts (`let w = a+b`).
5. Deferred const-type checking (item 3) — concrete-instance discharge + lazy
   residuals, loop body checked symbolically.

## Open questions

- Should a leaf's "is-param" be a field on `Leaf`, or a separate per-leaf
  domain carried alongside? (Affects every `flatten_leaves` consumer.)
- SV `function` vs `localparam` for a non-trivial symbolic const body: a constant
  function called in `localparam W = f(n);`, or inline? When is each preferred?
- Exact purity boundary: does an `#[inline]` callee count as "calls a module"?
  (It splices, so no — but transitive purity must follow the splice.)
- For-loop symbolic checking (item 3): what obligation language is expressive
  enough to check a body once, parametric over the genvar?
