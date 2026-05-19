# Polar high-level design

This document captures the current first-pass design for Polar as a language and compiler project.

It is intentionally high level. It should explain the direction of the language, the expected compiler stages, the initial core-language boundary, and the main design questions that remain open.

Detailed companion documents:

- `planning/syntax.md`
- `planning/compiler_architecture.md`
- `planning/impl.md`

## Project goals

Polar is a hardware description language aimed at register-transfer-level design.

The current goals are:

- make RTL structure explicit and correct
- keep source code readable
- type check enough of the design early to catch structural mistakes
- generate correct, readable, high-quality Verilog
- integrate testing as part of the language story

Non-goal for the first compiler:

- aggressive optimization inside the Polar compiler

Downstream tools can handle low-level optimization. The Polar compiler should focus on semantic correctness, elaboration, and good output structure.

## Current language shape

The current surface-language direction has six main pieces.

### 1. Components

Top-level hardware-building declarations use `cmp`.

Components can have:

- named interface arguments in braces
- positional arguments in parentheses
- an optional return type
- a block body

Named arguments split into two kinds:

- inferable arguments marked with `#`, currently intended mainly for clocks
- defaulted arguments that may be omitted because they have fallback values

Example:

```rust
cmp multAdd
  { #clk: Clock, rstn: Reset @clk = high, c: uint[8] @clk = 0, }
  ( a: uint[8] @clk, b: uint[8] @clk )
  -> uint[8] @clk
  {
    let mult = a * b;
    let mult = mult[8:0];
    let mult = mult.reg{rstn}();
    let add = mult + c;
    return add;
  }
```

### 2. Clocked types

For the current first pass, only clock domains are part of the typed domain system.

Use:

- `T @clk` for clocked values
- `Reset @clk` for resets associated with a clock

The design assumes:

- clock compatibility is checked explicitly
- there is no implicit clock crossing
- literals can remain simple surface syntax instead of needing an explicit temporal-const marker

### 3. Structs

Structs are positive data types with Rust-like field syntax.

They should remain distinct from ports:

- structs do not carry per-field input/output direction
- structs are normal aggregate values

### 4. Ports

Ports are first-class interface types with directional fields.

Important conventions:

- data should stay in positive positions where sensible
- a write input is an explicit exception
- when a port is passed in argument position but will be driven by the callee, that should be written explicitly, for example `out downstream: Stream8{clk}`

### 5. Explicit cycles

Stateful or cyclic definitions should be marked with `rec`.

This keeps feedback structure explicit instead of relying on implicit recursive meaning in ordinary local bindings.

### 6. `impl`

Polar should support Rust-like `impl` blocks as surface syntax for associated functions and methods on nominal types.

Current direction:

- use `fn` inside `impl`
- keep `self` explicit and typed
- allow `impl` on both structs and ports
- lower methods to ordinary namespaced declarations during elaboration

## Syntax subset for early tooling

The initial parser and editor tooling should target a small, consistent subset:

- `cmp`
- `struct`
- `port`
- named and positional argument sections
- `@clk`
- `Reset @clk`
- blocks with `let`, `return`, and `rec`
- method-style calls such as `value.reg{...}()`

`impl` is designed, but does not need to be part of the first parser slice.

## Compiler architecture

The recommended compiler structure is a staged pipeline:

```text
source text
  -> concrete syntax tree
  -> AST
  -> name resolution
  -> surface elaboration
  -> type and clock checking
  -> typed core IR
  -> RTL IR
  -> Verilog emitter
```

### Concrete syntax and parser strategy

Tree-sitter is the recommended first parser technology for the concrete syntax layer.

Recommended use:

- drive syntax highlighting and editor integration
- provide an incremental concrete syntax tree
- feed a Rust AST-lowering layer

Tree-sitter should own syntax recognition, while Rust code should own semantic interpretation.

### AST

The AST should remain close to source syntax while removing trivia and normalizing obvious syntax details.

It should preserve:

- names
- directions
- source-level type forms
- spans for diagnostics

### Name resolution

This stage establishes scopes and resolves:

- declarations
- locals
- fields
- builtins

### Surface elaboration

This stage should desugar the user-facing language into a smaller semantic model.

Key responsibilities:

- apply defaulted named arguments
- solve inferable arguments like `#clk` when uniquely determined
- lower method syntax
- normalize record construction and field access
- preserve explicit `rec` structure

### Type and clock checking

This is where the main RTL correctness checks live:

- type compatibility
- width compatibility
- clock compatibility
- legality of directional port use
- legality of cyclic definitions

## Minimal core language

The core language should be smaller and less convenient than the surface language.

It should include:

- fixed-width integers and primitive scalars
- clocked values
- structs
- ports with explicit directions
- literals
- local bindings
- field access
- indexing and slicing
- primitive operators
- explicit register operations
- explicit component instantiation
- explicit cyclic bindings

It should not need to preserve:

- inferable `#` arguments
- defaults
- method syntax sugar
- other surface conveniences

The purpose of the core language is to make semantic checking straightforward.

## Candidate IRs and stage boundaries

The architecture assumes two main semantic IRs after the AST.

### Typed core IR

This IR represents the fully elaborated meaning of the program.

Properties:

- every expression is typed
- clock information is explicit
- defaults and inference are already resolved
- methods are already lowered
- `rec` has an explicit form

### RTL IR

This IR is shaped for Verilog emission.

Properties:

- explicit module boundaries
- explicit ports
- explicit combinational logic
- explicit registers and next-state relationships
- deterministic naming model

The RTL IR should resemble what must be emitted, not what was written in source.

## Backend direction

The Verilog backend should optimize for:

- correctness
- readability
- stable naming
- faithful clock and reset behavior

The backend should preserve user-chosen names where reasonable and otherwise apply deterministic naming rules.

## Initial implementation path

The recommended first end-to-end slice is:

1. parse component declarations and simple blocks
2. parse `uint[N] @clk` and `Reset @clk`
3. build a small AST
4. perform basic name and clock checks
5. lower to a typed core
6. emit a minimal Verilog subset

This validates the language direction without requiring the whole language up front.

## Open questions

The main open design questions still visible from the current work are:

- exact inference rules for `#clk`
- how generics and const generics should interact with named parameter inference
- whether generalized metadata should ever extend beyond clock domains
- the eventual surface syntax for component instantiation
- the exact representation of ports across elaboration and core typing
- how integrated testing should appear in source syntax
