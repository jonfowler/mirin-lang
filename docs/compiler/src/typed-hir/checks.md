# Well-formedness checks

Inference settles types and domains; a few more checks finish the front end. They
read the body but little else, and they enforce things specific to hardware —
every signal driven once and only once, and connections that agree with port
directions. Each is its own query, so a language server can run them
independently.

## Every signal has exactly one driver

In hardware, two things driving one wire is a short. So `check_drivers` requires
that every `var` is driven *exactly* once — by a driving equation or an
out-connection. An undriven `var` and a multiply-driven one are both errors.

Driving a value in pieces is still allowed: a struct may be wired field by field,
as long as the pieces are disjoint and together cover the whole. Checking that
*coverage* needs types — the field set comes from the type — so it runs after
inference, as `completeness`: a field-driven aggregate must cover every leaf, and
an `out` parameter must be driven at all (an `integer`-typed `out`, being
compile-time-only, is exempt). Together the two checks enforce one invariant:
every signal leaf has exactly one driver — except a `let mut` accumulator, which
is reassigned in place sequentially rather than driven, so its repeated writes are
not competing drivers.

## Connections agree with directions

A port field and a directed parameter have a direction, and the connection
operator must match it: `=` drives a value *in*, `=>` reads one *out*. The
`directions` check confirms a call's operators agree with the callee's declared
directions, so an `out` field cannot be fed a value or an `in` field read from.

(One direction check is *not* here. Pairing the field directions of two connected
ports is a flatten-time concern — it depends on how a port's leaves split — so it
waits for the backend rather than this per-definition pass.)

## Inline bodies stay in their scope

An `#[inline]` function is spliced into its caller rather than emitted as a
module, and only a combinational, value-returning body can be spliced today.
`inline_check` enforces that boundary up front: it rejects a clocked body (`when`,
`.reg`), a `var`, an `out` parameter, an integer parameter, and recursion. (A
`const if` is *not* rejected — whether it folds is a property of the call site, so
it is left to ground at the splice.) Catching these here means an unsupported shape is reported against the
function the user wrote, rather than surfacing as a confusing failure deep in the
backend splice.

With these checks passed, the front end is done: the body is name-resolved,
typed, soundly driven, and correctly connected. The next part turns it into the
[MIR](../mir/mir.md) — the typed mid-level IR the backend lowers from.
