# Mutability at compile time

> **Status: LANDED (2026-06-19).** `let mut` and reassignment are implemented;
> a mutable accumulator reassigned across a `for` (or straight-line) lowers to
> **option 2** — a procedural `always_comb` with a mutable variable and a
> procedural `for` (the synthesiser unrolls). example:
> `examples/working/fold_sum.mrn`. Scalar *and* aggregate accumulators work
> (init + carry lower per leaf: a Vec → `'{…}`, a tuple/struct → `acc__0`/…).
> Still deferred: the mid-loop-reference form — a read of `acc` *between* two
> carrying loops, which would need the read (and its uses) pulled into the same
> `always_comb`; today it falls back to the structural generate-for and produces
> a duplicate combinational driver (a gap that should become a diagnostic).

Compile-time mutability for the loop-carried accumulator (`let mut`) case. The
`when`-binding case that used to share this doc now lives in `when_binding.md`;
it is a different idea (a single `var` node with a conditional, partial
equation), not loop-carried mutation.

This doc explores the semantics of a mutable local rebound across a folded loop.

## Folding

The common pattern of folding a vector might look like this:

```mirin
fn sum (v: Vec(N, uint(8))) -> uint(8) {
  let mut acc = 0;
  for x in v {
    acc = acc + x;
  }
  acc
}
```

`acc` takes a sequence of distinct values at distinct program points; a mid-loop
reference must read the intermediate value:

```mirin
fn sum (v: Vec(N, uint(8))) -> uint(8) {
   let mut acc = 0;
   // first half
   for x in v[0..N/2] {
      acc = acc + x;
   }

   // Some reference to acc here, should definitely have intermediate value.

   // second half
   for x in v[N/2..N] {
      acc = acc + x;
   }
   acc
}
```

This is a genuine loop-carried recurrence — there is no single node; `acc` is a
value rebound each iteration.

> Loops are **not** unrolled at elaboration. A `for` lowers to a Verilog
> `for`/generate loop and stays folded — unrolling at elaboration would let
> programs explode. Everything below assumes the loop survives into the
> generated SV.

## The workaround today, and why it doesn't compile (tested 2026-06-19)

The closest thing we could write without `let mut` is an explicit recurrence
array:

```mirin
fn sum (v: Vec(N, uint(8))) -> uint(8) {
   let acc : Vec(N+1, uint(8));
   acc[0] = 0;
   for i in 0..N {
      acc[i+1] = acc[i] + v[i];
   }
   acc[N]
}
```

This does **not** compile, for three independent reasons, in increasing depth:

1. **Syntax.** `let acc : Vec(N+1, uint(8));` (uninitialised `let`) is a hard
   syntax error; uninitialised declarations must be `var`. And `0..N` isn't an
   accepted `for` range — you need `range(n)` or `.enumerate()`.

2. **`param` is type-level only.** Rewriting to `var acc` + `param n: integer`,
   the param works in *type* position (`Vec(n+1, …)`) but `range(n)` and the
   final read `acc[n]` both fail with `undefined name n`. A `param` can size a
   type but cannot be read as a value.

3. **Partial-drive coverage doesn't track offset genvar indices.** Even fully
   concrete (`Vec(5)`, `acc[0]=0`, `acc[i+1]=acc[i]+x` over `enumerate()`), the
   coverage checker reports `[1]..[4]` "never driven." A bare genvar drive
   `acc[i] = …` covering the whole Vec is fine (see `for_loops.mrn`), but the
   affine **drive** offset `i+1` defeats coverage — it can't see that
   `{0} ∪ {i+1 : i∈[0,4)}` tiles `[0,5)`. See "Completeness" below — under the
   loose policy this should be *allowed*, not rejected.

## The recurrence form that *does* compile

Keep the offset on the **read**, not the drive, and handle the first iteration
with a boundary `if` on the genvar:

```mirin
fn sum {dom clk: Clock} (v: Vec(4, uint(8)) @clk) -> uint(8) @clk {
   var acc: Vec(4, uint(8)) @clk;
   for (i, x) in v.enumerate() {
      acc[i] = if i == 0 { x } else { acc[i-1] + x };
   }
   acc[3]
}
```

This compiles and emits a clean structural ripple chain (a generate-for whose
body reads `acc[i-1]`). It sidesteps the coverage bug precisely because the
*drive* index is the bare genvar `i` and only the *read* `acc[i-1]` carries the
offset. (The generic-`n` version still hits gap 2 at the final read `acc[n-1]`.)
This is the manual encoding of the loop-carried dependency as an array indexed by
iteration.

## The rust analogy

A natural first guess is "Rust lowers `mut` locals to fresh SSA defs per
assignment." That is **wrong**, and the loop case is exactly where it breaks:

