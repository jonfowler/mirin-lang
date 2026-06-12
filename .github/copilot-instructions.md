# Copilot Instructions

Read `CLAUDE.md` at the repository root — it is the maintained, up-to-date
guide for AI assistants working in this repo (commands, architecture, language
concepts, and conventions). Follow it; this file intentionally duplicates
nothing to avoid drift.

Quick orientation: Mirin is an HDL compiled to SystemVerilog. The compiler
(`packages/mirin-compiler/`) is query-based (salsa); concrete syntax lives in
the tree-sitter grammar (`packages/tree-sitter-mirin/`); design decisions are
documented in `planning/` and those documents are the source of truth.
