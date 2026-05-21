# Bindings, cycles, and scoping

This document records the design decisions around local bindings, cyclic definitions, and structural wiring in Polar.

## The two binding forms

Polar has two distinct ways to introduce a local name:

### `let` — sequential lexical binding

`let x = expr` introduces `x` into scope from that point forward. The name is not visible before the binding. This is the default form for local computation.

The key property is **shadowing**: each `let x = ...` introduces a fresh binding and shadows the previous one. This keeps the number of visible names low and supports readable pipeline-style code:

```rust
let x = stage1(a);
let x = stage2(x);
let x = stage3(x);
```

Each line is unambiguous: the RHS `x` refers to the binding from the previous line. This is not mutation — each `let x` is a distinct signal with a distinct name that happens to reuse the identifier.

### `var` — block-scoped signal declaration

`var x: T` declares a named signal node that is in scope for the **entire enclosing block**, regardless of where the declaration appears. A pre-scan collects all `var` declarations before the forward pass begins, giving them block-wide scope. This means a `var` may appear after statements that already reference the name.

The signal is given its value by a separate assignment equation:

```rust
var count: uint[8] @clk;
count = (count + 1).reg{rstn}(0);
```

**Single-assignment rule:** every `var` must have exactly one assignment in the block. An assignment is either an explicit equation (`x = expr`) or a source connection (`output => x`). Zero assignments is an undriven-signal error. Two assignments is a multiple-driver error. Both checks are enforced at structural checking time.

## Why the two forms are different

`let` and `var` are not interchangeable:

- `let` produces a value. It is sequential and forward-only. The RHS is fully known before the name comes into scope.
- `var` declares a signal node. It participates in an equation system. The equation for a `var` binding can refer to the name itself, or to other `var` names in the same block.

Making ordinary `let` recursive by default would break the simple sequential mental model and make pipeline-style shadowing ambiguous. Keeping `var` distinct preserves both modes without compromise.

## State feedback

Register feedback is the common case for `var`. The counter pattern:

```rust
var count: uint[8] @clk;
count = (count + 1).reg{rstn}(0);
```

The `var` declaration says "there is a signal called `count`." The equation says how it is connected. The cycle through `.reg` is what makes this well-formed — the register separates the current and next values in time.

Type annotation on `var` can often be inferred from the equation:

```rust
var count;
count = (count + 1).reg{rstn}(0);
```

## Structural feedback

Mutual component wiring also uses `var`. When two components are connected bidirectionally, the wires between them must be declared before either component is instantiated:

```rust
var vld, rdy : bool @clk;
var dat : uint[8] @clk;

const_df {
  out vld = vld,
  out dat = dat,
  in  rdy = rdy,
  const_val = 42,
}();

reg_df {
  in  in_vld = vld,
  in  in_dat = dat,
  out in_rdy = rdy,
  ...
}();
```

Both components share the same `vld`, `dat`, and `rdy` signals. The `var` declarations give those signals block-wide scope so they can appear in both instantiation blocks.

## Port connections and `=>`

Source fields (component outputs) use `=>` rather than `=` in connection blocks.
The scoping rule for `=>` is conditional:

- If the name on the RHS is **not in scope**, `=>` introduces it with
  **forward-only (let-like) scope** from this statement forward — visible after
  the component call, not before.
- If the name is **already in scope** (as `var` or `let`), `=>` connects to
  that existing binding. No new declaration is inserted.

`=>` always counts as the single assignment for its target. A second `=> x` in
the same block (whether `x` was implicitly introduced or pre-declared as `var`)
is a multiple-driver error.

```rust
// implicit introduction — out_df visible from here on
reg_df { input = inp_df, output => out_df }();

// pre-declared var — needed when out_df must be visible before this statement
var out_df;
reg_df { input = inp_df, output => out_df }();
```

For structural feedback between multiple components, the shared signals must be
pre-declared with `var` so both instantiation blocks can reach them (see
"Structural feedback" above). An implicitly introduced name from `=>` only has
forward scope and cannot serve this role.

For whole-port bindings where no single `=>` connection point exists, `var` is still declared directly:

```rust
var p: DF{clk};
```

The full connection syntax — including the optional `in`/`out` keywords, pre-declared `var` names for structural feedback, and the source/sink asymmetry — is covered in `planning/port_connections.md`.

## Shadowing rules

### `let` can shadow `var`

A `let` binding may shadow an earlier `var` declaration. This is useful when a `var` defines the feedback core of a computation and further sequential processing follows:

```rust
var count: uint[8] @clk;
count = (count + 1).reg{rstn}(0);

let count = count + offset;   // RHS sees var count; new let count shadows it
let count = count.clip(max);  // RHS sees the previous let count
```

The semantics are consistent with `let`-shadowing-`let`: the RHS of the shadowing binding sees the name as it was before the new binding takes effect.

**Important consequence:** once a `var` is shadowed by `let`, the original signal is no longer accessible by that name. If both the feedback signal and the transformed version are needed downstream, use distinct names:

```rust
var count_r: uint[8] @clk;
count_r = (count_r + 1).reg{rstn}(0);

let count = count_r + offset;  // count_r remains accessible
```

Shadowing a `var` is a statement: "I no longer need the feedback signal by this name." It is intentional, but a reader skimming from the bottom up may not notice the shadowing — this is a known readability cost.

### `var` cannot shadow `let`

Declaring a `var` that shadows an earlier `let` binding in the same block is an error. `var` has wider scope than `let` and allowing it to retroactively shadow earlier sequential bindings would make code hard to reason about.

## Lessons from SystemVerilog

SystemVerilog is useful background but not a model to copy directly.

- The LRM requires declaration before use for ordinary names (Clause 6). Polar follows this: `var` must be declared before it is used, even if the equation referencing it may be written later.
- Redeclaration within the same namespace is illegal in SV. Polar instead allows shadowing across forms (`let` shadows `var`) but not within the same form in the same scope.
- SV separates scope from lifetime. Polar follows this principle: `var` scope is the enclosing block; lifetime is determined by the elaboration and register structure, not the declaration form.
- SV's unnamed generate blocks receive synthesized names. Polar should avoid user-facing semantics that depend on generated names.

## Open questions

- Should there be a lint warning when a `let` shadows a `var`, given the readability risk?
- Should `var` require an explicit type annotation, or is full inference always acceptable?
- Can `var` be used inside `impl` method bodies, or only in component bodies? The safest initial rule is to disallow it and require stateful logic to live in a sub-component, but this needs an explicit decision before `impl` bodies are elaborated.
- How do `var` declarations interact with explicit block scoping if Polar adds named or anonymous block forms later?
- Is `var` legal inside `if`/`match` branches? Hardware signals do not conditionally exist, so the likely rule is that `var` is illegal in conditional branches and must be hoisted to the nearest component body. This must be decided before `if`/`match` is implemented.
- What is the error when a `var` is declared but never assigned an equation? The compiler should catch this at elaboration time (undriven signal), not at RTL lowering. The check should be: every `var` in a block must have exactly one equation whose LHS resolves to that signal node.
- Are two `var` declarations with the same name in the same block an error? Based on the principle that `var` declares a node in an equation system (unlike `let` which shadows), duplicate `var` declarations in the same scope should be a hard error.
- What happens when `=>` is used with a name that already resolves to a `let` binding? A `let` binding is a value, not a signal node, so it cannot be a connection target. This should be an error distinct from the `var`-shadows-`let` rule.
