//! The LSP backend: an *adapter*, not an analyser. It holds a rope + tree-sitter
//! [`Document`] per open file and (from M2) overlays those buffers onto a
//! `polar-db` query engine. All real analysis lives in `polar-db`; the server
//! never reimplements resolution or type checking (`planning/lsp.md`).

use std::sync::OnceLock;

use dashmap::DashMap;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};
use tracing::info;

use crate::document::Document;

pub struct Backend {
    client: Client,
    /// Open documents, keyed by their URI string. `DashMap` gives the
    /// interior mutability the shared `&self` handlers need.
    documents: DashMap<String, Document>,
    /// Position encoding negotiated in `initialize`. UTF-8 when the client
    /// supports it, else UTF-16 (the LSP default). Set once, before any
    /// document opens.
    encoding: OnceLock<PositionEncodingKind>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
            encoding: OnceLock::new(),
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Prefer UTF-8 (tree-sitter is byte-based, so this avoids per-edit
        // conversion); fall back to the LSP-default UTF-16 if unoffered.
        let offered = params
            .capabilities
            .general
            .and_then(|g| g.position_encodings)
            .unwrap_or_default();
        let encoding = if offered.contains(&PositionEncodingKind::UTF8) {
            PositionEncodingKind::UTF8
        } else {
            PositionEncodingKind::UTF16
        };
        let _ = self.encoding.set(encoding.clone());

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(encoding),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "polar-lsp".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let enc = self.encoding.get().map(|e| e.as_str()).unwrap_or("(unset)");
        info!(encoding = enc, "polar-lsp initialized");
        // Also surface it in the editor's LSP log (proves the client channel).
        self.client
            .log_message(
                MessageType::INFO,
                format!("polar-lsp initialized (position encoding: {enc})"),
            )
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let uri = doc.uri.as_str().to_owned();
        info!(%uri, "did_open");
        self.documents.insert(uri, Document::open(&doc.text));
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.as_str().to_owned();
        // FULL sync: the single change event carries the whole new text.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        match self.documents.get_mut(&uri) {
            Some(mut doc) => doc.set_text(&change.text),
            None => {
                self.documents.insert(uri, Document::open(&change.text));
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.as_str().to_owned();
        info!(%uri, "did_close");
        self.documents.remove(&uri);
    }
}
