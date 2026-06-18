# Structs as ports

Structs and ports are the same thing at the type level: a **nominal record** —
a named def with an ordered list of typed fields. They differ only in two
surface details:

- A **port** field carries an `in`/`out` **direction**; a **struct** field is
  **positive** (no direction — it flows with the whole).
- A **struct** is **pattern-matchable** (its fields destructure positively); a
  **port** is not (directional fields don't destructure).

So the compiler represents both with one type, `Type::Port { def, args, domain }`,
and one signature shape (`Signature.fields: Vec<Field>`, where `Field.direction`
is `None` for a struct field and `Some(In|Out)` for a port field). The **origin**
is retained on the def: `DefKind::Struct` vs `DefKind::Port`. Code that needs the
value-vs-interface distinction asks the def's kind rather than matching a
separate type variant.

There is no `ValueKind::Struct`; a struct is never a `Type::Value`.

## What each layer does

| Concern | Struct | Port |
| --- | --- | --- |
| Field direction | `None` (positive) | `Some(In/Out)` |
| Flatten drive (`flatten_leaves`) | `child = drives` (None ≠ `In`) | `child = drives == (dir != In)` |
| Pattern matchable (`body`) | yes | no (`PortNotPatternMatchable`) |
| Completeness owed set (`check::struct_leaf_paths`) | field leaf paths | deferred (direction folding decides) |
| `const`-only config record (`backend::is_const_only_ty`) | yes, if all fields const | never (a port is a hardware boundary) |

The single drive formula `drives == (f.direction != Some(In))` subsumes both: a
positive (`None`) or producer (`Out`) field flows with the parent; an `In` field
flips.

## Domains

A record carries one `domain` on the type. Per-field domains come from the
field types in the *definition* referencing the def's `dom` parameters (or
`@const`); they are substituted in at flatten time. Because a struct can now
declare a named-parameter (`dom`/`param`) section like a port, a struct can hold
fields on **different clocks** or mix `@const` with clocked values — the thing
the old single-lifted-`__Dom` model rejected (see the former
`fail-expected/mixed-struct-clocks`). A struct written with no `dom` section
still lifts to one shared `__Dom` and stamps every field, exactly as before.

### Returning a mixed-domain record

A *bare* return type `-> Pair{c1, c2}` is not writable: a `{` immediately after
the return-type name collides with the fn-body brace, and a type application's
argument content (bare identifiers, `a * b` width arithmetic, nested types) is
too close to body-expression content for the parser to back off — the spurious
named-args fork consumes the body and only fails at the very end. This is
unlike `if`/`for` headers, whose named-arg lists use the distinctive `name =
value` form. Requiring a trailing `()` (so an application ends in `)`) is sound
at the language level but does not survive tree-sitter's greedy resolution.

The **named-result** form sidesteps it entirely — the type sits inside the
result-place parens, away from the body boundary:

```
fn mk {dom c1: Clock, dom c2: Clock} (…) -> (out: Pair{c1, c2}) {
    out = pair { a = x, b = y };
}
```

See `examples/working/struct_mixed_return.mrn`.
