# Compiler architecture: first pass

This document proposes a starting architecture for the Polar compiler.

The goal is not to build a fully general optimizer. The goal is to parse Polar, elaborate the surface language into a smaller typed core, and emit correct, readable, high-quality Verilog.

## Main principles

- Implement the compiler in Rust
- Keep the front end explicit about source structure and diagnostics
- Lower early from surface syntax into a smaller semantic core
- Preserve naming intent so the backend can emit readable Verilog
- Keep optimization out of scope; rely on downstream synthesis tools for low-level optimization

## Proposed pipeline

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

## Stage responsibilities

## 1. Concrete syntax

Input:

- `.plr` source text

Output:

- lossless or near-lossless concrete syntax tree

Responsibilities:

- recognize declarations, argument sections, types, blocks, and punctuation
- preserve spans for good diagnostics
- tolerate incomplete code well enough for tooling

### Tree-sitter

Tree-sitter is a good fit for the concrete syntax layer and for editor tooling.

Recommended role:

- use tree-sitter for syntax highlighting, editor integration, and incremental parsing
- use the same grammar to produce the compiler CST consumed by the Rust compiler frontend
- keep a separate Rust AST-lowering layer so semantic decisions do not leak into the grammar

Why this is a good first fit:

- incremental parsing helps editor tooling immediately
- the grammar can serve both highlighting and parser prototyping
- CST-to-AST lowering gives a clean place to normalize syntax and report targeted errors

Risk:

- tree-sitter is a syntax tool, not a full semantic front end
- grammar ergonomics can become awkward if too much elaboration logic is pushed into it

So the recommended boundary is:

- tree-sitter owns concrete syntax
- Rust code owns AST construction, name resolution, elaboration, and typing

### Current implementation direction

The repository currently contains:

- `packages/tree-sitter-polar/` as the starting point for the concrete syntax grammar
- `packages/polar-compiler/` as the Rust compiler crate

The handwritten Rust parser in the compiler crate should now be treated as a temporary AST/front-end experiment rather than the long-term concrete-syntax strategy.

## 2. AST

The AST should still look like Polar source, but remove trivia and normalize obvious syntactic variants.

Examples:

- named argument sections become typed AST nodes
- `value.reg{rstn}()` becomes a method-call AST node
- cyclic/feedback bindings (`var`) are represented explicitly rather than inferred later

The AST should preserve:

- declaration names
- field directions
- source-level type forms
- source spans

## 3. Name resolution

Resolve:

- local bindings
- component names
- struct names
- port names
- field names
- builtin functions and operators

This stage should also establish scopes for:

- declaration parameters
- block-local `let` and `var` bindings (including implicit `var` from `=>`)
- record fields
- port field access

## 4. Structural checking

A dedicated pass between name resolution and type inference. Port field directions
are explicit in port declarations and are never polymorphic or inferred, so this
check can run without waiting for type inference to complete.

Checks include:

- connection-block direction agreement: `=` must be used with `in` fields, `=>` with `out` fields
- `var` single-assignment rule: every `var` must have exactly one assignment; both explicit equations (`x = expr`) and source connections (`output => x`) count; zero assignments = undriven error, two = multiple-driver error
- duplicate `var` declarations in the same scope (hard error)
- `var`-after-`let` shadowing (hard error)
- `=>` used with a `let` binding (hard error — tailored message, not exposed as a `var`/`let` conflict)
- multiple drivers: two assignments (equations or `=>`) to the same `var`

## 5. Surface elaboration

This is where Polar surface syntax is simplified into a smaller semantic form.

Responsibilities:

- apply defaulted named arguments
- solve inferable arguments such as `dom clk` when there is a unique solution
  (try to anchor from explicit `Reset @clk` arguments before falling back to
  a fixpoint pass over cyclic `var` equations)
- desugar method syntax into ordinary intrinsic or function calls
- normalize record literals and field access
- make cyclic binding boundaries (`var`) explicit in the lowered representation

This stage should reject ambiguous inference instead of inventing fallback behavior.

## 6. Type and clock checking

This stage enforces the language semantics that matter for correct RTL construction.

Checks include:

- type compatibility
- width compatibility
- clock-domain compatibility
- validity of cyclic `var` definitions
- clock domain consistency across `var`-declared shared wires (checked at every
  connection site, not just the first)

The current first-pass design assumes:

- only clock domains are in scope as typed domain information
- resets are clock-associated values
- no implicit clock crossing

## Minimal core language

The compiler should lower the surface language into a core language that is much smaller than the user-facing syntax.

Recommended core features:

