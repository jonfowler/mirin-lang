# The query engine

The [Overview](overview.md) introduced the compiler as a set of queries you ask
rather than passes you run. This chapter makes that precise: the query model, the
inputs queries read, the incremental firewall that makes edits cheap, and the one
place Mirin departs from the model. The phase chapters that follow each open by
naming their query, so this is the vocabulary they assume.

## Queries

A query is a pure function `query(db, key) -> value`, memoised by its key in a
central database. The engine is [salsa](https://github.com/salsa-rs/salsa), the
library behind rust-analyzer; the design follows rustc's query system. Two
properties define it:

- **Demand-driven.** Nothing runs until something asks for it. "Compile the
  crate" forces the emit query for each module, which pulls in only the queries
  on the path to those answers. A language server forces a far smaller slice —
  the type under the cursor — and most bodies are never inferred at all.
- **Dependencies are traced, not declared.** While a query runs, the engine
  records every input and sub-query it reads. It discovers the dependency graph
  by executing it, which is what makes the next edit's invalidation exact.

The database itself is `RootDatabase` (`src/base/db.rs`), a single in-process
handle holding the memo storage. Queries take it by reference; the engine threads
revision tracking through it underneath.

## Inputs

Queries are pure, so every fact that comes from outside the compiler enters as an
**input** — a leaf of the graph, set rather than computed. Mirin has two:

- **`SourceFile`** — one file's path and text. Its text is the only mutable
  *content* in the system; changing it advances salsa's revision and drives all
  downstream invalidation.
- **`SourceRoot`** — the crate's root file plus the set of files reachable for
  module resolution. Name resolution keys on it to map a `mod foo;` declaration
  to another file.

All source text enters through one boundary, the **VFS** (`src/base/vfs.rs`),
which maps each path to its `SourceFile` input. The batch compiler fills the VFS
from disk once; a language server overlays unsaved editor buffers and bumps the
revision on each change. Nothing else reads the filesystem. Keeping the file set
on `SourceRoot` separate from each file's text matters for incrementality: adding
or removing a file changes the set, but editing a file's body mutates only that
`SourceFile`, leaving the set — and name resolution through it — untouched.

## Incrementality and the firewall

The engine is revision-based — rustc's "red-green", salsa's algorithm. A global
counter advances on every input change. Each memoised value remembers when it was
last *verified* current and when its value last *changed*. On demand in a new
revision, a value whose dependencies have all not changed since it was verified is
reused untouched; otherwise it re-executes.

The firewall is one extra rule: **when a query re-executes but produces a value
equal to the old one, its "changed" timestamp does not advance.** Its dependents
compare against that timestamp, see no change, and do not re-run. This is
*backdating*, and it is what stops a keystroke inside one function body from
cascading through the whole crate — provided the early phases produce values that
compare equal across such an edit. The next two chapters are about making exactly
that true: the [item tree](../source-representation/item-tree.md) and the
[def map](../source-representation/name-resolution.md) are built to be unchanged
by a body edit, so resolution backdates and everything downstream survives.

## Why there is no parse query

The model says every derived value is a memoised query. Parsing is the exception.
A tree-sitter `Tree` is owned by the C library and is not structurally
comparable, so it cannot be a tracked salsa value — backdating depends on `==`,
and the tree has none that the engine can use. So parsing is a cheap **transient**
*inside* the queries that need a tree, each of which returns an owned, comparable
summary instead of the tree itself. This is where Mirin diverges from
rust-analyzer, whose green trees *are* storable and so back a real parse query.
The next chapter, [Parsing and the CST](../source-representation/parsing-and-cst.md), picks up there.

From here on, every phase is a query or a small family of them, and each chapter
says what its query returns and what it reads — so the firewall above tells you,
at each step, what an edit disturbs and what it leaves standing.
