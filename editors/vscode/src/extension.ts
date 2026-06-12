// Thin VS Code language client: launch the `mirin-lsp` binary over stdio and
// let it drive all language features. The TextMate grammar (contributed in
// package.json) stays as a cold-start fallback — VS Code composites it
// underneath the server's semantic tokens, so files have colour before the
// server attaches. See planning/lsp.md (M3).

import { window, workspace, ExtensionContext } from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(_context: ExtensionContext): void {
  const config = workspace.getConfiguration("mirin");
  const command = config.get<string>("server.path") || "mirin-lsp";

  // Same invocation for run and debug — the server is a plain stdio binary.
  const serverOptions: ServerOptions = {
    run: { command, transport: TransportKind.stdio },
    debug: { command, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "mirin" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.mrn"),
    },
  };

  client = new LanguageClient(
    "mirin",
    "Mirin Language Server",
    serverOptions,
    clientOptions,
  );

  client.start().catch((err) => {
    window.showErrorMessage(
      `mirin-lsp failed to start ("${command}"). Set "mirin.server.path" to the ` +
        `mirin-lsp binary (e.g. target/release/mirin-lsp). ${err}`,
    );
  });
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
