# Attributes and the `#[inline]` emission directive

Status: design. Motivated by replacing the backend's operator special case
(`prelude_op`) with one general, user-facing mechanism that operators, resizing
(`planning/pack_resize.md`) and user combinational helpers all share.

## Why

Today codegen hard-codes "a prelude operator-trait impl emits as an inline SV
operator, never an instance" as a method-name → `SvBinOp` table
(`backend/lower.rs::prelude_op`: `"add" => Add`, …) that emits `(a OP b)`. Two
problems:

- It only expresses single binary/unary operators. Resize's bodies are
  concatenation / slicing (`{self, {by{1'b0}}}`, `self[n-1:by]`), not an
  `SvBinOp`, so the table can't grow to cover them.
- It is redundant: the impl body *already* says `assign result = self + other`;
  the `SvBinOp` enum re-encodes the same operator a second time.

Rather than widen the special case, make inlining a **per-def directive**,
`#[inline]`, that **splices the impl body at the call site** instead of emitting
a child module + instance. One mechanism then covers operators, resize, and
user helpers, and the `prelude_op`/`SvBinOp` table retires.

## The attribute mechanism

Mirin has no attribute grammar yet — lang items are discovered by prelude name
(`planning/traits.md`). Several pending features want one: `#[inline]` (this
doc), `#[derive(BitPack)]` (the pack/unpack derive, `planning/pack_resize.md`),
and the todo-list "Optional" items (verilog pragmas, explicitly-named modules).
Introduce a **minimal, Rust-shaped outer-attribute facility** now, with
`#[inline]` as its first member.

- **Grammar.** `#[ path ( args? ) ]` outer attributes preceding an item. `#[` is
  one new token; an attribute list attaches to the following item in the item
  tree. v1 scope: attributes on `fn` and `impl` / impl-method items (all
  `#[inline]` needs); struct/field attributes arrive with `#[derive]`.
- **Alternative considered — a bare keyword** `inline fn …`, no general parser.
  Lighter if inlining were the *only* customer, but `derive` / pragmas / module
  names all want attributes too, and a keyword apiece doesn't scale. So the
  attribute facility is the primary; `inline fn` is noted only as the smaller
  change if the others slip.

## `#[inline]` semantics

The function contributes **no module hierarchy**: at each call site its body is
spliced (the result expression substituted for the call) rather than emitted as
a module instance. This is FIRRTL's per-module `inline` annotation / Yosys's
non-recursive inline — **not Rust's optimization hint**. It has an observable
effect on the generated hierarchy, and deterministic hierarchy is a Mirin goal,
so it is a **directive**: honored, or a compile error if the body can't be
inlined (never a silent fallback to an instance).

**Inlinability contract** (checked once per def; violation =
`"#[inline] requires a single combinational result"`):

- **Combinational only** — no registers / clocked process / `var` feedback; the
  body must reduce to a continuous assignment.
- **Single result value, no `out` parameters** — extra ports and reverse flows
  can't be one expression. Aggregate results inline per leaf (later); v1 is the
  scalar result.
- **`= verilog` body must be the single-assign shape** `assign ${result} =
  EXPR;`, so `EXPR` can be lifted. A Mirin body must be a pure tail expression
  (no statements that introduce nodes).

**Splice.** Arguments substitute as expressions — `${self}` → the receiver's
lowered expr, value params → argument exprs, const params → their instantiated
values — exactly as `prelude_op` substitutes operands today; SV
self-determined-then-LHS sizing applies unchanged.

```mirin
#[inline]
fn mask_low {param n: integer} (x: uint(32)) -> uint(32) = verilog {
    assign ${result} = ${x} & ((32'b1 << ${n}) - 1);
}
// each call emits the masked expression in place — no `mask_low` in the hierarchy.

#[inline]
impl {param n: integer} uint(n) {                 // impl-level: applies to all methods
    fn extend {by: integer} (self) -> uint(n + by) = verilog { assign ${result} = ${self}; }
    // … the rest of the resize family (planning/pack_resize.md)
}
```

`#[inline]` and `= verilog { … }` are **orthogonal axes** — emission strategy vs
body language. An inlined fn may have a Mirin or a verilog body; a verilog fn may
be inlined or instantiated.

### Subsumes the operator special case

Mark the prelude operator impls `#[inline]` too. The backend's emission rule
becomes "resolved def carries `#[inline]` → splice its body", and the
`prelude_op` name→`SvBinOp` table is deleted — the body
`assign result = self + other` carries the operator. Operators, resize, and user
helpers travel one path. (`SvBinOp` may still exist as an internal rendering
detail, but the *inline-vs-instance decision* is no longer keyed on prelude
names.)

## Pipeline placement

- **item tree** — parse attributes into an `attrs` field on fn / impl-method
  items; a small `is_inline(def)` query (or a flag on def data).
- **a check** — validate the inlinability contract per `#[inline]` def at
  sig/check time, with a span on the offending body. Once, not per call.
- **backend `lower`** — where it now tests `prelude_op(expr).is_some()`, test
  `is_inline(resolved_def)`; if set, render the def's body template with the
  call's substitutions spliced as an expression instead of building an
  `SvInstance`.
- **mirin-fmt / highlighting** — attributes round-trip verbatim; one new node.

## Open decisions

1. **Naming — recommend `#[inline]`** (FIRRTL-precedented, familiar), directive
   semantics. Alternatives: `#[flatten]` (Yosys term, but usually connotes
   *recursive* flattening, which this is not) and the `inline fn` keyword
   (rejected as primary, see above).
2. **Granularity — both.** `#[inline]` on an `impl` block is sugar for marking
   each of its methods; also allowed per method.
3. **Directive, not hint — settled.** Inline-or-error, for deterministic
   hierarchy.
4. **Retire `prelude_op`/`SvBinOp` inline keying — yes, as a follow-up** once
   operators carry `#[inline]` (kept until resize+inline land, to avoid a flag
   day).

## Staging

- **A1 — attribute grammar.** `#[…]` on fn / impl(-method) items, item-tree
  `attrs`, fmt + highlighting; `#[inline]` parsed and recognized (no emission
  change yet).
- **A2 — `#[inline]` emission.** Inlinability check + body-splice in the
  backend; convert the resize impls (`planning/pack_resize.md` PK2) from "extend
  the inliner" to plain `#[inline]` prelude impls.
- **A3 — migrate operators.** Operator prelude impls become `#[inline]`; delete
  the `prelude_op` name table (the inline decision is attribute-driven).
- **Later.** `#[derive(BitPack)]`; verilog pragmas and `#[name = "…"]` explicit
  module names (todo-list "Optional"); aggregate per-leaf inline results.
