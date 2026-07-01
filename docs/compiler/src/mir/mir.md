# The MIR

With the front end done, the compiler lowers each definition to the **MIR**: a
typed mid-level IR between the HIR and SystemVerilog. The HIR left types in a
side-table and method dispatch unresolved — the right shape for inference, the
wrong one for transforms. A pass that desugars a slice, monomorphises a call, or
splices an inline body wants the type *on the node* and the dispatch already
settled. The MIR bakes both in. This chapter covers what the MIR is, the lowering
that builds it, and what it does and does not yet host.

This is rustc's HIR→MIR step, and the move is a known one: rust-analyzer — also
salsa-based, also living off an inference side-table — built a MIR with no codegen
at all, purely for analysis. A backend is a consumer of the MIR, not the reason
for it.

## A typed, derived IR

`mir_of(def)` reads `body` and `infer` and rebuilds the definition as fresh nodes,
each carrying its resolved type. Because the MIR is *derived* — recomputed from
the HIR, never an input — embedding types in it costs nothing incrementally,
exactly as rustc keeps its `TypeckResults` separate from the MIR it bakes them
into. The MIR mirrors the HIR body one node at a time, with the resolved type
welded on, so a transform never has to reach back into the `ExprId`-keyed
inference table: the type is local.

## What the MIR resolves

Lowering does more than copy the body. It settles four things the HIR left open:

- **Dispatch.** The four HIR call shapes — plain call, method call, type-path
  call, and operator — collapse into one `Call` node carrying the resolved callee
  and its generic substitution. No method dispatch survives into the MIR.
- **Builtins.** The four methods that resolve to no definition (`reg`, `posedge`,
  `replace`, `enumerate`) become a closed set of `Builtin` nodes — recognised
  exactly by the absence of a resolution in `infer`, so the set is fixed and a new
  builtin must be taught to the MIR.
- **Drive targets.** An equation's left side becomes a `Place`: a base local plus
  a chain of `Field`/`Index`/`BitRange` projections. Slicing desugars here, where
  the operand's type is on hand to direct it.
- **`const if`.** A compile-time conditional folds to its taken branch, so the
  discarded arm — which may be ill-formed for this instantiation — is never
  emitted.

## Lowering is total

`mir_of` is structural and total, with its negative space made explicit. On a
well-typed body, any shape the lowering does not handle is a compiler bug, so it
`panic!`s at the exact site rather than emitting something wrong. On a malformed
body — one that already carries diagnostics — those same shapes can legitimately
appear from error recovery, so there the lowering degrades to a `Missing` node
instead of crashing. A `well_typed` gate, set from the body and inference
diagnostics, chooses between the two regimes: loud past the gate, soft before it.

## What the MIR does not yet host

The MIR was introduced to host the heavy MIR→MIR transforms — aggregate
flattening, monomorphisation, and inline splicing. Those do not run on the MIR
yet; they still run in the backend, which reads the MIR as its lowering source but
also reaches back into the HIR for a few things (verilog templates, the inline
splice). So the MIR is the backend's *primary* source, not yet its only one, and
moving flatten / mono / inline onto it as proper passes is unfinished work (as of
2026-06).

That is the MIR: a derived, typed, dispatch-resolved mirror of the body, total by
construction. The next part lowers it to SystemVerilog.
