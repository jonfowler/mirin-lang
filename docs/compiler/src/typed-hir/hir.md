# The HIR

After name resolution, the compiler turns to one definition at a time and lowers
it to the **HIR**: a name-resolved form of the definition's signature and body.
The HIR is split across two queries — `sig_of` and `body` — and it carries no
types. Inference is a third query that derives the types on the side. This chapter
covers that split and the lowering each query does; the two chapters after it are
the type vocabulary inference speaks and inference itself.

## Signature and body are separate queries

`sig_of(def)` lowers a definition's **signature**: its generic parameters, its
value parameters (each with an owner-relative `LocalId`), its return type and
result places, and — for a struct or port — its field types. It is a pure function
of the signature syntax and the crate's name resolution, and it never reads the
body. Editing a body therefore leaves every `sig_of` value-equal, so a caller that
depends only on a callee's signature is not disturbed — the signature/body
firewall. It is the same backdating the [item tree](../source-representation/item-tree.md)
uses, applied one level finer.

`body(def)` lowers the **body**: a name-resolved tree of expressions over
owner-relative ids — an `ExprId` arena and a `LocalId` arena, both reset to zero
per definition, after rust-analyzer's `Body`. It depends on `sig_of(self)`, so the
body's parameter locals line up with the signature, and on the def map to resolve
names. It never reads another definition's body.

## Types stay off the body

The body carries no types, and that is deliberate. Types come from inference, and
keeping them out of the body arena means an edit that changes a type — a callee's
signature, an annotation — does not dirty the body. `infer(def)` is a separate
query that walks the body and produces a side-table from each expression to its
inferred type. The body is the stable, types-off input; the inferred types are
derived and rebuilt freely. This is rust-analyzer's arrangement, and the reason
Mirin keeps a `body` arena and an `infer` side-table rather than one typed tree.

## What lowering does

Beyond resolving names, `body` lowering does three things shaped by the HDL:

- **It splits `var` declarations from their drivers.** A `var x` declares a
  block-scoped signal; the equation that drives it (`x = …`) is a separate
  statement. A body is an equation system, not a sequence of assignments, so the
  declaration and its driving equation are distinct nodes that inference and the
  driver checks treat independently.
- **It defers method dispatch.** A `recv.m(args)` call lowers to a `MethodCall`
  node and stays one: which `m` it resolves to depends on the receiver's inferred
  type, which the body does not have. Inference resolves it later against the
  receiver type and the impl-method index.
- **It records result places.** A `return`, or a named result like
  `-> (sum, carry)`, becomes a place the body can drive by name, carrying the
  SystemVerilog signal its leaves will emit under.

The result is a name-resolved tree that still says nothing about types. Giving it
types is inference's job — and inference speaks a specific vocabulary of types,
consts, and domains. That vocabulary is the next chapter.