- **MIR is not SSA.** A `let mut acc` is a single mutable `Local`, assigned by
  multiple statements. A loop is a CFG cycle (header block + body + back-edge);
  the loop-carried `acc` is just that one local re-stored in the body, carried by
  the back-edge. No renaming, no phi, and no special case for loops.
- **SSA appears downstream, in LLVM.** rustc lowers each mutable local to an
  `alloca` with load/store; LLVM's `mem2reg`/SROA then promotes it to SSA and
  *synthesises the phi node at the loop header*, merging the entry value with the
  back-edge value. "Fresh def per assignment" only ever describes straight-line
  code, and even there it's LLVM, not MIR. With a loop, SSA *requires* a phi
  because no single dominating name exists.

This is the relevant layering for Mirin: like rustc keeping the loop folded in
MIR, Mirin must not unroll, and like LLVM the *backend* (Verilog synthesis)
recovers the recurrence.

## Two ways to lower `let mut acc`

Both keep the loop folded; they differ in where the recurrence lives.

1. **Explicit-recurrence array** — `acc_vec[i] = if i==0 {init} else
   f(acc_vec[i-1], v[i])`, result `acc_vec[n-1]`. The *manual SSA* form: the phi
   written by hand as an array indexed by iteration. Lowers to a structural
   generate-for ripple (the form that compiles above). Slots into Mirin's
   existing var/index-drive machinery, but forces the `i==0` boundary and a
   self-referential read into the surface syntax every time.

2. **Rely on Verilog's mutability** — a procedural `for` inside `always_comb`
   with a real mutable `acc` reassigned each iteration. The direct analogue of
   LLVM's pre-`mem2reg` picture: one mutable storage location, sequential
   reassignment, the phi left implicit in procedural execution order; the
   synthesiser unrolls, Mirin never does. Closer to how the source reads and to
   how rust actually handles it.

Option 2 is the better practical target: the mutable slot is the honest
representation, and the backend (Verilog synthesis, like LLVM) recovers the
recurrence. Option 1 is what the language can already express today.

### What option 2 lowers to

Source:

```mirin
fn sum {dom clk: Clock} (const n: integer, v: Vec(n, uint(8)) @clk) -> uint(8) @clk {
  let mut acc = 0;
  for x in v {
    acc = acc + x;
  }
  acc
}
```

Lowered SV — `acc` is a variable local to one `always_comb`, initialised, then
reassigned by a procedural `for`; the final value drives the result. The
synthesiser unrolls the loop into a comb adder tree:

```systemverilog
module sum #(parameter int n) (
    input  logic       clk,
    input  logic [7:0] v [0:n-1],
    output logic [7:0] result
);
    always_comb begin
        logic [7:0] acc;
        acc = 8'd0;
        for (int i = 0; i < n; i++) begin
            acc = acc + v[i];
        end
        result = acc;
    end
endmodule
```

The mid-loop-reference version (two halves, reading `acc` between them) lowers to
the same shape with the read spliced between the two procedural loops — the
intermediate value is simply the value of `acc` at that point in the block:

```systemverilog
    always_comb begin
        logic [7:0] acc;
        acc = 8'd0;
        for (int i = 0;     i < n/2; i++) acc = acc + v[i];
        // ... use acc here: it holds the first-half partial sum ...
        for (int i = n/2;   i < n;   i++) acc = acc + v[i];
        result = acc;
    end
```

The single combinational block is what keeps the guard rail (below) honest:
there is one `acc` storage location, never a register.

**Semantic guard rail (either option):** this is purely combinational. `let mut
acc` lives in one `always_comb`; the mid-loop reference reads the partial value
naturally. Carrying `acc` across *clock* edges remains `var` + `.reg` — a
compile-time `let mut` must never silently become storage.

## Completeness

Coverage/completeness checking for loop-carried binding is **deferred**. The
policy for now: the tool should allow anything it cannot tell is incomplete, and
reject only what it can prove is incomplete. The offset-genvar-drive case in gap
3 above is exactly a case the checker cannot really analyse — under this policy
it should be allowed rather than rejected (its current rejection is the checker
being too strict, not catching a real error).

There is an aspiration to do better completeness checking later, most likely at
**monomorphisation** — once `n` is concrete, the set of driven indices is a
finite set that can be checked against the declared length. That is left for
later; being permissive first is the right default.

## Questions

- Is there a different model that makes `unfold`-style functions easy to write
  without mutability? The explicit-recurrence array (option 1) is the closest
  thing expressible today, modulo the `param`-as-value gap (gap 2).
- Does `let mut` warrant surface syntax, or should a `.fold(init, f)` builtin
  (alongside `.enumerate()`) carry the common case first?