- primitive scalar types
- fixed-width integers
- clocked types
- structs
- ports with explicit directions
- literals
- local bindings
- field access
- indexing and slicing
- primitive arithmetic and logical operators
- explicit register primitives
- explicit component instantiation
- explicit cyclic binding form (`var`)

Surface features that should lower away:

- inferable named arguments such as `dom clk`
- defaults on named arguments
- method syntax
- convenience sugar around record construction

The rule of thumb is:

- the core language should be easy to type check
- the RTL IR should be easy to emit as Verilog

## 7. Typed core IR

This IR should be close to the semantic meaning of the program and far less convenient than the surface syntax.

Properties:

- every expression has a known type
- clock domains are explicit
- defaults and inference are already resolved
- method syntax is already lowered
- cyclic `var` bindings have a dedicated representation

This is the right place to represent:

- fully typed component interfaces
- resolved struct and port types
- explicit register operations
- explicit instantiation wiring

## 7. RTL IR

Lower the typed core IR into an RTL-oriented representation designed for code generation.

Properties:

- explicit registers and next-state expressions
- explicit combinational logic
- explicit module boundaries
- explicit port directions
- stable internal naming model

This IR should be shaped around what must appear in Verilog, not around source syntax.

Examples of concepts that belong here:

- wires
- registers
- combinational assignments
- sequential blocks
- module instances
- reset behavior

## 8. Verilog emitter

The backend should focus on:

- deterministic naming
- readable emitted structure
- faithful reset and clock behavior
- predictable formatting

The emitter should preserve user intent where practical, especially around externally visible names.

## Suggested crate or module structure

This can stay in one crate at first, but should be organized in layers:

```text
packages/
  polar-compiler/
    src/
      syntax/        # tree-sitter integration, CST adapters
      ast/           # AST node definitions
      resolve/       # name resolution
      elaborate/     # defaults, inference, desugaring
      typeck/        # type and clock checking
      core_ir/       # typed semantic IR
      rtl_ir/        # lowering target for Verilog emission
      emit/          # Verilog backend
      diagnostics/   # spans, messages, labels
  tree-sitter-polar/
    grammar.js
    queries/
    test/corpus/
```

If tree-sitter lives as a separate grammar package for editor support, keep the Rust compiler crate consuming generated parser artifacts rather than mixing grammar source into semantic code.

## First implementation slice

The smallest useful end-to-end path is:

1. parse `fn` declarations with named and positional arguments
2. parse simple types including `uint[N] @clk` and `Reset @clk`
3. parse blocks with `let`, `var`, `return`, and simple operators
4. build a small AST
5. run basic name and clock checks
6. lower to a tiny typed core
7. emit a minimal Verilog subset

This slice is narrow enough to prototype quickly and wide enough to validate the architecture.

## Prior art: rustc IR phases

rustc is a useful reference because it faces similar problems — a rich surface language, a need for clean semantic analysis, and a pipeline that must preserve names for readable output.

Its pipeline uses entirely separate, purpose-built IR types for each phase:

| Phase | IR | Identifiers |
|---|---|---|
| Parsing | AST | `ast::Ident` — interned `Symbol` (u32) + `Span`, interned at lex time |
| Name resolution | HIR | `HirId` / `DefId` — unique opaque IDs for every definition |
| Type checking | THIR | HIR nodes with types attached to every expression |
| Borrow checking / optimization | MIR | Control-flow graph, fully desugared |
| Code generation | LLVM IR | Monomorphised concrete instances |

**Key lessons for Polar:**

- **Distinct types per phase, not a parameterized shared tree.** The generic contagion cost (every node type becoming `Expr<I>`, `Item<I>`, etc.) far exceeds any reuse benefit. The transform between IRs is where real work happens and should be explicit. See `planning/surface_ir_discussion.md`.
- **Intern identifiers early.** rustc interns identifier strings at lex time — the AST never carries heap-allocated strings. Polar's Surface IR currently uses `String` for simplicity, but moving to interned symbols before or during name resolution is the right long-term direction.
- **Name resolution converts to opaque IDs.** After the AST, rustc never compares names as strings. `DefId` is a stable, unique identifier for each definition. Polar's elaborated IR should do the same — resolve names to IDs and use those for all subsequent passes.
- **Monomorphisation is late.** rustc keeps generics parametric through HIR, THIR, and MIR, monomorphising only at codegen. For Polar, parameter elaboration (resolving `dom clk`, widths, type parameters) is the equivalent step and should similarly happen as late as is practical — after type checking has had a chance to work on the parametric form.

## Open questions

- exact representation of ports during elaboration versus core typing
- how much component instantiation should look like function calls versus dedicated syntax
- where `impl` methods lower into the core language
- whether the compiler should consume tree-sitter output directly or via a lightweight CST wrapper
