# Name resolution and the def map

Name resolution turns the per-file item trees into one crate-wide answer to a
single question: what does this name refer to? The answer is the **def map**, the
last representation the compiler builds before it needs types. This chapter covers
the pieces of it in turn — `DefId`, the stable identity of a definition; the
module tree and name tables the `crate_def_map` query builds; the namespaces it
splits names into; the prelude and the dispatch indexes it also collects — and
closes with what it deliberately does *not* do, which is the other half of why it
is cheap to recompute.

This is the name-resolution half of the front end, and like the phases before it,
it reads only structure — never a body, never a type.

## `DefId`: stable definition identity

A definition's identity is a **`DefId`**, minted by interning its syntactic
location: the file it lives in plus its `FileAstId`. Because that AstId is a
hash-of-identity rather than an offset (see [the item tree](item-tree.md)), the
interned `DefId` is stable across edits that don't change the item itself —
including edits to other items and edits inside the item's own body. salsa hands
back the same id for the same location on every revision, so every downstream memo
key built on a `DefId` survives those edits.

One syntactic item can introduce more than one definition. A `struct Bus = bus`
declares both a *type* and a term-level *constructor*. They share one AstId, so a
`DefId` also carries a **role** — `Item` or `Ctor` — to tell them apart.

## The def map

The `crate_def_map(root)` query builds the crate's name resolution in one pass
over the item trees. It produces:

- a **module tree** — the crate root, inline `mod`s, and `mod foo;` file modules,
  resolved against the `SourceRoot`'s file set;
- **name tables** for each module, mapping a name to the definition it binds;
- the **`DefPath`** of every definition — a disambiguated path from the crate
  root, like `crate::util::add`.

Names resolve in two **namespaces**, and Mirin splits them differently from Rust.
Rather than Rust's type/value split, Mirin separates **modules** from everything
else: a `mod` name lives in the `Module` namespace, while types, functions, and
constructors share the `Item` namespace. The split is what lets the common
pattern `mod df { port DF = df { … } }` work — the module `df` and the port `df`
coexist because they sit in different namespaces. A path's non-final segments
resolve in `Module`; a leaf or a bare name resolves in `Item`.

## Imports, the prelude, and dispatch

Three more things fall out of the same pass.

**Imports.** `use` declarations resolve to a fixpoint — an import can name
something itself imported — with visibility enforced as it goes.

**The prelude.** Every crate carries a synthetic `$prelude` module. It holds the
language builtins (`uint`, `bool`, `Vec`, `reg`, …) seeded directly, plus the
operator traits and their builtin impls, which are real Mirin source compiled in
from `src/prelude.mrn`. A bare name that misses the local tables falls through to
the prelude — this is rustc's `core`-injection move. (As of 2026-06, operators
resolve through these prelude traits.)

**Dispatch indexes.** Method and trait resolution should not scan the whole crate,
so the def map also builds indexes: methods per owning type, declarations per
trait, and impls per trait. Inference uses these later to resolve a method call
once it knows the receiver's type.

## What it doesn't do

The def map resolves *names*, and stops there. It does not lower signatures, does
not look at types, and does not resolve method dispatch — that waits for
inference, which needs the receiver's type, not just its syntax. And because the
whole query depends only on item-tree structure, never on a body, a body edit
cannot reach it: the item-tree firewall absorbs the edit, `crate_def_map`
backdates, and goto-definition, privacy, and signature resolution all stay cached.

That makes the def map the last purely syntactic phase. From here the compiler
needs types — which means lowering each definition's signature and body and
running inference over them. That is the HIR, and the next part.
