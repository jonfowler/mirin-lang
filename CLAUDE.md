# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

This repository is in a restart/planning state. Treat `planning/top.md` as the source of truth for language goals and `planning/syntax.md` as the source of truth for the first-pass concrete syntax subset used by examples and tooling. Until more implementation files land, treat code changes as greenfield work rather than assuming a stable compiler, parser, or runtime architecture.

## Commands

```bash
cargo test -p polar-compiler                                          # full test suite
cargo test -p polar-compiler parses_add_constant_example -- --exact  # single test
cargo run -p polar-compiler -- examples/mult_add.plr                 # parse a .plr file, print CST
cargo fmt --all                                                       # format Rust workspace

cd packages/tree-sitter-polar && tree-sitter generate  # regenerate parser sources
cd packages/tree-sitter-polar && tree-sitter test      # run grammar corpus tests
```

The VS Code syntax extension lives in `editors/vscode/`.

## Architecture

**Polar** is an HDL focused on RTL correctness, readability, and high-quality generated Verilog. The repo is a small monorepo:

- `packages/polar-compiler/` — Rust crate: CST types, diagnostics, CLI. `build.rs` compiles the tree-sitter grammar (C sources) and links it in.
- `packages/tree-sitter-polar/` — Tree-sitter grammar (JavaScript): concrete syntax, highlighting, editor integration.
- `planning/` — Design docs that are the source of truth for language decisions.
- `examples/` / `fail-examples/` — `.plr` source files used in parser tests.

Data flow: `.plr` source → tree-sitter CST → Rust AST lowering → elaboration → type checking → IR → Verilog (later stages are planned but not yet implemented).

Tree-sitter owns concrete syntax; Rust owns CST-to-AST lowering, elaboration, and semantic analysis.

## Key language concepts

- **Ports** are first-class. They define module boundaries, support per-field input/output direction, and can carry embedded parameters. Do not collapse them with structs.
- **Structs** use similar syntax but are strictly positive and carry no direction annotations.
- **Arrays** are fixed-size and strictly positive; **vecs** are fixed-size but may contain ports. These are intentionally different.
- **Domains/clocks**: only clock domains are in scope for the current pass. Clocked values are written `T @clk`; resets as `Reset @clk`. `#` marks inferable arguments (clocks are the primary use case).
- **`fn`** introduces a component; named argument sections use braces `{ }`, positional sections use parens `( )`.
- **`let` vs `var`**: `let x = expr` is a sequential lexical binding (forward-only scope, supports shadowing for pipeline style). `var x: T` declares a block-scoped signal node that can participate in cyclic equations — used for register feedback and mutual structural wiring. See `planning/cycles_and_scoping.md`.
- Testing is expected to be integrated into the language itself, not only external tooling.

## Conventions

- Priority order: **readability first**, then strong RTL semantics, then high-quality Verilog generation.
- Keep generated naming deterministic; leave room for users to force explicit names.
- Treat clock/reset/domain information as core semantics, not optional decoration.
- Before making design decisions, read the relevant file in `planning/`.
