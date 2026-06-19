# `param` / `const` / `type`: flattening generic scope and naming the kinds

> **Status: LANDED (2026-06-19).** All three parts implemented:
> const params are usable as values in the body (`ExprKind::ConstParam`),
> `@const` lowers to `Domain::Const`, and the keyword rename
> `param`→`const` + new `type` (dropping the `: Type` heuristic) is in the
> grammar and across the corpus. The sections below describe the *original*
> mechanism (the "before") as the problem statement; the syntax in their
> examples is pre-rename.

Prerequisite design work for both `when_binding.md` and `compile_mutable.md`. The
mutability work keeps tripping over "gap 2": a `param n: integer` is usable in
*type* position (`Vec(n+1, …)`) but not in *value* position (`range(n)`,
`acc[n-1]`) — `undefined name n`. That is a symptom of how generic parameters are
scoped today. This doc proposes to fix the scoping and, separately, to rename the
declaration keywords.

## Current mechanism (what's actually there)

Generics are **not** in a namespace. A `fn`/`struct`/… carries a positional
`generics: &[GenericParam]` array; each param has a `TermKind`:

- `dom clk: Clock` → `TermKind::Domain`
- a `: <trait>` annotation (e.g. `param T: Bits`) → `TermKind::Type`
- `param` + a non-trait annotation (e.g. `param n: integer`) → `TermKind::Const`

(`sig.rs:1601-1624`, `classify()`. Note the heuristic at `sig.rs:1608`: a
`: Type`-shaped annotation *wins over* the `param` keyword — so whether
`param X: Foo` is a type or a const depends on whether `Foo` resolves as a
trait.)

Resolution is **position-dispatched against that array, kind-filtered**:

- Type position → `generic_index(name, Type)` (`sig.rs:1305`)
- Const position (type args / widths) → `generic_index(name, Const)` then locals
  then assoc consts (`sig.rs:1420`)
- Domain position → `generic_index(name, Domain)` (`sig.rs:1338`)
- **Value position (body exprs)** → body locals, then the `Item` namespace —
  **it never consults the generics array** (`body.rs:1389-1397`).

So there aren't "two namespaces" in the literal sense (the `Namespace` enum is
just `{Module, Item}`, `ids.rs:55-68`, and struct type-ctor `Pix` and
record-ctor `pix` already share `Item` — they're just different *names*). The
split is: **type/const/domain positions read the generics array; value position
ignores it.** The one asymmetry that makes `dom` work but `const` not: domain
params *are* copied into body locals (`body.rs:520-530`), const params are not.
That single omission is the proximate cause of gap 2.

## Proposal 1 — flatten generic scope

Make a generic parameter one binding per name, visible uniformly in every
position, with its declared kind deciding what a reference *means* and erroring
when a position is incompatible (a `type` param used in value position is an
error; a `const` param used in value position resolves to its value).

Concretely this is: register const (and where meaningful, type) generics into the
same scope value-position resolution already consults — mirroring what
`body.rs:520-530` already does for domain params — so `range(n)` and `acc[n-1]`
resolve. This directly closes gap 2.

### Where flattening could bite — checked

- **Tuples (the flagged worry): not a problem.** Tuples introduce no names —
  no type constructor, no value constructor; flattening uses numeric indices
  (`x.0` → `x__0`), `types.rs:63-66`, `planning/tuples.md`. There is nothing for
  a flattened scope to collide with.
- **Struct ctor names: already fine.** `Pix`/`pix` are distinct names in one
  `Item` namespace today; making generics name-keyed doesn't touch them. (The
  deliberate type-ctor/record-ctor name split is exactly what makes a flat scope
  safe here.)
- **The one genuinely new interaction: a generic name vs a value local / item
  of the same name.** Today separate scopes mean `let A = 5` cannot clash with a
  type param `A`. Flattened, it can — and the answer should be the existing
  `let`-shadowing precedence (innermost binding wins; `let` may shadow a
  generic, as it already shadows `var`). This wants one explicit rule, not new
  machinery.

## Proposal 2 — rename the keywords: `param` → `const`, add `type`

```mirin
fn append {type A, const M: integer, const N: integer}
          (Vec(M, A), Vec(N, A)) -> Vec(M+N, A)
```

- `type A` declares a type param; a bound is `type A: Bits`. The `: Type` kind
  heuristic disappears.
- `const N: integer` declares a const param.
- `dom clk: Clock` is unchanged.

### Assessment — I'm broadly for it, with two caveats

