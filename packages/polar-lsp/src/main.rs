//! `polar-lsp` — an editor-agnostic language server for Polar that speaks LSP
//! over stdio. It reuses `polar-db`'s tree-sitter parser and (from M2) its
//! incremental query engine, so there is one grammar and one analysis shared by
//! the compiler and the editor tooling (`planning/lsp.md`).

mod document;
mod server;

use server::Backend;
use tower_lsp_server::{LspService, Server};

#[tokio::main]
async fn main() {
    // stdout is the LSP transport; all logging goes to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
