# Binding variables inside `when`

This proposal covers letting a `var` receive its assignment *inside* a `when`
statement, via index/field drives in the body. It is the smaller, self-contained
half of the old `compile_mutable.md`; the loop-carried `let mut` half lives in
`compile_mutable.md`.

> **Status: LANDED (2026-06-19).** The statement-form `when` is implemented —
> grammar (`when_statement` + `if_statement` + `init { … }`), HIR (`Stmt::When`),
> infer, driver/coverage checking, and the backend (the inferred-BRAM idiom).
> example: `examples/working/ram_write.mrn`. The clock/event is a **clock edge**
> (the value-form model), not a bare condition — the original sketch below is
> corrected accordingly.

## The shape

```mirin
fn ram_write {dom clk: Clock}
    (addr: uint(8) @clk, data: uint(32) @clk, we: bool @clk) -> () {
  var ram: Vec(256, uint(32)) @clk;
  init { ram = [0; 256]; }
  when clk.posedge() {
    if we { ram[addr] = data; }
  }
}
```

`ram` is a single signal node — a register. The `init { … }` block is its
power-on contents (an SV `initial`, not reset); the `when clk.posedge()` body
drives individual elements on the clock edge, guarded by `if we`. Unwritten
elements **hold** (a register), so `if` needs no `else`. Reading `ram` anywhere
else just reads that node. This lowers to the textbook inferred-BRAM idiom
(`always_ff @(posedge clk) if (we) ram[addr] <= data;`).

## Rules

1. **A `var` may be bound inside a `when`.** The `when` body (together with its
   `init` preceder) is the binding site for that `var` — the place its equation
   lives. This is the statement/binding form, as opposed to the value form
   (`mem = init [...] when … { … }`) already landed in `planning/when_ram.md`.

2. **Per-leaf single assignment, with disjointness.** This is the existing
   partial-drive rule, not a stricter whole-`var` one. A given *leaf* (field or
   element) may be driven once. Within the `when` body there may be many drives
   (multiple statements, multiple `var`s — see rule 4), and the `init`/`when`
   pair together is one binding for the leaves they cover. **Provably disjoint
   leaves of the same `var` may be bound elsewhere** — e.g. `v.valid` driven in
   a `when` body while `v.data` is driven combinationally is fine, because the
   two cover disjoint leaves. Two drives of the *same* leaf (whether both in the
   `when`, or one in the `when` and one outside) is a multiple-driver error.
   (`init` is part of the same binding, not a competing drive.)

3. **The binding need not be complete.** The body may drive only part of the
   `var` — `ram[addr] = data` touches one element of 256. We start **loose**: the
   checker rejects only what it can *prove* is incomplete, and otherwise allows
   it. A tighter coverage check can come later, but being permissive first is the
   right default here (the same deferral stance taken for `for`-loop binding in
   `compile_mutable.md`).

4. **Multiple `var`s may be bound in one `when` body.** A single `when` can be
   the binding site for several signals at once:

   ```mirin
   init { a = 0; b = 0; } when go {
     a[i] = x;
     b     = y;
   }
   ```

5. **No `mut` keyword.** `var` alone is sufficient — the `when` is simply where
   the equation is written. There is no separate mutable-declaration form for
   this case.

## Why this is safe (and why the plain statement-form was reverted)

An earlier free statement-form `when` was reverted (see `planning/when_ram.md`):
allowing arbitrary in-body assignment reopened the general let-mutation
questions — e.g. `v[3] = 5;` after a `let`, mutation as a side effect of a
clocked block. Jon's call was to keep that can of worms shut.

The rules above reclose it. The body is not general mutation: it is the
**single, conditional, possibly-partial equation of one `var` node**. Restricting
the binding to `var` (rule 5), to exactly one site (rule 2), keeps the
single-assignment model intact — this is the existing partial-index-drive model
(`range_and_index_set.mrn`) conditioned by `when`, not a new mutation construct.
A `let` is a value, not a node, and so can never be a `when` binding target.

## Relationship to the value form

The value form already landed (`planning/when_ram.md`):

```mirin
var mem: Vec(4, uint(8)) @clk;
mem = init [0x10, 0x20, 0x30, 0x40] when clk.posedge() {
    if we { mem.replace(waddr, wdata) } else { mem }
};
```

produces the *whole* new value functionally (`replace` is a copy-with-one-swap).
The binding form in this proposal is its imperative-looking dual: drive
individual indices/fields conditionally instead of producing the whole value.
Both are single-assignment at the `var` level; they differ only in whether the
update is written as one whole-value expression or as per-element drives.

## Open questions

- Should `init`-conflict checking (two `init`s for one place) be shared with the
  value form's, per the "Later" list in `planning/when_ram.md`?
- What is the boundary between "can prove incomplete" (reject) and "cannot tell"
  (allow) for rule 3, once dynamic indices are in play? Starting loose defers
  this, but the eventual rule wants writing down.
