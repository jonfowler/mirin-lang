# Constant evaluation

A width is a const expression, and sooner or later it has to become a number — to
check that two widths are equal, or to emit a bus of a definite size. Reducing a
const expression to a value is **constant evaluation**. This chapter covers the
evaluation model, what inference uses it for, and the two evaluators that
currently exist — one of which is meant to replace the other.

## Demand-driven, per output

Constant evaluation is Rust's CTFE in shape — an interpreter run on demand — but
with one structural difference. A Mirin body is an equation system, not a sequence
of statements, so there is no stepping from top to bottom. Instead, evaluating a
local means *finding what drives it* — its `let`, its driving equation, or the
call out-connection that produces it — and evaluating that. Results are memoised
per call frame, with an in-progress marker that turns a definitional cycle into a
clean failure rather than a loop.

A callee's `out` parameters are evaluated lazily, as
thunks. Calling a function to read one output never evaluates the others.

## Three outcomes

Evaluating a width has three results, and the difference between two of them
matters:

- **a value** — the expression folded to an integer;
- **symbolic** — it bottomed out on a free generic parameter, so it cannot be
  decided *yet*, but might be once a call supplies the parameter;
- **failed** — it is closed but still has no value, such as a divide-by-zero or an
  overflow.

Symbolic defers; failed is a hard error. Keeping them apart is what lets inference
carry an undecidable width forward as a residual (the optimistic checking of the
[Overview](../architecture/overview.md)) instead of rejecting a perfectly good
generic body.

## Two evaluators, consolidating

There are two const evaluators today.

- **The HIR evaluator** walks the body HIR. Inference uses it to ground the width
  and fit obligations at its fixpoint, and the type-level width axis depends on it.
- **The MIR evaluator** walks the [MIR](../mir/mir.md). The backend uses it for the consts it needs
  — slice endpoints, `const if` conditions — where having the full typed
  expression tree is worth more than the HIR's lossy const form.

They share a single value/arithmetic/projection core, so they agree on what an
expression means; they differ only in which tree they read. The intent is to
retire the HIR evaluator in favour of the MIR one, leaving a single evaluator over
a single typed IR. That consolidation is **not yet done** — inference still runs
on the HIR evaluator, and moving it requires inference to read const facts off the
MIR. Until then, both exist, and a change to constant-folding behaviour has to
land in both.

Evaluation is what turns inference's optimistic obligations into decisions.
The ones it can only mark symbolic are decided later, on ground instances, by the
monomorphisation checks the backend runs — or, where even those cannot decide,
left to a runtime `initial assert`.
