# Language server

Polar ships a single editor-agnostic language server (`polar-lsp`) that speaks
LSP over stdio. It reuses the compiler's own tree-sitter parser, so there is one
grammar and one CST shared between the compiler and the editor tooling. The
server starts as a thin syntactic adapter and grows semantic features as the
compiler pipeline (`planning/ir_pipeline.md`) stabilises.

## Why an LSP, not a VS Code extension

An LSP is editor-agnostic: one stdio binary serves VS Code, Neovim, Helix, and
Zed. The alternative — driving tree-sitter inside a VS Code extension via a WASM
build and a JS semantic-tokens provider — is throwaway work the moment the LSP
lands, and it only ever serves one editor.

The decisive fact is that `packages/polar-compiler` already compiles and links
the tree-sitter grammar through the Rust `tree-sitter` crate and exposes
`language()`, a `Parser`, and `collect_syntax_diagnostics` (`parser/tree_sitter.rs`).
A Rust LSP depends on `polar-compiler` and reuses that parser directly. There is
no second parser, no WASM toolchain, and the path from "highlight tokens" to
"resolve names with `resolve.rs`" is a straight line through the same crate.

The existing TextMate grammar (`editors/vscode/syntaxes/polar.tmLanguage.json`)
stays as a cold-start fallback: VS Code composites TextMate colour underneath
LSP semantic tokens, so the file has colour before the server attaches.

## Architecture

A new thin crate, no grammar recompile:

```
packages/
  tree-sitter-polar/   grammar + C sources (unchanged)
  polar-compiler/      links grammar; re-exports language() + parse()
  polar-lsp/   (new)   depends on polar-compiler; stdio binary
editors/vscode/        LSP client (keeps TextMate as fallback)
```

`polar-lsp` is an adapter, not an analyser. Per open document it holds a `ropey`
rope and a `tree_sitter::Tree`; an edit updates both, reparses incrementally, and
maps the result to LSP. All real analysis lives in `polar-compiler` — the server
never reimplements resolution or type checking.

## Stack

| Concern | Choice | Rationale |
| --- | --- | --- |
| Framework | `tower-lsp-server` (community fork) | Maintained (v0.23, Dec 2025); used by Biome/Oxc/ast-grep. **Not** the original `ebkalderon/tower-lsp` — stale since 2023 with an open concurrency-ordering bug (#284). `async-lsp` is the fallback if state-drift bugs appear; its `&mut self` notification model is stricter. |
| Document store | `ropey` | O(log n) edits and line/col↔byte conversion on every change. |
| Sync | `INCREMENTAL` → `Tree::edit(InputEdit)` + `parser.parse(text, Some(&old))` | Reparse cost is proportional to the edit, not the file. Full reparse is an acceptable v0 shortcut. |
| Position encoding | Negotiate UTF-8 via `general.positionEncodings`; UTF-16 fallback at the rope boundary | tree-sitter is byte-based; LSP defaults to UTF-16. This is the #1 bug class — centralise all conversion in one module. |
| Logging | `tracing` → stderr | stdout is the LSP transport. |

## Milestones

### M0 — Skeleton

New `polar-lsp` crate. Re-export `polar_language()`/`parse()` from
`polar-compiler`. `initialize` advertising capabilities and negotiating UTF-8.
`didOpen`/`didChange`/`didClose` maintaining a rope + `Tree` per document.
Server attaches and logs to stderr.

### M1 — Syntactic features

Tree-only, robust, no compiler analysis. The bulk of user-visible value, and it
works against the current grammar today.

- **Semantic tokens** — run `queries/highlights.scm` (already rich) via a cached
  `tree_sitter::Query`, mapping captures (`@type`, `@variable.parameter`,
  `@constructor`, …) to `SemanticTokenType` and modifiers. No reusable
  scm→LSP-token crate exists; the capture table is hand-rolled (~20 lines).
- **Document symbols / outline** — walk `function_definition`,
  `struct_definition`, `port_definition` and their fields/params.
- **Folding ranges** and **selection ranges** (parent-chain expansion).
- **Syntactic diagnostics** — traverse with `node.is_error()` / `is_missing()`.
  Do *not* try to query for these: MISSING nodes are zero-width and unqueryable
  (tree-sitter #650/#1136). Coarse but immediate.

### M2 — Semantic features

Route through the compiler as pipeline stages stabilise.

- **Diagnostics** from elaboration / `typeck.rs`, debounced ~300–500 ms after an
  edit; the cheap reparse still runs per keystroke.
- **Go-to-definition / references / scope-aware highlight** via `resolve.rs`.
  These must use the real resolver, not tree-sitter name matching — name-equality
  matching breaks on Polar's `let` shadowing and `var` scoping by design.
- **Hover** with resolved types; **completion** seeded by node-at-cursor context
  plus resolver symbols (port fields, `param`s, `dom` clocks).

### M3 — Multi-editor and packaging

One `cargo build --release -p polar-lsp` binary serves every editor.

- **VS Code** — thin `vscode-languageclient` extension (~30 lines), reusing
  `editors/vscode/`, keeping the TextMate grammar as fallback.
- **Neovim (0.11+) / Helix** — config-only, no plugin: point `cmd`/`command` at
  the binary and register the `.plr` filetype.
- **Zed** — needs a small WASM extension to register the server; defer.

## Pitfalls

- **Position encoding** — one conversion module; negotiate UTF-8.
- **Query caching** — compile the `Query` once at startup; `QueryCursor` is
  `!Sync`, so use one per request/thread.
- **Noisy errors** — a single typo can produce a large ERROR region; M1
  diagnostics stay coarse until M2's real error recovery.
- **Concurrency drift** — the reason for using the maintained fork; the escape
  hatch is `async-lsp`'s ordered-notification model.
