# Aggregate domains

How clock domains attach to `Vec`, tuples, and structs. Refines
`domain_checking_redux.md` (the semantic model) with the concrete
representation and a staged fix for a soundness hole found 2026-06-13.

## The bug: a stored aggregate domain launders clock domains

Today `Type::Value { kind, domain }` stores a `domain` on *every* value
type, including aggregates. An aggregate's element domains are a *separate*
fact, reconciled with the stored top-level domain only lazily (a `Stamp`
fills `Unspecified` element slots at a projection/index) and only on the
**read** side. On the **write** side the element domain is still
`Unspecified`, and `unify_domain(concrete, Unspecified)` is lenient — so the
incoming value's domain is never checked. The stored top-level domain then
relabels the value on the way out. That is a clock-domain-crossing laundry:

```mirin
// ACCEPTED today — crosses @a to @b with no synchroniser:
fn launder {dom a: Clock, dom b: Clock} (x: uint(8) @a) -> uint(8) @b {
    let v: Vec(2, uint(8)) @b = [x, x];   // element Unspecified; @b not unified with @a
    v[0]                                   // read stamps @b → result @b
}
// same hole via a tuple:
//   let t: (uint(8), uint(8)) @b = (x, x); t.0
```

The crossing *is* caught wherever the domain sits on the leaf where
unification meets it — a direct `@a`→`@b` return, an element-level
annotation (`Vec(2, uint(8) @b)`), or any struct field. Only the
aggregate-level annotation slips through. Two further symptoms of the same
stored-and-derived redundancy:

- **Drift accepted:** `Vec(2, uint(8) @b) @a` (aggregate `@a`, element `@b`)
  type-checks — two independent stored facts silently disagree.
- **Valid types rejected:** `-> Vec(3, (integer @const, uint(8) @clk))` is
  rejected as "missing `@domain`" because the explicit-mode check only
  inspects the top-level domain, which a derived aggregate has none of.

## The model: `@` is a constraint; aggregates have no domain of their own

From `domain_checking_redux.md`: **`Ty @ D` is a constraint** ("every
clock-domain slot in `Ty` is `D`"), discharged for a head-known type by
unifying every unsolved/unspecified domain slot under it with `D`. Two
consequences fix the operand of `@`:

- **`@` applies to a *type*, never a *value*.** `integer : Type`, so
  `integer @D` is well-formed (`integer @const` is the const-domain integer,
  `integer @clk` a clocked one). But `N : integer` — the length — is a
  *value*; `N @D` (e.g. `Vec(N, T) where N @const`) is a category error. A
  value has no domain slots to constrain. (Surface syntax can't even spell
  `@` on a value today, so this is a rule the type language must keep as it
  grows, not a current accept-bug.)
- **A domain is fundamentally a property of a *leaf type*** (`uint`, `bool`,
  `integer`, …). Aggregates carry none of their own:

- `Vec(N, T)` — **no** domain parameter and **no** `T @ D` constraint: `T`
  may be heterogeneous (e.g. `(integer @const, A)`), so forcing one domain on
  it is unsound. A `Vec` is a homogeneous-in-*type* tuple; its domain
  structure lives entirely in `T`. `N` is the length value — unconstrained.
- `Tuple(T, U)` — no domain parameter; mixed domains fall out free.
- A struct's single domain is only the *lifted* (pure) sugar; an explicit
  struct may carry per-field domains. So structs are aggregates too.

`@D` on an aggregate is therefore sugar: propagate `D` into the elements'
unspecified leaf slots and unify, then forget it — there is no aggregate
domain to remember. `enumerate` needs no domain machinery at all:

```
enumerate (Vec(N, A)) -> Vec(N, (integer @const, A))
```

## Fix, staged

**Stage 1 — close the laundry (soundness). [done]** When an explicit `@D`
annotation is lowered onto an aggregate type, propagate `D` into the element
types' *unspecified* domain slots immediately (`stamp_domain`, exactly as the
pure-function lift does for `__Dom` via `LiftDomains`). The element then
carries `@D` from birth, so a write of `@a` data unifies `@a` with `@D` and
conflicts. This is the doc's "discharge `@` by unification for head-known
types," at lowering rather than lazily at projection. Backend unaffected
(domains are erased before SV). Closes the Vec and tuple laundry; does NOT
yet catch drift (an explicit element domain *conflicting* with `@D` — the
stamp fills only unspecified slots) — that waits for Stage 3.

**Stage 2 — structural annotation check (completeness). [done]**
`type_has_domain` derives "is annotated" structurally: a `Vec` is annotated
iff its element is; a tuple iff every element is. Removes the false rejection
of an element-annotated `Vec`.

**Stage 3 — drop the stored aggregate domain (representation). [done]** `Vec`
and `Tuple` are now top-level `Type` variants with no domain field (leaves
and structs keep theirs). A domain lives only on leaves; an aggregate's is
*derived* (a `Vec`'s is its element's; a tuple's is the per-element set —
none singular). Drift between a stored aggregate domain and its elements is
**unrepresentable**. Touched `super_fold`/`match_header`,
`unify`/`subsume`/`merge`/`freshen`/`with_domain`, field/index/literal/
builtin inference, `lower_type`, completeness, and the backend
`flatten_leaves`/`sv_type`/`subst_type`.

**Conflict check (error on inert/drift). [done]** A leaf still has a domain,
so an aggregate `@D` may only *fill* unspecified element slots. An aggregate
`@D` that meets an element's own explicit clock domain ≠ D (`Vec(2, uint(8)
@b) @a`, `(uint(8) @a, uint(8) @b) @c`) is a `ConflictingDomain` error, not a
silent no-op (`@const` stays compatible). Checked syntactically over
signature types; body-ascription conflicts are a remaining gap.

## Out of scope

Emitting a *compile-time integer* (`integer`, the `enumerate` index) at a
module boundary — e.g. returning `Vec(N, (integer @const, A @D))` from a
function — has no hardware representation (the integer has no width). The
*type* is now accepted (Stage 2); turning such a value into ports is a
separate question, not a domain-checking one.

## Evidence / regression cases

- `examples/fail-expected/cdc-launder-vec.mrn`, `cdc-launder-tuple.mrn` —
  the CDC laundry, now rejected (Stage 1).
- `examples/fail-expected/vec-domain-drift.mrn`, `tuple-domain-inert.mrn` —
  drift / inert annotation, now `ConflictingDomain` (Stage 3 + conflict
  check).
- `examples/working/vec_elem_domain.mrn` — an element-annotated `Vec` with no
  outer domain, accepted and emitted (Stage 2).

The element-level (`Vec(N, uint(8) @b)`), struct-field, and direct `@a`→`@b`
crossings already behave correctly and guard against regressions. (`@` on a
*value* — `N @const` — is a syntax error today, so it needs no fixture.)
