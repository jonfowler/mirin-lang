# Monomorphisation and checking

A generic definition is not one module. `fn add{T}(…)` called at `uint(8)` and at
`uint(16)` needs two modules, because SystemVerilog has no generics over types.
This chapter covers how the backend specialises a generic definition across its
call sites, how it assembles the whole file, how it decides the ground constraints
inference deferred, and the name collisions it must hard-error on.

## Specialising generics

Mirin splits its generic parameters the way rustc does, and the split decides what
gets specialised:

- **Type-kind generics specialise.** At each call site the backend reads the
  concrete argument types and requests a specialised module, named for the
  instance (`Callee__uint8`). A worklist drives this: emitting a module collects
  the specialisations it calls for, which are themselves emitted, and so on.
- **Const-kind generics stay polymorphic.** A width parameter does *not* fork a
  module; it becomes a SystemVerilog parameter (`#(parameter int N)`), bound per
  instance at the call. One module serves every width.

The backend resolves a trait-method call late: when the receiver was a type parameter,
inference recorded the trait method's *declaration*, and the backend re-selects
the concrete impl once the specialised type is known, composing the impl's
bindings with the call's own generics.

## Assembling the file

`sv_file` assembles the modules. It first gates on the front end: if any query —
syntax, names, signatures, bodies, inference, the driver and completeness checks —
reported a diagnostic, it emits nothing, so a broken crate never produces
half-built Verilog. Otherwise it emits the concrete (non-generic) modules in
source order, then drains the monomorphisation worklist for the specialised
copies, dedups them, and sorts them by name. The ordering is deliberate: the
emitted file is byte-for-byte deterministic across runs.

## Deciding ground constraints

Inference carried some constraints forward as residuals — a width equality, a
literal fit — because it could not decide them while the generics were open.
`mono_check` decides the ground ones, and adds one check of its own: that every
width is non-negative. It walks every call site and, where the call's concrete
arguments make a residual ground, evaluates it and reports the failures: mismatched
widths, a literal that overflows its type, a width that came out negative. It composes substitutions down the call
chain, so a constraint buried in a callee that only grounds once an outer call's
literals flow through is still caught.

`mono_check` is **reporting-only**: it does not gate emission. A ground violation
is a hard error to the user, but the other modules still render, and a constraint
it cannot make ground simply does not fire — the symbolic `initial assert` from
the [previous chapter](lowering.md) guards the parameter-level cases at
elaboration. One gap remains: a constraint that is neither ground nor reducible to
bare parameters — a compound symbolic width that never grounds — is checked by
neither path today.

## Reserved-word collisions

Because the backend synthesises names — flattened leaves, specialised modules,
fresh blocks — a generated identifier can collide with a SystemVerilog keyword.
The emitter checks every module, port, parameter, and signal name against the
reserved-word table and hard-errors on a collision, rather than emit Verilog that
will not compile.

With the modules emitted, specialised, and checked, the compiler has produced its
SystemVerilog — the end of the pipeline this book has followed from source text.
The one thing left to keep legal is the boundary case where a width is zero, the
next chapter.