**`param` → `const` is a clear win, keep it.**
- It names the concept and matches how you already think of it (`@const`).
- It kills the fragile heuristic at `sig.rs:1608`: today `param T: Bits` is a
  *type* only because `Bits` happens to resolve as a trait; a typo or an
  unresolved bound silently reclassifies the param as a const. Explicit
  `type`/`const` removes that whole failure mode.
- **There is already precedent and no clash:** `const` is *already* a keyword,
  used for associated consts (`trait_const`/`impl_const`, `grammar.js:173,220`).
  A `const` generic param is the same concept in a different position — reusing
  the word is consistent, not overloaded.

**Caveat A — `type` is a new keyword; two things to settle first.**
- `type` is currently *not* a keyword (grammar only uses `"type"` as a field
  name). Adding it is free *today*, but if Mirin later wants `type Foo = Bar`
  aliases or trait associated *types* (the parallel to the existing
  `trait_const`), the same word will be in play. That's probably fine — Rust uses
  `type` for assoc types and aliases and nobody confuses them — but decide the
  word is reserved for "type-level binding" generally before spending it.
- Adding an explicit `type` keyword diverges from Rust, where a bare `{A}` is a
  type param and only `const`/lifetimes get keywords. The bare-ident alternative
  (`fn append {A, const M: integer, const N: integer}`) is terser and more
  familiar. I still **lean to explicit `type`**: Mirin's generic section is a
  keyword-tagged brace list (`dom`/`param` today), so `type`/`const`/`dom` is
  internally uniform and reads top-to-bottom without a "bare ident is special"
  rule — which fits the readability-first priority better than matching Rust. But
  it's the one place worth a deliberate call rather than defaulting.

**Caveat B — `const` param vs the `@const` domain are related but not identical.**
You think of `param` as a way to express `@const`, and that's the right
intuition: `const N: integer` is morally `N: integer @const`. But two facts mean
this needs an explicit decision, not an implicit identification:
- The "bare type means `@const`" reading was *already tried and dropped*
  (`inferred_dom_reg.mrn`). So `const N: integer` must mean "const param," and
  must not silently reintroduce the dropped bare-`@const` rule.
- `@const` does not currently lower to `Domain::Const` — it falls to
  `Unspecified` (known gap, in memory). If `const` params are to *be* `@const`
  values, that lowering gap is on the path and should be closed as part of this
  work, so a `const` param actually carries the const domain into the body
  (which is also what makes it usable and correctly-typed in value position
  under Proposal 1).

## Decisions (2026-06-19) — doing all of this in one slice

- **(a) Explicit `type`.** Generic type params are written `type A` (bound:
  `type A: Bits`). No bare-ident form; the brace section is uniformly
  keyword-tagged (`type`/`const`/`dom`).
- **(b) `type` is reserved for type-level bindings** (type params, and possibly
  future assoc types) — *not* for type aliases. Aliases, if added, get a
  separate word (`alias` or similar).
- **(c) Close the const story.** A `const` param must be a usable value in the
  body, and `@const` is to lower to `Domain::Const` (closing the current
  `Unspecified` gap) so a const value carries the const domain correctly.

## Implementation order

1. **Flatten + const-in-body** (Proposal 1, decision c, part 1): register const
   generics into value scope; a const param resolves as a value of its declared
   type at `@const`. Unblocks the mutability docs. Lowest risk — additive.
2. **`@const` → `Domain::Const`** (decision c, part 2): tighten the lowering,
   preserving the `@const <: @clk` subtyping edge the existing examples rely on
   (`const_then_clocked.mrn`, `reg_const_input.mrn`).
3. **Keyword rename** (Proposal 2, decision a/b): grammar `param` → `const`, add
   `type`; drop the `: Type`-wins heuristic (kind now comes from the keyword);
   regenerate the grammar (pinned tree-sitter 0.24.7 / ABI 14); migrate the whole
   corpus and fail-examples; update mirin-fmt / LSP / vscode. Done last because
   it's broad and mechanical, and best applied once the semantics are settled.

## Open questions (remaining)

- Exact shadowing rule when a `let`/`var` local shares a name with a generic
  param (Proposal 1's one new interaction) — default: existing `let`-shadowing
  precedence, innermost wins.
- Should `const` params be any const-evaluable type (`const F: bool`,
  `const X: uint(8)`), generalising beyond `integer`? The `const` keyword invites
  this; lean yes, but verify const-eval handles non-`integer` kinds.
