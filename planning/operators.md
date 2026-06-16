# Operators

Status: design. Builds directly on the operator-trait machinery that landed in
`planning/traits.md` T5 — operators are *not* a special case in inference; each
`a ⊕ b` desugars to a prelude trait method (`a.op(b)`), dispatches through
ordinary trait selection, and the backend emits an inline SV operator for the
prelude impls instead of an instance. This doc widens that foundation from the
five operators present today (`+ - * == <`, plus the bool logicals `! && ||`)
to the full first-pass set across four categories.

Companion reading: `planning/traits.md` (the dispatch path and the
prelude-impl→inline-SV special case), `planning/bits.md` (why `bits` is not a
number), `planning/numeric_literals.md` (how a bare `2` in `x << 2` takes its
type), `planning/const_eval.md` (the const fragment these feed).

## The four categories

| Category | Operators | Prelude trait(s) → method | Result |
|---|---|---|---|
| **Arithmetic** | `+` `-` `*` `/` `%`, unary `-` | `Add`·add, `Sub`·sub, `Mul`·mul, `Div`·div, `Rem`·rem, `Neg`·neg | `Self` |
| **Bitwise** | `&` `\|` `^`, unary `~` | `BitAnd`·bitand, `BitOr`·bitor, `BitXor`·bitxor, `BitNot`·bitnot | `Self` |
| **Shift** | `<<` `>>` | `Shl`·shl, `Shr`·shr | `Self` |
| **Comparison** | `==` `!=` `<` `<=` `>` `>=` | `Eq`·eq/ne, `Ord`·lt/le/gt/ge | `bool` |

The arithmetic, bitwise, and comparison traits keep the existing homogeneous
shape — `fn op(self, other: Self) -> Self` (or `-> bool` for comparison, `fn
op(self) -> Self` for the unary ones), no associated `Output` type. **Shift is
the one exception**: its count is a separate, widthless type, `fn shl(self,
other: integer) -> Self` (see *Shift count* below).

### Per-type support

| Type | Arithmetic | Bitwise | Shift | `==`/`!=` | Ordering `< <= > >=` |
|---|:---:|:---:|:---:|:---:|:---:|
| `uint(n)` | ✓ | ✓ | ✓ (logical) | ✓ | ✓ |
| `sint(n)` | ✓ (`+ - * / %`, unary `-`) | ✓ | ✓ (`>>` sign-extends) | ✓ | ✓ |
| `bits(n)` | — | ✓ | ✓ (logical) | ✓ (existing) | — |
| `bool` | — | — | — | ✓ (existing) | — |
| `integer` | ✓ (const-only) | (future) | (future) | ✓ | ✓ |

- **`uint`/`sint` get everything**, including `/` and `%` (SV `/` `%`; signed for
  sint). sint adds unary `-` (`Neg`, already present); uint has no negation.
- **`bits` is a bag of bits, not a number** (`planning/bits.md`): bitwise + shift
  + the equality it already has, but **no arithmetic and no ordering**. `b1 < b2`
  on a representation is almost always a bug; convert through a number
  deliberately.
- **`bool`** keeps only equality and its logical operators (`! && ||`, the
  `Not`/`And`/`Or` traits) — distinct from bitwise (below).
- **`integer`** is compile-time only; its operators type const arithmetic and
  evaluate in `const_eval`. Arithmetic + comparison land now; bitwise/shift on
  `integer` are deferred until a const customer needs them.

## Semantics that differ from "just emit the operator"

- **Shift count is `integer`.** `<<`/`>>` take a widthless compile-time count
  (`x << 2`, `x >> CFG`), so the count never needs a width annotation and the
  operator does constant shifts only — a runtime/dynamic shift is a later
  explicit form (see *Shift count*). `<<` is logical for every type (arithmetic
  and logical left shift coincide). `>>` is **logical** for `uint`/`bits` (SV
  `>>`) and **arithmetic / sign-extending** for `sint` (SV `>>>`). sint already
  emits as `logic signed`, so `>>>` sign-extends correctly with no extra work.
- **Signed `/` and `%`.** sint operands emit as `logic signed`, so SV `/`
  truncates toward zero and `%` takes the sign of the dividend — matching
  const-eval's `i128` semantics, so a const fold and the synthesized hardware
  agree. uint `/` `%` are unsigned. Divide-by-zero is non-const (const-eval
  yields no value → the usual unresolved-const diagnostic); runtime `/` `%`
  synthesize a (large, comb) divider — the cost is the user's to own, like any
  bare SV `/`.
- **Width is preserved, not widened.** Every binary operator returns `Self`, so
  the result has the operand width. `a * b` and `a << k` truncate to that width
  in the assignment context (standard SV self-determined-then-LHS sizing). A
  widening multiply is an explicit future method, not the `*` operator.
