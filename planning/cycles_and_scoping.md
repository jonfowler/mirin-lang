# Cycles, shadowing, and scoping

This note records the current discussion around cyclic definitions, local rebinding, and what Polar should learn from SystemVerilog scoping rules.

## Terminology

The desired pattern

```rust
let x = stage1(a);
let x = stage2(x);
let x = stage3(x);
```

is best described as **shadowing** or **rebinding**, not variable capture.

In this style, each `let x = ...` introduces a fresh local binding and the later one shadows the earlier one. This is useful because it keeps the number of names in scope low while preserving a readable dataflow-style pipeline.

## Why the cycle question became harder

The earlier `rec` discussion collapsed two rather different problems into one syntax question:

1. **State feedback**
   - A register next-state computation depends on the current state.
   - Example: a small Mealy-like machine or counter where `count_next` depends on `count`.

2. **Structural feedback**
   - Two or more components are connected through ports that may point in both directions.
   - The resulting wiring graph may contain cycles even if no single local expression looks recursive.

These do not necessarily want the same surface syntax.

## The design pressure

There are two competing goals:

- **Declarative wiring by default**
  - desirable for hardware structure
  - makes simultaneous connections feel natural
- **Sequential local code with shadowing**
  - desirable for readable elaboration-style code
  - keeps the number of local names small

The difficulty is that both modes feel fundamental rather than niche.

## Candidate directions

### 1. Explicit recursive form everywhere

The earlier proposal was:

```rust
rec count = {
  let next = count + 1;
  return next.reg{rstn, reset_val = 0}();
}
```

This makes feedback explicit, but it does not fit local shadowing very naturally, and it is not obviously the right model for mutually connected components.

### 2. Cyclic meaning by default

Another possible direction would be to make ordinary local definitions part of a declarative equation system and add a separate procedural form for ordered code.

That may fit hardware structure better, but it makes ordinary local code harder to read, and it weakens the simple mental model of `let` as lexical binding.

### 3. Declaration first, equation later

One compromise is to allow a name to be declared before it is defined:

```rust
let count: uint[8] @clk;
count = (count + 1).reg{rstn}(0);
```

This is attractive because it can express feedback without making ordinary `let` recursive by default.

However, it has two real drawbacks:

- `let x = expr;` and `let x: T; x = expr;` would have importantly different semantics
- mutual component wiring becomes more verbose if every cyclic edge requires a prior declaration

The syntax may still be viable, but only if Polar makes the distinction very obvious and keeps declarations cheap.

## Current direction

The most promising current split is:

- ordinary `let` keeps **lexical scope** and **shadowing**
- cyclic structure uses a separate concept closer to a **declared node** or **equation**
- structural interconnect and local rebinding are not forced into the same construct

That means the language can still support:

```rust
let x = stage1(a);
let x = stage2(x);
let x = stage3(x);
```

without also implying that every `let` participates in a recursive system of equations.

The open question is whether the cyclic form should remain something like `rec`, or whether declaration-plus-equation is the better primitive.

## SystemVerilog scoping rules

SystemVerilog is useful here because it cleanly separates local procedural scope from structural hierarchy.

### Declaration before use

The 1800-2017 LRM states:

- "Data shall be declared before they are used, apart from implicit nets" (Clause 6, extracted text around line 5441 of the PDF text)

This is an important point of comparison for Polar: ordinary names are not implicitly hoisted into scope.

### Multiple namespaces

The LRM defines multiple namespaces, including local block-related namespaces (Clause 3.13, extracted text around lines 3425-3429 of the PDF text).

In particular:

- blocks introduce their own namespace
- redeclaration is illegal **within a namespace**
- nested scopes therefore give a clean way to shadow outer bindings

This suggests that lexical rebinding in Polar should be treated as an ordinary scope rule, not as a special cyclic feature.

### Block-local scope and lifetime

Clause 6.21 says:

- compilation-unit and module-level variables have static lifetime
- variables declared inside a static task, function, or block are local in scope and default to static lifetime
- individual variables can be marked `automatic`, in which case they have call or block lifetime

For Polar, the important lesson is not the static/automatic split itself, but that **scope** and **lifetime** are treated as distinct concerns.

### Named and unnamed blocks

Clause 9.3.4 says:

- a named `begin : name` or `fork : name` block creates a new hierarchy scope
- an unnamed block creates a new hierarchy scope only if it directly contains a block item declaration
- items in such unnamed scopes cannot be referenced hierarchically

This is a strong hint that Polar should keep local names local, and not rely on every intermediate binding being externally visible.

### Parallel blocks

The syntax for parallel blocks is:

```text
fork [ : block_identifier ] { block_item_declaration } { statement_or_null } join_keyword [ : block_identifier ]
```

This matters because SystemVerilog allows declarations inside `fork` blocks while still keeping the fork semantics clearly distinct from ordinary sequential `begin ... end` execution.

For Polar, that supports the idea that local scoping and simultaneous structure can coexist, as long as the syntax makes the mode clear.

### Generate blocks

The LRM gives unnamed generate blocks synthesized names such as `genblk<n>`.

That is useful as a warning sign: if unnamed structural scopes exist, tools will still need some internal naming scheme. Polar should avoid making user-facing semantics depend on such generated names.

## Lessons for Polar

The SystemVerilog comparison suggests the following:

1. **Keep lexical local bindings simple**
   - ordinary local names should be declared before use
   - shadowing should be an ordinary scope rule

2. **Do not overload ordinary local binding with recursive meaning**
   - rebinding and cycles are different concerns

3. **Make structural equations explicit**
   - whether via `rec` or declaration-plus-equation, feedback should be visible in the syntax

4. **Treat local scope separately from structural hierarchy**
   - local temporary names do not need to be part of the externally visible structure

5. **Keep room for both modes**
   - declarative structural code
   - local ordered code with shadowing

## Open questions

- Should Polar keep `rec` as the explicit cyclic form?
- Should declared-node syntax replace or complement `rec`?
- Can mutual component wiring be expressed ergonomically without forcing verbose forward declarations?
- Should there be an explicit sequential sub-block for local shadowing-heavy code, or is ordinary lexical `let` already enough?
