# Const/net duality: SV functions and parameters as first-class lowerings

Status: PARTLY LANDED (2026-06-21). Stages 0–2 are implemented (see "Landed"
below); stages 3+ (SV functions in *net* position, the deferred const-type
checker) remain design. The original draft is kept below for context, with the
scope corrections from the 2026-06-21 review folded in.

## Landed

- **Stage 0** — silent backend coercions eliminated; symbolic widths/lengths
  render via `render_const_sv`, anything unrenderable is a hard error (commit
  `45bedc5`).
- **Stage 1** — `= verilog expr { EXPR }` body form; operators are ordinary
  inline-expression fns spliced via `inline_call`/`render_inline`;
  `prelude_op`/`prelude_unary`/`receiver_is_signed` retired (commit `82a7aa4`).
- **Stage 2** — a const-only `fn` called in a **constant position** lowers to an
  **in-module SV `function`** and the const local it feeds to a `localparam`
  (`examples/working/const_fn_localparam.mrn`). Mechanics: a symbolic `let w =
  f(N)` is promoted to `localparam int w = f(N);`; `ConstArg::Local` uses of a
  promoted local render via a new backend-only `ConstArg::Symbol(name)`; the
  callee body lowers to `function automatic int …` (the same procedural shapes a
  fold uses). `const_eval` now treats `ExprKind::ConstParam` as symbolic so such
  a width *defers* to the elaborator instead of being rejected.

### Scope correction (2026-06-21 review)

The placement question (where SV functions live — `$unit`, package, or
in-module) resolved to: **everything is a module by default** (net-position
calls stay module instances, no corpus churn), and an **in-module `function` is
generated *only when required*** — i.e. a fn applied in a constant/localparam
position, which a module instance cannot occupy. Simple integer math (`clog2`,
`n + n`) stays an **inline SV const expression**, never a function. The key SV
constraint that forces in-module placement: a function takes its widths from its
*enclosing scope's* parameters (an argument can't size a declaration), so a
parametric-width function must see the module's `#(parameter n)` — only possible
inside the module. Duplication across callers is accepted (complex const math on
parameters is rare). This supersedes the `$unit`/package options and the
"two lowerings for the same fn in net position" framing from the draft below.

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

## Item 2 — `@const` leaves become constants (never nets)

**Every `@const` leaf is a compile-time constant in SV — never a net.** An
earlier draft proposed lifting to a parameter *lazily* (only when used in a
type). That was wrong: a `@const` value is already forbidden by the type system
from ever carrying a runtime driver, so representing it as a net buys no
permissiveness and only loses const-position usability. There is no program that
compiles only because a `@const` stayed a net. So the decision is not *whether*
to make it a constant, only *which constant form* — and that is **not tied to
"width"**; a `@const` value is usable in every elaboration-time position (width,
`Vec` length, `for`/`range` bound, generate condition) uniformly.

**The form is decided per leaf**, by concreteness + where it originates:

| leaf domain | originates as | SV home |
|---|---|---|
| `@const`, concrete, unnamed | anywhere | inline literal (`[7:0]`) |
| `@const`, symbolic | generic / value param | `#(parameter)` |
| `@const`, symbolic | body local | `localparam` (or inline const-expr) |
| clocked | param | net **port** |
| clocked | body local | net (`logic` / `assign`) |

- **Interface vs internal (`#(parameter)` vs `localparam`) is structural, not
  metadata.** A param/generic flattens into the module *interface*; a body local
  lowers *internally* — already distinct code paths. So "is this a module
  argument" is kept by construction.
- **Per-leaf is the load-bearing requirement.** A Mirin aggregate can mix const
  and clocked leaves — `enumerate`'s `(integer @const, A @D)` is the canonical
  case, and this must be supported. Each leaf routes to its own home: a
  mixed-const **argument** legitimately spans `#()` *and* the port list, which is
  fine because flatten already explodes one arg into many leaves. No boundary
  restriction.
- **The one mechanism needed:** the per-leaf `@const` lives on the resolved
  `Type` (`Domain::Const` per `Type::Value`) **before** flatten, but
  `flatten_leaves` discards domain (its `Leaf` has no domain field) and
  `ground_widths` collapses `ConstArg::Local` to a literal during flatten. So
  `Leaf` must **carry per-leaf domain**, and a symbolic `@const` local must be
  promoted (to a `localparam`/const-expr) *before* `ground_widths` erases its
  identity. The width-shaped `width_locals`/`ground_widths`-to-literal path is
  the narrow thing this uniform `@const → constant` rule replaces.

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
2. Carry per-leaf domain through `flatten_leaves`; route every `@const` leaf to
   its constant home (parameter / localparam / inline literal — never a net),
   promoting symbolic locals before `ground_widths` collapses them.
3. SV `function` IR + purity classifier; lift pure native fns; add the FFI purity
   marker.
4. `localparam` emission for named/shared symbolic consts (`let w = a+b`).
5. Deferred const-type checking (item 3) — concrete-instance discharge + lazy
   residuals, loop body checked symbolically.

## Resolved (2026-06-20)

- **All `@const` leaves are constants, never nets** (no lazy lifting). Form is
  per-leaf: literal (concrete) / `#(parameter)` (interface) / `localparam`
  (derived symbolic).
- **Interface-vs-internal is structural** (param/generic → interface, body local
  → internal), so it needs no extra metadata — only per-leaf *domain* does.
- **Mixed-const aggregates are supported everywhere**, including as arguments (a
  single arg's leaves split across `#()` and the port list).

## Open questions

- `Leaf` carries per-leaf domain — as a `Domain` field, or a reduced
  `is_const: bool`? (Affects every `flatten_leaves` consumer.)
- SV `function` vs `localparam` for a non-trivial symbolic const body: a constant
  function called in `localparam W = f(n);`, or inline? When is each preferred?
- Exact purity boundary: does an `#[inline]` callee count as "calls a module"?
  (It splices, so no — but transitive purity must follow the splice.)
- For-loop symbolic checking (item 3): what obligation language is expressive
  enough to check a body once, parametric over the genvar?
