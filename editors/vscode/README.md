# Mirin for VS Code

Language support for the [Mirin](../../planning/top.md) HDL: a TextMate grammar
for cold-start colour, plus a thin client for the `mirin-lsp` language server
(semantic tokens, outline, folding, selection ranges, diagnostics, and document
formatting — growing as the server does, see `planning/lsp.md`).

Document formatting is served by `mirin-lsp` via the `mirin-fmt` crate, so
**Format Document** (and `editor.formatOnSave`) reformat `.mrn` files the same
way the `mirin-fmt` CLI does. Files with syntax errors are left untouched.

The extension is editor-agnostic at heart: the same `mirin-lsp` binary serves
Neovim, Helix, and Zed. This package is just the VS Code client.

## Build & run (development)

1. **Build the server:**
   ```bash
   cargo build --release -p mirin-lsp     # produces target/release/mirin-lsp
   ```
2. **Point the client at it** — either put `mirin-lsp` on your `PATH`, or set
   `mirin.server.path` (Settings → Mirin) to the absolute binary path, e.g.
   `/path/to/mirin/target/release/mirin-lsp`.
3. **Build the client:**
   ```bash
   cd editors/vscode
   npm install
   npm run compile        # or `npm run watch` while iterating
   ```
4. **Launch:** open this folder in VS Code and press <kbd>F5</kbd> ("Run Mirin
   Extension"). Open a `.mrn` file in the Extension Development Host.

## Settings

| Setting             | Default     | Description                                  |
| ------------------- | ----------- | -------------------------------------------- |
| `mirin.server.path` | `mirin-lsp` | Path to the language server binary.          |
| `mirin.trace.server`| `off`       | Trace JSON-RPC traffic (`messages`/`verbose`)|

## Packaging

```bash
npm run compile && npx vsce package      # produces mirin-lsp-<version>.vsix
```