- **Bitwise NOT is `~`, logical NOT is `!`.** They are different operators on
  different traits (`BitNot` vs `Not`) so `~uint` and `!bool` never collide;
  likewise `&`/`|` (`BitAnd`/`BitOr`) are distinct from the short-circuit-shaped
  `&&`/`||` (`And`/`Or` on bool). This mirrors Rust's `BitAnd` vs `&&` split.

### Shift count: `integer` now, `Unsigned` later

The count's width is irrelevant to the result (`Self`), so a width-carrying
count type (`uint(m)`) would leave a bare literal's width unconstrained —
`x << 2` would demand an annotation, and the no-defaulting rule
(`planning/numeric_literals.md`) forbids inventing one. The only count types
that dodge this are widthless ones. v1 fixes the count to **`integer`**:
constants are natural, const shifts fold, and a dynamic (runtime-variable)
shift is simply not expressible by the operator yet.

The forward path is a built-in **`Unsigned` marker trait** (one of the type-zoo
markers, `planning/traits.md` "Later") implemented by `uint(n)` *and* `integer`,
with shift methods generic over the count: `fn shl {param S: Unsigned} (self,
other: S) -> Self`. That admits any uint count *and* keeps annotation-free
constants (`integer: Unsigned`). The migration is purely additive — `x << 2`
type-checks under both, and the marker version only newly admits runtime-uint
counts — so nothing written against the `integer`-count v1 breaks.

## Prelude additions

`src/prelude.mrn` gains the new traits and one impl per (trait, type) cell of
the support matrix. The bodies are inline `verilog` — the checked record of the
semantics — and codegen emits them inline (never as instances), exactly as for
the operators present today. Sketch:

```mirin
pub trait BitAnd { fn bitand(self, other: Self) -> Self; }   // & | ^ ~
pub trait BitOr  { fn bitor (self, other: Self) -> Self; }
pub trait BitXor { fn bitxor(self, other: Self) -> Self; }
pub trait BitNot { fn bitnot(self) -> Self; }

pub trait Shl { fn shl(self, other: integer) -> Self; }      // << >> (count: integer)
pub trait Shr { fn shr(self, other: integer) -> Self; }

pub trait Div { fn div(self, other: Self) -> Self; }         // / %
pub trait Rem { fn rem(self, other: Self) -> Self; }

// Ord/Eq grow the derived comparisons as real methods (no default-method
// support yet), so each comparison emits its DIRECT SV operator:
pub trait Eq  { fn eq(self, other: Self) -> bool; fn ne(self, other: Self) -> bool; }
pub trait Ord { fn lt(self, other: Self) -> bool; fn le(self, other: Self) -> bool;
                fn gt(self, other: Self) -> bool; fn ge(self, other: Self) -> bool; }

impl {param n: integer} Shr for sint(n) {
    fn shr(self, other: integer) -> Self = verilog {
        assign ${result} = ${self} >>> ${other};   // arithmetic: sign-extend
    }
}
impl {param n: integer} Shr for uint(n) {
    fn shr(self, other: integer) -> Self = verilog {
        assign ${result} = ${self} >> ${other};     // logical
    }
}
// … bits Shr is logical `>>`; Shl is `<<` for all three; bitwise are & | ^ ~;
//    Div/Rem are `/` `%` on uint/sint (signed for sint).
```

The impls are mechanical boilerplate; the repetition is the point — each is a
one-line, individually-checked statement of what the operator means for that
type.

## Grammar

Adopt Rust's precedence (the existing levels already match Rust for the
operators present). New levels for shift and the three bitwise binaries slot
**between** comparison and additive; bitwise binds tighter than comparison,
shift tighter than bitwise, looser than `+`. All six comparisons stay
non-associative at one `comparison` level (Rust-style — chaining needs parens),
which is what the grammar already does for `== <`.

| Prec (loose→tight) | Operators |
|---|---|
| assign | `=` |
| logical_or | `\|\|` |
| logical_and | `&&` |
| comparison | `== != < <= > >=` |
| bitwise_or | `\|` |
| bitwise_xor | `^` |
| bitwise_and | `&` |
| shift | `<< >>` |
| additive | `+ -` |
| multiplicative | `* / %` |
| unary | `- ! ~` |
| postfix | `.` `()` `[]` |

`binary_expression` gains the new operator choices at their levels;
`unary_expression` gains `~`. Highlighting tags the new operators like the
existing ones; mirin-fmt already round-trips `binary_expression`/`unary_expression`
generically (operator text is preserved), so only the new tokens need adding.

## Desugar

`body.rs` already maps operator text → trait method (`"+" => "add"`, …). Extend
the binary map with `/ % & | ^ << >>` and the four derived comparisons, and the
unary map with `~`:

