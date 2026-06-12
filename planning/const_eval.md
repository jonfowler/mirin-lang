# Const eval (Q4c)

Compile-time evaluation of `@const` values: width arithmetic (`uint(n + 1)`),
struct-valued configuration (`uint(cfg.bits)`), and const functions
(if/else, `out`-param fns). This doc is the design; the examples in
`examples/working/const_*.plr` are the acceptance tests.

## The rustc analogy

rustc's CTFE: `ty::ConstKind::Unevaluated` consts reference an anonymous body
and stay lazy until a query (`const_eval_*`) interprets the MIR on demand,
keyed by `(DefId, args)`; structural results become valtrees. We follow the
lazy-until-demanded shape and the "consts carry a body reference" idea, with
two deliberate divergences:

1. **No separate const IR.** Polar fn bodies are equation systems, not
   statement lists — the body HIR is already the thing to interpret. A width
   expression lowers to a small `ConstArg` tree whose `Local` leaves point
   into the enclosing body; everything bigger (calls, if/else) is reached *by
   demanding a local*.
2. **Demand-driven, not sequential.** rustc's interpreter steps a CFG. A
   Polar fn with `out` params has no execution order — outputs are equations.
   So evaluation is *per-output thunks*: demanding an out param (or any
   local) finds its defining `let` / driving equation / out-connection and
   evaluates that expression, memoized, with an in-progress marker for cycle
   detection ("const evaluation cycle").

## What is const?

The domain system already decides: the const fragment is exactly the
`@const`-domain values. `ConstDomain` obligations (a width local must be
`@const`) gate entry; the evaluator never sees clocked values.

## Representation (`ConstArg` grows a tree)

```rust
enum ConstArg {
    Lit(i128), Param(u32), Local(LocalId), Infer(InferVar), Error, Deferred,
    Op(ConstOp, Box<ConstArg>, Box<ConstArg>),   // + - *  (uint(n + 1))
    Field(Box<ConstArg>, String),                // uint(cfg.bits)
}
```

Width positions admit literals, names, arithmetic, and field chains. A
*call* in width position stays `Deferred` — write `let w = f(…); uint(w)`;
the evaluator's full power (calls, if/else, out-params) is reached through
`Local` leaves, keeping the tree small. `integer` is the const scalar:
arbitrary-size signed (i128 today), so intermediates may go negative; a
**negative evaluated uint width is an error** at discharge.

## The evaluator

`hir/const_eval.rs`, a plain function over `body()`/`infer()` results (the
calling queries are tracked; evaluation is deterministic from their inputs,
so it needs no salsa key of its own yet — revisit if profiles say otherwise).

```
Value  = Int(i128) | Bool(bool) | Record(DefId, Vec<(String, Value)>)
Frame  = { def, bindings: LocalId -> Slot }       Slot = Todo | Evaluating | Done(Value)
```

- `demand(frame, local)`: Param → caller-bound value; Let → eval its value
  expr; Var → eval the RHS of its driving equation, or, when driven by a call
  out-connection (`f(x, => l)`), eval that call's out-param thunk.
- `eval(frame, expr)`: literals, locals (→ demand), field projection on
  Record values, record construction, `if` (eval cond, take one branch),
  prelude ops (`+ - *` on Int), user-fn calls.
- A user-fn call makes a fresh frame: in-args eagerly bound (cheap, pure);
  each **out param is a thunk** — demanded only if the caller uses that
  target. The return value is the body tail / `return`, also by demand.
- Guards: per-frame `Evaluating` markers (cycles), a frame-depth cap, a step
  budget. All failures are soft (`None`) — callers fall back to symbolic.

**Asserts under laziness** (future): when the language grows const asserts,
a thunked-out output could skip one. The call rule then becomes: demand every
*assert-bearing* statement of an entered frame, even if no output needs it.
Pure-value code keeps full laziness.

## Integration

- **infer**: `ConstEq` obligations at the end-of-body fixpoint try the
  evaluator (no symbolic `Param`s bound). Both ground → equal discharges,
  unequal is a `WidthMismatch`. Anything symbolic stays a residual exactly as
  today. A ground negative width → `NegativeWidth` diagnostic.
- **backend**: `sv_type` grounds width trees through the evaluator; a still-
  symbolic tree over generic `Param`s renders as an SV parameter expression
  (`[(N+1)-1:0]`). Monomorphisation is untouched: const params stay symbolic
  per `parametricity.md`; eval fires only when values are ground.

## Acceptance examples

- `const_arith.plr` — `let n = 3; let x: uint(n + 5)` → `[7:0]`.
- `const_record_config.plr` — `uint(cfg.bits + cfg.extra)` through a struct
  value.
- `const_fn_if.plr` — `let w = pick(false, 8, 16); uint(w)` forcing branch
  evaluation in a callee.
- `const_out_params.plr` — `widths(3, => narrow, => wide)`; each out param
  is an independent thunk; `uint(narrow + wide)`.
- `fail-expected/negative-width.plr` — `uint(n - 3)` with `n = 1` → error.
