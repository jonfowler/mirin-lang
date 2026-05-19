# Copilot Instructions

## Current repository shape

- This repository is currently in a restart/planning state. Treat `planning/top.md` as the source of truth for language goals, and `planning/syntax.md` as the source of truth for the first-pass concrete syntax subset used by examples and tooling work.
- Until more implementation files land, treat code changes as greenfield work rather than assuming an existing compiler, parser, or runtime architecture.

## High-level architecture

- **Polar** is intended to be a hardware description language focused on register-transfer-level correctness, readability, and high-quality generated Verilog.
- The planned type system has **two levels**:
  - library-level checking for compatibility and inference
  - instantiation-time checking for width matching, port compatibility, and other connection rules
- **Ports/interfaces** are a first-class language feature. They define module boundaries, can be used for connections, and allow per-field input/output direction plus embedded parameters.
- **Structs** use syntax similar to ports, but they are strictly positive and do not carry per-field direction annotations.
- **Arrays** and **vecs** are intentionally different concepts:
  - arrays are fixed-size and strictly positive
  - vecs are fixed-size but may contain ports
  - test-time variants may be variable-sized
- **Domains** are part of the type story. For the current syntax/tooling pass, only clock domains are in scope, and resets are written as `Reset @clk`.
- Testing is expected to be integrated into the language itself rather than bolted on later as an external-only workflow.

## Key conventions

- Preserve the repo's stated priority order: **readability first**, then strong RTL semantics, then high-quality Verilog generation.
- Do not collapse the distinction between ports, structs, arrays, vecs, and metadata-bearing types. The design notes treat those as separate concepts with different rules.
- Keep generated naming deterministic and leave room for users to explicitly force names when required.
- Treat clock/reset/domain information as a core semantic feature, not optional decoration.
- Follow the syntax direction shown in `planning/top.md` when extending examples or prototyping:
  - `cmp` introduces a component
  - optional named argument sections use braces
  - `#` marks arguments that may be elided at instantiation
  - `@domain` attaches domain information to values and types
  - examples rely on local `let` bindings and inference-heavy syntax
- For current tooling and parser work, prefer the narrower rules in `planning/syntax.md`: only clock domains are in scope, resets are written as `Reset @clk`, and `#` is reserved for inferable arguments such as clocks.
