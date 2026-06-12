# Copilot Instructions

Read `CLAUDE.md` at the repository root — it is the maintained, up-to-date
guide for AI assistants working in this repo (commands, architecture, language
concepts, and conventions). Follow it; this file intentionally duplicates
nothing to avoid drift.

Quick orientation: Polar is an HDL compiled to SystemVerilog. The compiler
(`packages/polar-compiler/`) is query-based (salsa); concrete syntax lives in
the tree-sitter grammar (`packages/tree-sitter-polar/`); design decisions are
documented in `planning/` and those documents are the source of truth.
