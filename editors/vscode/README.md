# Polar for VS Code

Language support for the [Polar](../../planning/top.md) HDL: a TextMate grammar
for cold-start colour, plus a thin client for the `polar-lsp` language server
(semantic tokens, outline, folding, selection ranges, and diagnostics — growing
as the server does, see `planning/lsp.md`).

The extension is editor-agnostic at heart: the same `polar-lsp` binary serves
Neovim, Helix, and Zed. This package is just the VS Code client.

## Build & run (development)

1. **Build the server:**
   ```bash
   cargo build --release -p polar-lsp     # produces target/release/polar-lsp
   ```
2. **Point the client at it** — either put `polar-lsp` on your `PATH`, or set
   `polar.server.path` (Settings → Polar) to the absolute binary path, e.g.
   `/path/to/polar/target/release/polar-lsp`.
3. **Build the client:**
   ```bash
   cd editors/vscode
   npm install
   npm run compile        # or `npm run watch` while iterating
   ```
4. **Launch:** open this folder in VS Code and press <kbd>F5</kbd> ("Run Polar
   Extension"). Open a `.plr` file in the Extension Development Host.

## Settings

| Setting             | Default     | Description                                  |
| ------------------- | ----------- | -------------------------------------------- |
| `polar.server.path` | `polar-lsp` | Path to the language server binary.          |
| `polar.trace.server`| `off`       | Trace JSON-RPC traffic (`messages`/`verbose`)|

## Packaging

```bash
npm run compile && npx vsce package      # produces polar-lsp-<version>.vsix
```
