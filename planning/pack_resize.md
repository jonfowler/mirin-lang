# Pack / unpack and resizing

Status: design. Two width-manipulation primitive families that build on the
trait machinery (`planning/traits.md`, all slices landed) and `bits(N)`
(`planning/bits.md`). They are the next operator workstream after the four
operator categories (`planning/operators.md`):

- **Pack / unpack** — the `BitPack` trait. `pack` flattens a value to its raw
  `bits` representation; `unpack` rebuilds it. A type's `bit_size` is an
  associated const. Hand-written for the primitives now; **`derive`-able** for
  structs / vecs / tuples later (the gateway to generic FIFOs, memories and CDC
  primitives over any packable payload — `planning/traits.md` customer 3).
- **Resizing** — `extend` / `truncate` / `extend_lsb` / `truncate_lsb` /
  `resize` / `resize_lsb`: width-changing methods on the width-carrying
  primitives (`uint` / `sint` / `bits`), as ordinary per-type **inherent impls
  in the prelude** (no compiler special-casing — the direction lives in each
  impl's return type).

Companion reading: `planning/bits.md` (why `bits` is the pack target, not a
number), `planning/traits.md` T4 (associated consts — the `bit_size`
mechanism), `planning/numeric_literals.md` (how the count / amount literals
type), `planning/vectors.md` (slicing shares the backend machinery resize
needs).

## The rustc analogy

- **Pack/unpack** has no single rustc feature; the closest shapes are the
  `zerocopy` / `bytemuck` *derived* byte-view traits and `mem::transmute`
  between layout-compatible types. The Mirin twist: the representation type is
  always `bits(N)`, the layout is little-endian and the trait is derivable, so
  a struct's representation is a checked, mechanical concatenation of its
  fields' representations — not an `unsafe` reinterpret.
- **Resizing** is rustc's numeric `as` casts. `u8 as u16` zero-extends,
  `i8 as i16` sign-extends, `u16 as u8` truncates the high bits — exactly
  `extend` (sign-aware) and `truncate`. We split the directions into *named,
  strict* methods (extend must grow, truncate must shrink) instead of one
  direction-blind `as`, because a silent width change in RTL is a bug magnet;
  `resize` is the one bidirectional escape hatch, and the `_lsb` variants are
  the fixed-point / alignment cases `as` cannot express at all.

---

# Pack / unpack — the `BitPack` trait

```mirin
pub trait BitPack {
    const bit_size: integer;                  // depends only on the type
    fn pack(self) -> bits(bit_size);          // value  → representation
    fn unpack(b: bits(bit_size)) -> Self;     // representation → value  (static fn, no receiver)
}
```

This supersedes the T4 sketch in `planning/traits.md` / `trait_assoc_const.mrn`
(which named the const `width` and packed to `uint`). Two corrections, both
already anticipated by `planning/bits.md`:

- **The target is `bits`, not `uint`.** A packed value is a *representation*;
  arithmetic on it is almost always a bug. You convert back through `unpack`
  (or through an explicit number) deliberately.
- **`bit_size` is an associated const, not a method** — despite the
  `bit_size()` phrasing. It has to be a const, because it appears in a **type
  position**: the return type `bits(Self::bit_size)`. Type arguments are
  const args (`ConstArg`); a method-*call* result is not one and could never
  index `bits(...)`. "A method that depends only on the type, producing an
  integer" *is* an associated const in this compiler. Referenced bare inside
  the trait/impl (`bits(bit_size)`), and as `Self::bit_size` / `T::bit_size`
  outside (the path form T4 already supports).

### Round-trip laws

`unpack(pack(x)) == x` for every value, and `pack(unpack(b)) == b` for every
`b: bits(Self::bit_size)`. The primitive impls are bit-for-bit identities, so
both hold trivially; the derive must preserve them (the endianness rule below
is what pins the second one).

### Primitive impls

| Type | `bit_size` | `pack` | `unpack` |
|---|---|---|---|
| `uint(n)` | `n` | reinterpret bits | reinterpret bits |
| `sint(n)` | `n` | reinterpret bits | reinterpret bits |
| `bits(n)` | `n` | identity | identity |
| `bool`    | `1` | the one bit | the one bit |
| `integer` | — (no impl) | — | — |

`integer` is widthless and compile-time only, so it has **no representation** —
no `BitPack` impl. Pack it by first constructing a concrete-width value
(`uint(8)::k`).

The bodies are inline `verilog` — a packed value occupies the same bits as its
source, so every primitive pack/unpack is a wire:

```mirin
impl {param n: integer} BitPack for uint(n) {
    const bit_size: integer = n;
    fn pack(self) -> bits(bit_size)        = verilog { assign ${result} = ${self}; }
    fn unpack(b: bits(bit_size)) -> Self   = verilog { assign ${result} = ${b};    }
}
// sint(n): same wire reinterpretation (the two's-complement bits are the value);
// bits(n): both directions are the identity; bool: bit_size 1, same single bit.
```

No backend special-case is needed for the primitives: these are ordinary trait
impls whose bodies are inline SV, dispatched and emitted like the operator
impls (and carrying `#[inline]`, `planning/attributes.md`, so a pack is a wire,
not a module). `pack` is a plain method call (`x.pack()`) — receiver-dispatched
through the existing method probe.

**`unpack` is deferred (v1 ships `bit_size` + `pack`).** Unlike `pack`, `unpack`
dispatches on its *return* type (`unpack(b: bits(W)) -> Self`), which Mirin's
receiver/owner dispatch and current grammar don't express: there is no
concrete-type path-call (`uint(8)::unpack(b)`) and no return-type-directed
dispatch. `unpack` becomes its own slice once one of those lands; the
round-trip law is stated for when it does.

### Endianness — little-endian, settled

**The zeroth index / first field becomes the rightmost (least-significant)
bits.** This is the one semantic choice in pack, and it governs the derive.

SV concatenation `{a, b, c}` is most-significant-first (`a` is the high bits,
`c` the low bits), so "element 0 at the right" means the concatenation lists
elements **in reverse**. For a struct `S { f0: T0, f1: T1, … fk: Tk }`:

```
pack(s)   =  { fk.pack(), … , f1.pack(), f0.pack() }     // f0 is rightmost / LSBs
bit_size  =  T0::bit_size + T1::bit_size + … + Tk::bit_size
```

and `unpack` slices the same way — `f0` from bits `[T0::bit_size-1 : 0]`, `f1`
from the next `T1::bit_size` bits up, etc., each slice fed to `Ti::unpack`:

```
              MSB ────────────────────────────────────► LSB
   bits(B) =  [  fk  ] … [  f1  ] [        f0        ]
                                   └ Ti::bit_size wide ┘
```

`Vec(N, A)` is the same rule with a uniform element type — index 0 at the
right:

```
pack(v)   =  { v[N-1].pack(), … , v[1].pack(), v[0].pack() }
bit_size  =  N * A::bit_size
unpack    :  element i  ←  b[(i+1)*A::bit_size - 1 : i*A::bit_size]
```

Tuples `(T0, …, Tk)` pack exactly like a struct (element 0 rightmost). The
ordering is consistent across all three aggregates, so nesting composes
(`pack` of a struct-of-vecs is well-defined by recursion) and the round-trip
law holds by construction.

### Deriving `BitPack` (the real payoff — later)

`derive(BitPack)` synthesizes the impl above for any **positive** aggregate whose
leaves are all `BitPack`:

```mirin
#[derive(BitPack)]
struct Pixel { r: uint(8), g: uint(8), b: uint(8) }   // bit_size 24; r is bits [7:0]
```

- The generated impl carries a bound *per generic leaf*:
  `impl {param N: integer, param A: BitPack} BitPack for Vec(N, A)` and likewise for
  generic structs/tuples — checked by the solver like any bounded impl.
- The `bit_size` body is const arithmetic over the leaves' `bit_size`s
  (`T0::bit_size + …`, `N * A::bit_size`). While generic this is an
  unevaluated assoc-const expression; it rides the **ConstEq deferral** T4
  already built and folds at mono time (`planning/traits.md` §associated
  consts). No `generic_const_exprs` machinery.
- **Ports are not packable.** A port has direction; its `in` leaves are not a
  positive value, so a struct/tuple containing a port has no `BitPack` impl
  (`derive(BitPack)` errors, the same restriction the type-zoo `Pos` marker will
  formalize). Only positive aggregates pack.

Deriving is listed as future on the todo-list ("deriving pack instances for
structs"); this doc fixes the contract so the hand-written primitive impls and
the eventual derive agree bit-for-bit.

---

# Resizing

Six width-changing methods on the width-carrying primitives. They change the
**width** of a value, keeping it the same *kind* — `uint(8).extend{by=4}` is a
`uint(12)`, still a number, not `bits`.

| Method | Side | Direction | Strict? | uint | sint | bits |
|---|---|---|:---:|---|---|---|
| `extend`       | MSB | grow  | ✓ | zero-extend | **sign**-extend | zero-extend |
| `truncate`     | MSB | shrink| ✓ | drop high bits | drop high bits | drop high bits |
| `extend_lsb`   | LSB | grow  | ✓ | append low zeros | append low zeros | append low zeros |
| `truncate_lsb` | LSB | shrink| ✓ | drop low bits | drop low bits | drop low bits |
| `resize`       | MSB | either| — | extend **or** truncate | sign-extend or truncate | zero-ext or truncate |
| `resize_lsb`   | LSB | either| — | extend_lsb or truncate_lsb (same, all types) |

- **MSB-side ops change the high end.** `extend` is the value-preserving grow
  (zero-extend a uint, sign-extend a sint, zero-pad a bits — `>>` of operators
  already chose `>>>` for sint, same sign discipline). `truncate` drops the
  high bits — a modulo for numbers, a slice for bits.
- **LSB-side ops change the low end.** `extend_lsb` appends zero LSBs (a left
  shift that *grows* the type — fixed-point scale-up); `truncate_lsb` drops the
  low bits (a right shift that *shrinks* — fixed-point scale-down / alignment).
  Sign is irrelevant at the LSB end, so these are identical for all three types.
- **`integer` has no width → no resize.** Resizing an `integer` is "widths take
  `integer`, found nothing to resize" — a type error. Resize numbers and bits,
  not compile-time scalars.

### The amount: a named, inferable argument

`extend` / `truncate` / `extend_lsb` / `truncate_lsb` take **`by`** — the
number of bits to add or remove. `resize` / `resize_lsb` take **`to`** — the
target total width. The split reads as English: *extend by 4*, *resize to 12*.

Both are **named const arguments** in the brace section. Like every
`param`/`dom` argument they are **inferable by default** — when omitted, the
width comes from the expected type (the LHS ascription / callee param), no
special marker needed:

```mirin
let a: uint(8)  = …;
let w = a.extend{ by = 4 };        // explicit: uint(12)
let x: uint(12) = a.extend{};      // inferred:  by = 12 - 8 = 4
let y: uint(8)  = some_wide.truncate{};   // inferred: by = src - 8
let z: uint(16) = a.resize{};      // to = 16  (would extend; resize would also accept a narrower target)
```

The amount is compile-time (it sets a width), so it is a `param`-kind named
arg, not a runtime value. Surface call form follows the section convention
(`{named} (positional)`, `planning/traits.md`): the receiver fills `self` in
the positional section, the amount goes in the named braces, so a fully-inferred
call is just `a.extend{}` (or `a.extend()` if the empty named section elides —
*open, see below*).

### Strictness

`extend` / `truncate` (+ their `_lsb` variants) are **strict**: the operation
must actually go in its direction.

- `extend{by=k}` requires `k > 0` (equivalently, an inferred target *wider*
  than the source). `a.extend{}` into a same-or-narrower type is an error —
  "extend must grow; target uint(8) is not wider than uint(8)". Use `resize` if
  you mean "maybe grow".
- `truncate{by=k}` requires `0 < k < n` (the result keeps ≥ 1 bit). Truncating
  to the same width, wider, or to nothing is an error.
- The check is on the resolved widths. With an explicit `by`, it is a direct
  compile-time check; with an inferred `by` and a *symbolic* target width, it
  rides the residual `initial assert` pipeline like the literal-fit and width
  residuals (`planning/numeric_literals.md`).

`resize` / `resize_lsb` are **non-strict**: they grow, shrink, or no-op to reach
the target, picking the direction from the resolved widths. They are the
deliberate "I don't care which way" form, and the only one that silently does
nothing when widths match.

### Resize as per-type inherent impls — no compiler special case

Resize is **neither a builtin type rule nor a trait**. It is a set of ordinary
**inherent impls in the prelude**, one per primitive, with inline-`verilog`
bodies — the operator-impl pattern, minus the trait. This is the
fewest-special-cases option: the trait route hits the const-indexed
associated-type wall (below), and a builtin type rule would special-case the
result-width math in the compiler. An inherent impl needs neither — each impl
just writes its own *concrete* return type, so there is nothing to abstract over.

Everything it relies on already exists and is general:

- **Const arithmetic in a type position** — `uint(n + by)`, `uint(n - by)` —
  works today (`examples/working/const_arith.mrn`, and `negative-width.mrn`
  shows `uint(n - 3)` is *width-validity checked*).
- **Const-param interpolation in inline verilog**, including arithmetic
  (`${by}`, `${n - by}` → `(n - by)`) — `planning/inline_verilog.md`.
- **Inherent impls whose owner is a builtin primitive** — the one incremental
  bit. Trait impls on `uint(n)` already work (the operator prelude), so the
  self-type machinery is in place; this just allows the trait-less form.

```mirin
impl {param n: integer} uint(n) {
    // MSB side: SV self-determined LHS sizing does the work; DIRECTION lives in
    // the return type, so the three share one body.
    fn extend   {by: integer} (self) -> uint(n + by) = verilog { assign ${result} = ${self}; }
    fn truncate {by: integer} (self) -> uint(n - by) = verilog { assign ${result} = ${self}; }
    fn resize   {to: integer} (self) -> uint(to)     = verilog { assign ${result} = ${self}; }

    // LSB side: explicit shift.
    fn extend_lsb   {by: integer} (self) -> uint(n + by) = verilog { assign ${result} = {${self}, {${by}{1'b0}}}; }
    fn truncate_lsb {by: integer} (self) -> uint(n - by) = verilog { assign ${result} = ${self}[${n} - 1 : ${by}]; }
}
// sint(n): identical bodies — `logic signed` makes `assign result = self`
//          sign-extend / truncate. bits(n): identical to uint.
```

**Strictness is encoded in the return type, Clash-style** — no runtime check,
no compiler rule:

- `extend -> uint(n + by)` is provably ≥ `n` *iff `by` is non-negative*;
  `truncate -> uint(n - by)` can't underflow because the **existing
  negative-width check** rejects `n - by < 1`. So directional strictness reduces
  to one thing: **`by` must be a non-negative const (a `Nat`)**, not a signed
  `integer` — exactly Clash's `KnownNat b` in `extend :: f a -> f (b + a)` /
  `truncateB :: f (a + b) -> f a`. With a signed `by`, a negative amount would
  let `extend` silently *shrink*; until a `Nat` const kind exists, guard it with
  a const `where by >= 0` predicate (a general residual-assert obligation, not
  resize-specific) or lean on the naming + width-validity backstop.
- The MSB trio collapses to `assign result = self`: SV LHS-width sizing
  zero-extends (uint/bits), sign-extends (sint, via `logic signed`), or
  truncates to fit. Only the *return type* distinguishes extend / truncate /
  resize. `extend_lsb` / `truncate_lsb` need explicit shifts; `resize_lsb` is
  direction-dependent (the shift sign flips with `to - n`), so it is either a
  conditional body or deferred.

> Forward path — a unifying `Resize` trait. The inherent impls duplicate six
> near-identical methods across uint/sint/bits. A trait would dedupe them, but
> the width-varying return needs a return type that is a *function of the
> method's amount* — `Output(by) = uint(n + by)` — i.e. a **const-indexed
> associated type**. Clash gets this from HKT (`Resize (f :: Nat -> Type)`,
> `extend :: f a -> f (b + a)`); Rust from a **GAT** (`type Output<const W>`),
> though `extend`'s `Unsigned<{N + BY}>` needs the unstable
> `generic_const_exprs`. Mirin is *better* placed than stable Rust — the const
> arithmetic that blocks Rust is what `planning/traits.md` already defers to mono
> time (ConstEq residuals), and widths are already const-generic (`ConstArg` =
> Clash's `Nat`). But associated types (let alone const-indexed) are deferred
> from the trait core, and the duplication is cheap, so the inherent impls are
> the right call now; the surface (`x.extend{ by = k }`) is identical if a
> `Resize` trait ever subsumes them.

### Backend

**No new `SvExpr` and no resize-specific emission.** Resize impls are inline
`verilog`, so the concat / slice / replicate they use is plain text inside the
block (`{…}`, `[a:b]`, `{k{1'b0}}`) — the existing splice machinery carries it.
(The `Concat` / `Slice` / `Replicate` `SvExpr` nodes are still wanted, but by
*pack's derive* and the bits-slicing workstream — `planning/vectors.md` — not by
resize.)

The one backend concern is **SV quality**, and it is shared with the operators.
An ordinary impl emits as a per-call *instance* — a tiny module
(`assign result = self`) per `extend`/`truncate`. The resize impls carry
**`#[inline]`** (`planning/attributes.md`), the general emission directive that
splices a combinational body at the call site instead of instantiating a module,
so `a.extend{ by = 4 }` emits as the bare RHS, exactly like `a + b`. `#[inline]`
replaces (and generalizes) today's operator-only `prelude_op` inliner — no
resize-specific backend code, and no new `SvExpr` (the concat/slice live as text
inside the inline-`verilog` body).

### Const evaluation

Nothing new for the primitive slice. `bit_size` is an associated const, so it
folds through T4's assoc-const evaluation (resolve impl → eval body); for the
primitives that body is the width arg `n`. The derive's `bit_size` sums/products
are `+`/`*` on `integer`, already in `const_eval`. `pack`/`unpack` produce
runtime wires (never const). Resize amounts (`by`/`to`) are `integer` consts and
fold with the existing integer arithmetic; resize *values* are runtime SV.

---

## Grammar

Minimal — in contrast to the operator workstream's precedence levels.

- `pack` / `unpack` are ordinary method/path calls; no new syntax.
- Resize calls reuse the existing `{named} (positional)` call form; the amount
  is one named const arg, omittable because `param`/`dom` args are inferable by
  default. No new syntax.
- Highlighting/`mirin-fmt` round-trip method calls generically already.

## Open decisions

1. **`bit_size` — settled: associated const** (not a method), because it
   appears in the return *type* `bits(Self::bit_size)`. Renames T4's `width`;
   `trait_assoc_const.mrn` updates to match (target `bits`, add `unpack`).
2. **Pack target — settled: `bits`** (not `uint`), per `planning/bits.md`.
3. **Endianness — settled: little-endian** — index 0 / first field at the LSB
   (rightmost), so SV concatenation lists leaves in reverse. Governs the derive
   and the round-trip law.
4. **Amount is the target width `to` (settled during impl), inferred from
   context.** The generic is the *target width*, not the delta `by`: `-> uint(to)`
   unifies trivially with the expected type (`uint(to) ~ uint(12)` → `to=12`),
   whereas a delta would need `n + by = 12` solved by algebraic inversion, which
   the const solver does not do. Each body computes its own fill/slice from
   `to - n`. v1 is **inference-only**: `a.extend()` with `to` solved from the
   LHS/callee context. An explicit amount (`a.extend{to=12}()`) needs named
   generic args on *method* calls (a named section currently lowers a method call
   to a `Call{callee: Field}`, which infer rejects) — deferred with that
   machinery.
5. **Resize is per-type inherent impls in the prelude — settled** (not a
   builtin type rule, not a trait). Each impl writes its own concrete return
   type (`uint(n + by)` …), so the const-indexed-associated-type wall never
   appears and the compiler needs no resize-specific rule. A unifying `Resize`
   trait (the GAT/Clash `f :: Nat -> Type` shape, ConstEq-evaluated) is a future
   dedup once const-indexed associated types exist.
6. **Strictness — settled, encoded in the return type; v1 documents one gap.**
   `truncate -> uint(n - by)` can't underflow (existing negative-width check).
   `extend -> uint(n + by)` can't shrink *iff `by` is non-negative* — but v1 has
   `by: integer` and **no `Nat` const kind / const `where` predicate**, so a
   negative inferred `by` would silently shrink an `extend`. **v1 accepts this as
   a documented limitation** (lean on naming + the negative-width backstop); full
   directional strictness arrives with a `Nat` const kind or const predicates.
7. **Open: empty-named-section elision.** Does a fully-inferred call write
   `a.extend{}` or may the empty braces drop to `a.extend()`? Lean toward
   allowing `a.extend()` for readability; pin when the call grammar for
   omitted-but-present named sections is finalized.

## Staging

Each slice is independently committable with examples + fail-examples, on the
T4/T5 foundation:

- **PK1 — `BitPack` for primitives.** Prelude `BitPack` trait (`bit_size`,
  `pack`; **`unpack` deferred** — return-type dispatch, see decisions), impls
  for `uint`/`sint`/`bits`/`bool` (inline-verilog wire identities, `#[inline]`).
  Smallest slice.
- **PK1.5 — inherent impls on builtins.** Allow `impl {param n: integer}
  uint(n) { … }` (trait-less, builtin owner) — the one prerequisite the resize
  impls need that doesn't exist yet. Generic, not resize-specific.
- **PK2 — extend / truncate.** Prelude inherent impls on `uint`/`sint`/`bits`
  with inline-`verilog` bodies (MSB trio = `assign result = self`, direction in
  the return type); `by` inferable; strictness via the return-type width
  arithmetic + negative-width check. Carry `#[inline]`
  (`planning/attributes.md`, stage A2) so they emit inline, not as instances.
- **PK3 — `_lsb` variants.** `extend_lsb` / `truncate_lsb` (explicit shift
  bodies).
- **PK4 — resize.** The non-strict bidirectional MSB form, `#[inline]` via SV's
  width-cast `to'(self)` (one composition-safe expression: zero/sign-extends or
  truncates the high bits, preserving signedness). **resize_lsb deferred** — an
  LSB-aligned bidirectional resize can't be a single width-cast (the cast is
  MSB-aligned) and needs direction-aware lowering.
- **Later.** `derive(BitPack)` for struct / vec / tuple (little-endian
  concatenation, per-leaf `BitPack` bounds, ConstEq `bit_size` folding) — this is
  what introduces `SvExpr::Concat`/`Slice`/`Replicate`; generic packers / FIFOs
  / CDC over `T: BitPack`; a unifying `Resize` trait with a const-indexed `Output`
  once associated types land; bits-slicing (`x[7:4]`) sharing the `Slice` node;
  a `Nat` const kind (or const `where` predicates) for type-level strictness.
