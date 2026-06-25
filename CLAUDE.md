# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

This repository is in a restart/planning state. Treat `planning/top.md` as the source of truth for language goals and `planning/syntax.md` as the source of truth for the first-pass concrete syntax subset used by examples and tooling. Until more implementation files land, treat code changes as greenfield work rather than assuming a stable compiler, parser, or runtime architecture.

## Commands

```bash
cargo test -p mirin-compiler                                          # compiler test suite
cargo test -p mirin-compiler infer -- --exact                         # a single test by name
cargo run -p mirin-compiler -- examples/working/mult_add.mrn          # compile a .mrn → ./sv/<stem>.sv
cargo run -p mirin-compiler -- --emit cst examples/working/mult_add.mrn   # print the CST instead
cargo fmt --all                                                       # format Rust workspace

tests/rtl/run.sh                 # RTL behavioural tests (cocotb + verilator); bootstraps a venv on first run
tests/rtl/run.sh -k counter      # a single RTL test

cd packages/tree-sitter-mirin && tree-sitter generate  # regenerate parser sources
cd packages/tree-sitter-mirin && tree-sitter test      # run grammar corpus tests

scripts/install-tooling.sh   # build + install mirin-lsp/mirin-fmt into ~/.local/bin
                             # rerun after any grammar/LSP/formatter change, or the
                             # editor runs a stale binary that can't parse new syntax
```

The VS Code syntax extension lives in `editors/vscode/`.

## Architecture

**Mirin** is an HDL focused on RTL correctness, readability, and high-quality generated Verilog. The repo is a small monorepo:

- `packages/mirin-compiler/` — the compiler: a query-based, demand-driven front-to-back implementation on salsa (`planning/query_engine.md`), structured by layer (`base` → `syntax` → `nameres` → `hir` → `backend`). Emits SystemVerilog; `build.rs` compiles the tree-sitter grammar (C sources) and links it in. This is the primary `mirin-compiler`.
- `packages/mirin-compiler-old/` — the original whole-crate-pass compiler, kept as a **parity oracle** (the query-based one reached corpus parity at Q5-mono). Off the build path of everything else; retained for reference/diffing.
- `packages/mirin-lsp/` — the language server, built on `mirin-compiler`'s query stack.
- `packages/tree-sitter-mirin/` — Tree-sitter grammar (JavaScript): concrete syntax, highlighting, editor integration.
- `planning/` — Design docs that are the source of truth for language decisions.
- `examples/` / `fail-examples/` — `.mrn` source files used in tests.

Data flow: `.mrn` source → tree-sitter CST → per-file `item_tree` → `crate_def_map` (name resolution) → `sig_of`/`body`/`infer` (typed HIR) → `verilog` (flatten + monomorphise + emit). Each is a salsa query.

Tree-sitter owns concrete syntax; Rust owns CST-to-AST lowering, elaboration, and semantic analysis.

## Key language concepts

- **Ports** are first-class. They define module boundaries, support per-field input/output direction, and can carry embedded parameters. Do not collapse them with structs.
- **Structs** use similar syntax but are strictly positive and carry no direction annotations.
- **Arrays** are fixed-size and strictly positive; **vecs** are fixed-size but may contain ports. These are intentionally different.
- **Domains/clocks**: only clock domains are in scope for the current pass. Clocked values are written `T @clk`; resets as `Reset @clk`. `param`/`dom` arguments (consts and clocks) are inferable by default — usually solved from context rather than passed explicitly.
- **`fn`** introduces a component; named argument sections use braces `{ }`, positional sections use parens `( )`.
- **`let` vs `var`**: `let x = expr` is a sequential lexical binding (forward-only scope, supports shadowing for pipeline style). `var x: T` declares a block-scoped signal node that can participate in cyclic equations — used for register feedback and mutual structural wiring. See `planning/cycles_and_scoping.md`.
- Testing is expected to be integrated into the language itself, not only external tooling.

## Conventions

- Priority order: **readability first**, then strong RTL semantics, then high-quality Verilog generation.
- Keep generated naming deterministic; leave room for users to force explicit names.
- Treat clock/reset/domain information as core semantics, not optional decoration.
- Before making design decisions, read the relevant file in `planning/`. `planning/ir_pipeline.md` is the source of truth for compiler stages.
- Keep `planning/ir_pipeline.md` in sync when you edit the compiler — adding/removing a pass, introducing a new IR type, or otherwise changing stage shape. Keep the doc concise: one paragraph per IR, one row per pass, no implementation details that live in the code.

## Negative-space programming

Make the shapes you *don't* handle as explicit as the ones you do. The compiler
distinguishes two failure modes and must keep them distinct:

- **Well-formed but unhandled / asserted-impossible** → `panic!`/`todo!`/
  `unreachable!` (loud). If a body is type-correct and the input invariant holds,
  any shape the code doesn't cover is a *compiler bug*, not a user error — fail
  loudly at the exact site rather than emitting wrong output or silently falling
  through. Example: a drive-target root that isn't a `Local` after pattern
  desugaring cannot occur, so `place_of` panics instead of guessing.
- **Ill-typed / unsupported-by-design** → soft, clean rejection (`None`,
  `Missing`, a diagnostic). Anything reachable from malformed user input must
  degrade gracefully. The gate is "is the body well-typed?" (body + infer
  diagnostics clean): once past it, switch to loud panics; before it, stay soft.
  Example: MIR lowering returns `Missing` on a body that already has diagnostics,
  but panics on an unhandled well-typed shape.

In practice: don't write a silent `_ => fallthrough` or a default that papers
over an unexpected case. State the assumption in code so a violated invariant
surfaces immediately and pins down where the model broke. Guard the loud panics
behind the well-typed gate so they never fire on user error. When you add a new
shape you can't lower yet, prefer an explicit `todo!("S4: slice-set")` naming the
slice/step over a quiet `0`/empty that miscompiles.

## Commit cadence

Commit after every self-contained chunk of work — a finished pass, a passing
test set, a doc cleanup, a refactor that builds. Don't wait until "the whole
thing is done." Small commits keep the history readable and let us roll back
cleanly when something turns out wrong two slices later.

## Designing new language features

Mirin's compiler is rustc-shaped (staged pipeline, distinct IRs per phase, eager unification with deferred obligations). Before designing a new feature, work the rust analogy:

1. **Find the analogous feature or pass in rustc.** `if`/`when` lower like Rust's block-to-MIR flattening. Method dispatch routes through an `impl_methods` table the way rustc resolves inherent impls. Domain inference borrows the OutsideIn(X) split. Look first; reinvent only when nothing fits.
2. **Research the rust implementation before settling on a design.** Use a sub-agent (Explore or general-purpose) to read the relevant rustc passes/docs when the shape isn't already obvious. Use what you learn to inform the IR choice, the pass placement, and the failure modes. Note the differences too — HDL semantics force divergence (e.g. `var` participates in an equation system, not single-assignment locals).