```
"/"  => "div",    "%" => "rem",
"&"  => "bitand", "|" => "bitor", "^" => "bitxor",
"<<" => "shl",    ">>" => "shr",
"!=" => "ne",     "<=" => "le",  ">" => "gt", ">=" => "ge",
// unary: "~" => "bitnot"  (alongside "-" => "neg", "!" => "not")
```

No new IR — each is an `ExprKind::MethodCall`, dispatched by the solver like
any method. (Alternative considered: desugar `!=`/`<=`/`>=` to `!(eq)`/`!(lt
swapped)` and keep `Eq`/`Ord` two-method. Rejected: the direct methods give
cleaner generated Verilog — `a >= b`, not `!(a < b)` — at the cost of a few
mechanical prelude lines.)

## Backend

`SvBinOp` gains `Div Rem BitAnd BitOr BitXor Shl Shr Ne Le Gt Ge` (rendering `/
% & | ^ << >> != <= > >=`); the sint `>>` renders `>>>` — so either a distinct
`AShr` variant or, simpler, the sint impl body literally writes `>>>` and
codegen emits the impl's operator verbatim. **`prelude_op` must key on the
resolved method name, not the trait name**, because one trait (`Ord`) now backs
four operators. The method def is already in hand via
`method_resolution(expr)`; switch the match from trait name → method name
(`"lt" => Lt`, `"le" => Le`, `"shl" => Shl`, `"bitand" => BitAnd`, …). The
unary path gains `"bitnot" => "~"`.

## Const evaluation

`const_eval` evaluates the operator desugar by method name on evaluated
operands — it already does `add/sub/mul/eq/lt/and/or/neg/not`. Extend it for
**`integer`** to begin with (the user-facing promise):

- `div`/`rem` via `ConstOp::Div`/`Rem` → `i128::checked_div`/`checked_rem`,
  which already yield `None` on divide-by-zero *and* on the `INT_MIN / -1`
  overflow — propagating as a non-const result (the existing unresolved-const
  diagnostic), exactly the failure modes we want. Their truncate-toward-zero /
  sign-of-dividend semantics match SV signed `/` `%`.
- the new comparison methods (`ne le gt ge`) on `Value::Int` (producing
  `Value::Bool` directly — no `ConstOp` variants needed).
- bitwise/shift on `Value::Int` only when a const customer appears (add
  `Shl/Shr/BitAnd/BitOr/BitXor` to `ConstOp` then).

Shift/bitwise on `uint`/`sint` *values* (not `integer`) need no const support —
they are runtime SV.

## Open decisions

1. **Shift count type — settled: `integer` now.** Constant shifts, no
   annotation, const-eval folds. A runtime/dynamic shift by any uint is the
   `Unsigned`-marker generalization (see *Shift count*), an additive follow-up
   once type-zoo markers land — not a v1 blocker.
2. **Division / modulo — settled: included.** `/ %` as `Div`/`Rem` on
   `uint`/`sint` (signed for sint) + `integer` const-eval. Bare runtime `/` `%`
   synthesize a divider; that cost is the user's, consistent with plain SV.
   Naming follows Rust (`Rem` for `%` = remainder).
3. **Comparison method set — settled: full set.** `eq/ne` + `lt/le/gt/ge` as
   real prelude methods → each comparison emits its direct SV operator (`a >=
   b`, not `!(a < b)`), at the cost of a few mechanical prelude lines. (The lean
   two-method + negation-desugar alternative was rejected for Verilog quality.)
4. **Bitwise trait naming — settled.** `BitAnd/BitOr/BitXor/BitNot` kept
   distinct from the bool `And/Or/Not`, with `~` as the bitwise-NOT operator —
   the `&`/`&&` and `~`/`!` operator split.

## Staging

Each slice is independently committable with examples + fail-examples, on the
T5 foundation:

- **O1 — comparison completion.** `!= <= > >=` + `eq/ne/lt/le/gt/ge` methods on
  `uint/sint/integer` (and `ne` on `bits`/`bool`). Grammar keeps one comparison
  level. const_eval for `integer`. Smallest slice, no new precedence.
- **O2 — div/mod.** `/ %` as `Div`/`Rem` on `uint/sint/integer`; `* / %` at the
  multiplicative level; `ConstOp::Div/Rem` via `checked_div`/`checked_rem`.
  Small (no new precedence level), high const-eval value.
- **O3 — shift.** `<< >>` with `Shl/Shr`, count `integer`; sint `>>`
  arithmetic; new shift precedence level.
- **O4 — bitwise.** `& | ^ ~` with `BitAnd/BitOr/BitXor/BitNot` on
  `uint/sint/bits`; three bitwise precedence levels + unary `~`.
- **Later.** Dynamic shift by any uint (the `Unsigned` marker + generic count);
  const bitwise/shift on `integer` (own `ConstOp` variants); a widening
  multiply method.
