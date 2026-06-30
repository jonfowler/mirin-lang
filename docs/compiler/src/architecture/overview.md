# Overview

This chapter is the overall picture: how a `.mrn` file becomes SystemVerilog, and
how the compiler's pieces fit together. Later chapters take each piece in turn.

Mirin's compiler borrows heavily from Rust's: it is staged into the same kinds of
intermediate representation as rustc — a high-level HIR and a mid-level MIR — and
it runs as a demand-driven *query engine* in the manner of rust-analyzer rather
than as a fixed sequence of whole-program passes. It departs from Rust in two
ways worth flagging up front. It tracks the **clock domain** of every value as
part of that value's type and rejects unsynchronised crossings — a safety check
most HDLs leave to external tools. And it checks generic code **optimistically**:
inference assumes a body's width and const constraints hold, and a later pass,
`mono_check`, settles them once a concrete call site grounds the generics. Those
traits — the query architecture, domain checking, and optimistic checking — shape
the rest of this guide. We take them after the pipeline itself: first the path
your code travels from text to Verilog, then the engine that moves it along.

## How the compiler transforms your code

Source text becomes Verilog through a sequence of representations:

```
.mrn text
  ─►  syntax tree     parse: tree-sitter builds a concrete syntax tree
  ─►  resolved names  what each name refers to, across the whole crate
  ─►  typed HIR       a lowered body per definition, with a type and a
                      clock domain inferred for every expression
  ─►  MIR             a typed mid-level IR, dispatch resolved, slicing desugared
  ─►  SystemVerilog   flatten aggregates, monomorphise generics, emit
```

The compiler **parses** the source into a concrete syntax tree that preserves the
text exactly. It **resolves names**, working out across the whole crate what every
identifier refers to — which function a call names, which type an annotation
means. It lowers each definition to the **typed HIR** and runs inference,
assigning every expression both a type and a clock domain; this is where most of
Mirin's checking happens, the domain analysis included — though it defers the
width checks it cannot yet decide, as the next section explains. The **MIR** bakes those
inferred types onto the nodes and resolves what inference left abstract — method
dispatch, slicing. Finally the backend flattens aggregate types into per-field
signals, monomorphises generic instances, and **emits SystemVerilog**.

The first two stages, parsing and name resolution, are purely syntactic — types
and domains enter only at the HIR. The names *HIR* and *MIR*, and the layering
itself, come straight from rustc; the [Source representation](../source-representation/parsing-and-cst.md)
part covers the syntactic front, and the typed-HIR part covers inference.

## Optimistic checking

Inference is deliberately optimistic about widths. A generic body works with
widths it cannot pin down — a `uint(n)` whose `n` is a parameter — so instead of
demanding that every width be decidable at the definition, inference accepts the
body and records the constraints it cannot yet settle: that two widths are equal,
that a literal fits, that a width is at least one. The `mono_check` pass settles
them afterwards. It walks each call site and, where the call's concrete arguments
make a deferred constraint ground, decides it and reports the failures.

This is a deliberate divergence from rustc, which checks a generic body in full at
its definition. Mirin's widths are value-dependent — a width *is* a const
expression — so the honest check can only run once a call supplies the values.
Checking optimistically and grounding late keeps inference structural: it never
has to decide const arithmetic to produce a type, and a generic that is correct
for the ways it is actually called compiles, even where no symbolic proof was
available.

## Goals

The implementation answers to a few goals, and its architecture follows from
them. It must be **incremental**: a one-character edit should redo work in
proportion to what the edit affects, never re-check the whole crate. It must drive
**two clients from one engine** — the batch `.mrn → .sv` compiler and a language
server — so the two never drift apart. And it must stay **responsive on large
designs**, computing only what an answer actually needs.

## The query engine

Those goals point at one architecture, and it is rust-analyzer's: the compiler is
not a sequence of passes you run, but a set of **queries** you ask. Each query is
a pure function from the database to a value, memoised and recomputed only when
something it read has changed. Asking for a module's emitted Verilog pulls in name
resolution, signatures, and bodies along the way, computes each once, and reuses
everything untouched since the last edit; a language server asks for a far smaller
slice and leaves most bodies alone. [The query engine](query-engine.md) chapter
describes the model — the inputs, demand-driven evaluation, and the incremental
firewall — and the rest of the guide assumes it.

## Where the code lives

The compiler is one crate, `packages/mirin-compiler`, laid out by layer. Each
layer is also a part of this book:

| Layer | Directory | What it holds |
|---|---|---|
| base | `src/base/` | The salsa database, the VFS input boundary, the tree-sitter parser |
| syntax | `src/syntax/` | Stable AstIds, the item tree, syntax errors |
| nameres | `src/nameres/` | The def map: module tree, name tables, def identity |
| hir | `src/hir/` | Signatures, bodies, the type system, inference, checks |
| mir | `src/mir/` | The typed mid-level IR and its const-evaluator |
| backend | `src/backend/` | Monomorphisation and SystemVerilog emission |

The layers depend strictly downward: `syntax` reads `base`, `nameres` reads
`syntax`, and so on. Nothing reads back up.

That is the whole shape: a pipeline of representations, driven by a query engine,
organised as a stack of layers. The next chapter describes the engine; the parts
after it walk the layers in pipeline order, starting at the source text.
