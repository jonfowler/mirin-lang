# Copilot Instructions

## Current repository shape

- This repository is currently in a restart/planning state. Treat `planning/top.md` as the source of truth for language goals, and `planning/syntax.md` as the source of truth for the first-pass concrete syntax subset used by examples and tooling work.
- The repository is now organized as a small monorepo. The Rust compiler crate lives in `packages/polar-compiler/`, and the tree-sitter grammar lives in `packages/tree-sitter-polar/`.
- Until more implementation files land, treat code changes as greenfield work rather than assuming a stable compiler, parser, or runtime architecture.

## Build, test, and formatting commands

- `cargo test -p polar-compiler` runs the current Rust compiler/parser prototype test suite.
- `cargo test -p polar-compiler parses_add_constant_example -- --exact` runs a single Rust parser test; replace the test name as needed.
- `cargo run -p polar-compiler -- examples/mult_add.plr` parses a `.plr` file and prints the current CST with source spans.
- `cargo fmt --all` formats the Rust workspace.
- `cd packages/tree-sitter-polar && tree-sitter generate` regenerates the tree-sitter parser sources.
- `cd packages/tree-sitter-polar && tree-sitter test` runs the tree-sitter grammar corpus tests.
- The VS Code syntax extension lives in `editors/vscode/`.

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
- Prefer tree-sitter as the concrete-syntax frontend for parsing, highlighting, and future LSP work. Keep Rust code responsible for CST-to-AST lowering, elaboration, and semantic analysis.
- `packages/polar-compiler` now uses tree-sitter to parse source files and build a CST that preserves byte and row/column spans.
