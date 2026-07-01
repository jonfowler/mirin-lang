# Traits and dispatch

A method call's target is not knowable from syntax: `x.add(y)` means different
code for a `uint` than for a `bits`, and which it means depends on the type of
`x`. Inference resolves it, through the same trait machinery that discharges a
`where` clause. This chapter covers how a method call is dispatched, how a trait
bound is solved, and why operators are ordinary trait methods.

## Resolving a method call

Once inference knows the receiver's type, it resolves the call in three tiers,
stopping at the first that answers:

1. **Inherent.** If the receiver type's own impl defines the method, that is the
   resolution.
2. **Trait impls.** Otherwise inference gathers every trait impl for the receiver
   type that defines a method of this name, and keeps those whose `Self` header
   matches the receiver. One match resolves the call; two are an ambiguity error;
   none falls through.
3. **The param environment.** If the receiver is a type parameter, inference looks
   to the definition's own bounds — a `T: Add` in scope means `T.add(…)` resolves
   to the trait's method.

Header matching ignores domains: trait impls are domain-blind, and the clock flows
through the resolved method's own signature instead. A resolved call records its
callee and the call's generic substitution, which the MIR later bakes onto the
node — so no dispatch survives into the MIR.

## Builtins resolve to no def

Four methods — `reg`, `posedge`, `replace`, `enumerate` — are typed structurally
by inference and resolve to *no* definition. Their absence from the resolution
table is deliberate: it is exactly how the MIR and backend tell a builtin from a
dispatched call. The MIR carries them as a closed set of `Builtin` nodes rather
than calls, so adding a builtin means teaching the MIR about it too.

## Solving a trait bound

A callee's `where` clause becomes a trait obligation at each call site (one of the
four obligation kinds from [inference](inference.md)), instantiated with that
call's substitution and discharged at the body's fixpoint. Solving mirrors
dispatch:

- a **concrete** self type is matched against the trait's impl headers — no match
  is an unsatisfied bound, two is an ambiguity, one confirms the impl;
- a confirmed impl brings its *own* bounds, which nest as fresh obligations a
  level deeper and solve recursively (a depth cap turns an accidental cycle into
  an error);
- a **parameter** self type is discharged from the param environment, the bounds
  the definition itself declares.

## Operators are prelude traits

Mirin has no built-in arithmetic. `a + b` desugars to `a.add(b)`, `a == b` to
`a.eq(b)`, and so on; the operator traits and their implementations for the
builtin types are ordinary Mirin source in the prelude (as of 2026-06). So
resolving an operator *is* method dispatch, and constant-folding one is just
evaluating the resolved prelude method — which is how [constant evaluation](const-eval.md)
handles arithmetic.

## Generic receivers defer to monomorphisation

When the receiver is a type parameter, the call resolves to the trait method's
*declaration*, not a concrete impl — there is no concrete type yet to select one.
The impl is chosen later, once monomorphisation grounds the parameter to a real
type. Dispatch, in other words, finishes in two stages: inference settles it for
concrete receivers and defers it for generic ones, and the backend closes the
deferred case when it instantiates the call.
