#!/usr/bin/env bash
# Build and install the editor tooling (mirin-lsp, mirin-fmt) into ~/.local/bin.
#
# The VS Code extension launches `mirin-lsp` from PATH, so the installed binary
# embeds a snapshot of the tree-sitter grammar and highlight query. Rerun this
# after any grammar, LSP, or formatter change — a stale binary means files
# using new syntax stop parsing, which silently degrades highlighting and
# makes the formatter refuse to run.
set -euo pipefail

cd "$(dirname "$0")/.."

BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

cargo build --release -p mirin-lsp -p mirin-fmt

mkdir -p "$BIN_DIR"
for bin in mirin-lsp mirin-fmt; do
    install -m755 "target/release/$bin" "$BIN_DIR/$bin"
    echo "installed $BIN_DIR/$bin"
done
