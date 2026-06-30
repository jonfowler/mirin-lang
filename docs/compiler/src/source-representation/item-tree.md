# The item tree

Editing a function's body should not re-run name resolution for the whole crate.
The **item tree** is what makes that true. It is a lean, per-file summary of the
items a file declares — their names, kinds, and nesting, but none of their bodies
— paired with a **stable identity** for each item that survives edits elsewhere in
the file. This chapter covers both: the identity scheme first, then the summary
built on it, and finally how the two together form the firewall that protects
every later phase from body edits.

Both are lifted straight off the CST, and both follow rust-analyzer's design,
specialised to Mirin's small item set.

## Stable identity: the AstId map

The `ast_id_map(file)` query assigns every item a `FileAstId` — an identity
derived from *what the item is*, not *where it sits*. The id packs three things
into a small integer: the item's kind, a hash of its `(parent, name)` identity,
and an index that disambiguates identical siblings. No part of it depends on a
byte offset or a sibling position, so inserting an unrelated item, reformatting
the file, or editing inside one item's body leaves every *other* item's id
unchanged — which is exactly what lets a memo key built on that id survive the
edit.

## The summary

The `item_tree(file)` query lowers the CST to an `ItemTree`: the file's items in
source order, with modules recursing into their children. Each entry carries only
what *name resolution* needs:

- **functions** — name, visibility, AstId, and whether they are `#[inline]`;
- **structs and ports** — the type name and its mandatory constructor name
  (`struct Bus = bus`), visibility, AstId;
- **traits and impls** — names and the index of their methods and associated
  consts, but no method bodies;
- **modules** — name, visibility, and whether the module is inline or a `mod foo;`
  file reference;
- **`use` declarations** — the import tree as pure syntax, not yet resolved.

What the item tree deliberately leaves out matters as much as what it keeps: no
field types, no parameter signatures, no expressions, no bodies. Those live below
this layer, and signatures are lowered later, on demand, in the HIR. The item
tree holds the skeleton and nothing edit-volatile.

## The firewall

Put the two together and the payoff follows. Because the item tree is a pure
function of the parse that drops everything volatile, editing a function body — or
even its signature — produces a *structurally equal* `ItemTree`. As [the query
engine](../architecture/query-engine.md) explained, salsa then backdates the value:
it re-runs the query, sees the result compares equal, and does not advance the
"changed" timestamp. Name resolution depends on the item tree, sees no change, and
is reused untouched. So does everything downstream of it.

This is the most valuable incremental boundary in the front end. A keystroke in
one body reaches the item tree, stops there, and the crate's resolved names stand.
And those resolved names are the next chapter: with a stable id and an item-only
skeleton for every file, the compiler can build the crate's module tree — the
[def map](name-resolution.md) — without ever opening a body.
