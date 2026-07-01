# Numeric literals

Status: design. Decision points marked **[L1]**–**[L7]**, collected at the
end. This replaces the last literal-typing approximation in the compiler:
the lenient `integer ~ uint` unification arm in `infer` (flagged for
replacement since it landed). Foundation: the trait system
(`planning/traits.md`, all slices landed) and the obligation fixpoint.

## Where we are

A numeric literal types as `uint(?w) @const` — a fresh const-var width —
and the lenient `integer ~ uint` unification arm bridges the cases where
literals meet `integer` contexts (and lets genuine `integer` values flow
into uints, which is the part that's wrong).
It works for the corpus but is wrong in principle:

- it is *direction-blind* — genuine `integer` VALUES (not literals) also
  silently mix with hardware uints;
- `1 + x` dispatches `Add` on `integer` (the receiver), so the result
  degrades to `integer` even when `x: uint(8)`;
- there is no fit check at the literal (255 into `uint(4)` sails through
  to Verilog truncation).

## The rustc analogy

rustc gives an integer literal a dedicated **integer inference variable**
(`{integer}`, a separate flavor from general type vars) that only unifies
with integral types, then *defaults* unconstrained ones to `i32` in a
fallback pass after the main fixpoint stalls. The research notes (traits
doc §rustc) flagged the i32 default as a wart — defaulting interleaving
with trait resolution breeds order-dependence — but the *mechanism*
(literal-flavored var + post-stall fallback) is right.

Mirin's divergence: our default is **`integer`** — the arbitrary-size
compile-time scalar — which is safe in a way `i32` is not. There is no
wrong-width hazard: an unconstrained literal stays a compile-time number,
and compile-time numbers don't synthesize. Width never comes from a
default; it comes from context or explicit construction.

Two clarifications (settled 2026-06):

- **Inference, not subtyping.** A literal occurrence has exactly ONE
  monomorphic type, found by unification — `?L` is an inference variable
  with a restricted domain, not a polymorphic `Num a => a`. The language
  keeps exactly one subtyping edge (`@const` below clocks); literals do
  not add a second, for the same reason the domains doc rejects lattice
  growth: edges degrade unification into ≤-solving.
- **Types police where values live, never how const arithmetic
  computes.** const_eval is arbitrary-precision integer math regardless
  of inferred types. An ascription commits first: `let x: uint(8) = 512;
  let y = x - 288;` errors AT the literal `512` even though 224 would
  fit downstream — evaluation cannot launder a type commitment.

## Design

### Lexical: bases and separators **[L1]**

```mirin
let a: uint(8)  = 255;
let b: uint(8)  = 0xFF;
let c: uint(8)  = 0b1111_1111;
let w: uint(16) = 0xBEEF;
let k           = 1_000_000;      // integer
```

Decimal, `0x` hex, `0b` binary; `_` separators anywhere except the first
character. No octal (RTL has no use that survives code review). No
Verilog `8'hFF` forms — width never rides the literal. Value bound stays
i128 (`NumberTooLarge` exists); >128-bit constants are future work
(bigint), noted in Limitations.

### Typing: literal vars + fit obligations **[L2]**

A literal expression types as a **fresh literal-flavored inference
variable** plus one obligation:

```
5        :  ?L          obligation  LiteralFits { ty: ?L, value: 5, span }
```

`?L` unifies with `uint(n)`, `integer` (and `sint` when it lands) —
unification with anything else is the ordinary type error. The fixpoint
discharges `LiteralFits` when `?L` resolves:

- `integer` → trivially ok;
- `uint(n)`, `n` ground → check `0 ≤ value < 2^n`; failure is
  "`255` does not fit `uint(4)`" *at the literal's span*;
- `uint(n)`, `n` symbolic → the check survives as a **residual**
  (alongside the ConstEq residuals) and emits as the module's
  `initial assert (255 < 2**n);` — elaboration-time, like width
  residuals;
- still unresolved when the fixpoint stalls → **fallback**: bind `?L :=
  integer`, then run the queue once more (rustc's fallback placement —
  after the stall, before error reporting).

The lenient `integer ~ uint` arm is **deleted**. Consequence: a genuine
`integer` value no longer crosses into hardware positions implicitly —
`fn f {param n: integer} (x: uint(8)) { x + n }` becomes a type error
(as it should be: `n` has no width). Widths are unaffected (they live at
the `ConstArg` level, not the value level). The corpus tells us how much
this bites; the escape hatch is explicit construction.

### Dispatch: `1 + x` works **[L3]**

With literals as vars, `1 + x` no longer dispatches on `integer`: the
receiver type is `?L`, dispatch defers until `?L` resolves. Method
dispatch on an unresolved receiver currently bails — it gains one step:
if the receiver is a literal var and some argument has a concrete
numeric type, unify first (`?L := uint(8)`), then dispatch. This is a
narrow, literal-var-only rule (not general bidirectional inference), and
it is exactly the place rustc's int-vars interact with method lookup.

### Explicit construction: `uint(6)::4` **[L4]**

```mirin
let x = uint(6)::4;          // exactly uint(6), value 4
let y = uint(8)::0xFF;
let m = uint(n)::1;          // parametric width — fit check rides the residuals
```

Type-path-to-literal syntax: the type expression, `::`, a number. This is
deliberately the **associated-const namespace** — `uint(6)::4` reads as
"the constant 4 of type uint(6)", and the same path shape later hosts
`uint(6)::max` / `uint(6)::min` as real associated consts on a prelude
trait. A literal is morally an anonymous associated const; the syntax
says so. (Rejected: Verilog `6'd4` — width on the literal token; Rust
suffixes `4u6` — unreadable past one digit of width; standalone
ascription `(4 : uint(6))` — fine but heavier, and `let` ascription
already covers most cases.)

Grammar: `literal_expression := type_expression-restricted "::" number`
(the restricted no-named-args type form, same as impl self types). Lowers
to the literal with its type known immediately — no inference var, the
fit check is direct.

### Negation **[L5]**

`-` in prefix position is the unary operator, desugared to `Neg::neg` —
the T5 pattern (`planning/traits.md`). The lexer has **no negative
literals**: `-5` is `neg(5)`, like Rust. This is unambiguous in a C-call
expression grammar (a `-` at operand-start is unary, after an operand is
binary); Haskell's pain comes from juxtaposition-application and operator
sections, neither of which Mirin has. We even dodge Rust's `-128i8`
carve-out, since literals are arbitrary-size at birth and `neg` is just
const arithmetic.

- `impl Neg for integer` in the prelude now (compile-time, const_eval
  matches by method name like add/sub/mul).
- **No `Neg for uint`** in v1: unary minus on unsigned is a silent
  two's-complement wrap; it waits for the sint design, where `-x` has an
  honest type. (`0 - x` remains available and visibly wrapping.)
- Grammar: prefix `-` at `PREC` above multiplicative (Rust: unary binds
  tighter than any binary arithmetic).

### Widths require `integer` (the wrap guard)

Hardware uint arithmetic wraps at its width (`uint(8)`: 200 + 100 = 44);
const_eval's integer math does not (300). The one channel where the two
could be OBSERVED disagreeing is a hardware-typed `@const` value flowing
into a width position (`uint(x)` with `x: uint(8) @const` — const_eval
would compute the unwrapped value). So width/const positions **require
`integer`-typed values**: literals are fine (an unconstrained literal
falls back to `integer`), a hardware-typed value is "widths take
`integer`, found `uint(8)`" — explicit conversion is the future escape
hatch. With this rule const_eval never folds uint-typed arithmetic at
all, and the unbounded/wrapping divergence is unobservable. (rustc's
CTFE wraps because u8 arithmetic IS u8-typed; widths are our one
typed-value→pure-arithmetic channel, so we guard the channel.)

### const_eval and the backend

- const_eval: literals already evaluate; `neg` joins the method-name
  arithmetic. Fit checks never reach const_eval (they're obligations).
- Backend: a literal that resolved to `uint(n)` emits in its **source
  base** (`0xFF` → `8'hFF`, `0b101` → `3'b101`, decimal stays decimal) —
  the base rides as a field on the Number expr ([L6], settled).
  `uint(6)::4` emits `4`. Verilator's width lint remains the backstop.

### Future, explicitly out of scope

- `FromInteger`-style trait so USER types take literal syntax (the
  literal var's unification falls back to an impl search). Wait for a
  customer (fixed-point).
- sint: literal vars unify with it the day it exists; `Neg for sint`.
- bigint literals (>128-bit constants).
- `uint(_)::255` width-from-value inference: rejected — fence-post
  ambiguity (8 or 9 bits?) and it hides the width the reader most needs.

## Staging

ALL LANDED (2026-06-12): N1 (48e304e), N2 (8016c47), N3 (follows).
The lenient `integer ~ uint` arm is gone; the corpus survived with one
adjustment (the wrap guard exempts struct-valued config locals, whose
integer FIELDS project into widths legitimately).

## Decision points

- **[L1]** Bases: decimal + `0x` + `0b` + `_` separators, no octal.
- **[L2]** Literal-flavored inference var, fit obligations, fallback to
  `integer`; the lenient arm dies. Strictness fallout is intended.
- **[L3]** Literal-receiver dispatch unifies against a concrete numeric
  argument first (`1 + x`), as a literal-var-only rule.
- **[L4]** `uint(6)::4` construction (the assoc-const namespace).
- **[L5]** `-` is `Neg::neg`; `Neg for integer` only (no uint until sint).
- **[L6]** SETTLED: emitted SV preserves the source base (`8'hFF`); the
  base is a field on the Number expr.
- **[L7]** SETTLED (from the L2 discussion): width/const positions
  require `integer`-typed values — the wrap guard above.
