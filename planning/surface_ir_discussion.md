# Surface IR discussion

## Identifier representation in Surface IR

Question: should `Identifier` move away from a plain string representation to something faster to compare, while still keeping the string around for debugging?

Current conclusion:

- Keep `Identifier { text: String, span: ... }` in the syntax-facing Surface IR for now.
- Introduce a separate interned symbol type later, at or just before name elaboration.
- Use that later layer for faster identifier comparison instead of complicating the Surface IR.

Rationale:

- parsing stays simple
- diagnostics can still print original source text directly
- the fast-comparison machinery only appears once names matter semantically

Suggested shape:

```rust
pub struct Identifier {
    pub span: SourceSpan,
    pub text: String,
}

pub struct Symbol(u32);
```

and later:

```rust
pub struct BindingName {
    pub span: SourceSpan,
    pub symbol: Symbol,
}
```

Recommendation:

1. keep `String` in `Identifier` for now
2. add a `Symbol` interner later
3. convert identifiers to symbols during Surface IR -> elaborated IR / name resolution

## Should Surface IR be parameterized over identifier type?

Question: if identifiers eventually become interned symbols, should the Surface IR be parameterized over the identifier representation so it can be reused across stages?

Current conclusion:

- Do **not** parameterize the whole Surface IR over the identifier type.
- Keep textual identifiers in the Surface IR.
- Introduce a separate elaborated IR that uses interned symbols.

Rationale:

If the Surface IR were generic over identifier type, that generic parameter would spread through:

- expressions
- items
- traversal helpers
- tests
- constructors
- later passes

That adds a lot of noise without much benefit.

A cleaner compiler structure is:

1. **CST**: exact syntax
2. **Surface IR**: readable source-level structure with textual identifiers
3. **Elaborated IR**: resolved names, interned symbols, reduced sugar
4. later typed / lowered IRs

Recommendation:

- keep Surface IR non-generic
- add a separate elaborated IR rather than reusing the same tree with a different identifier type
