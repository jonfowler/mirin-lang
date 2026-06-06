//! The LSP backend: an *adapter*, not an analyser. It holds a rope + tree-sitter
//! [`Document`] per open file and (from M2) overlays those buffers onto a
//! `polar-db` query engine. All real analysis lives in `polar-db`; the server
//! never reimplements resolution or type checking (`planning/lsp.md`).

use std::sync::{Mutex, OnceLock};

use dashmap::DashMap;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};
use tracing::info;
use tree_sitter::Query;

use crate::document::Document;
use crate::encoding::Encoding;
use crate::semantic::Analysis;
use crate::{semantic, semantic_tokens, syntax};

pub struct Backend {
    client: Client,
    /// Open documents, keyed by their URI string. `DashMap` gives the
    /// interior mutability the shared `&self` handlers need.
    documents: DashMap<String, Document>,
    /// Position encoding negotiated in `initialize`. UTF-8 when the client
    /// supports it, else UTF-16 (the LSP default). Set once, before any
    /// document opens.
    encoding: OnceLock<PositionEncodingKind>,
    /// The highlight query, compiled once at startup (`QueryCursor` is
    /// per-request; the immutable `Query` is shared).
    highlight_query: Query,
    /// The incremental analysis engine (salsa db + VFS) for semantic
    /// diagnostics. One per server; serialized behind a `Mutex` since edits
    /// mutate salsa inputs.
    analysis: Mutex<Analysis>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
            encoding: OnceLock::new(),
            highlight_query: semantic_tokens::query(),
            analysis: Mutex::new(Analysis::new()),
        }
    }

    /// The negotiated position encoding (UTF-16 until `initialize` runs).
    fn encoding(&self) -> Encoding {
        self.encoding
            .get()
            .map(Encoding::from_kind)
            .unwrap_or(Encoding::Utf16)
    }

    /// Recompute and publish diagnostics for `uri`: syntactic (ERROR/MISSING
    /// nodes) always; semantic (the `polar-db` query stack) only once the file
    /// parses cleanly, so a syntax typo doesn't trigger a cascade of spurious
    /// name/type errors. All computed under the locks, which are released
    /// before the async client send.
    async fn publish(&self, uri: Uri) {
        let enc = self.encoding();
        let diags = {
            let Some(doc) = self.documents.get(uri.as_str()) else {
                return;
            };
            let mut diags = syntax::diagnostics(&doc.rope, &doc.tree, enc);
            if diags.is_empty() {
                // Clone (cheap: ropey is COW, Tree is ref-counted) so we don't
                // hold the document lock while taking the analysis lock.
                let (rope, tree) = (doc.rope.clone(), doc.tree.clone());
                drop(doc);
                let path = semantic::uri_to_path(&uri);
                let mut analysis = self.analysis.lock().unwrap();
                diags = semantic::diagnostics(&mut analysis, &path, &rope, &tree, enc);
            }
            diags
        };
        self.client.publish_diagnostics(uri, diags, None).await;
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
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic_tokens::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            ..Default::default()
                        },
                    ),
                ),
                document_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
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
        let item = params.text_document;
        let uri = item.uri;
        info!(uri = uri.as_str(), "did_open");
        self.documents
            .insert(uri.as_str().to_owned(), Document::open(&item.text));
        self.publish(uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let key = uri.as_str().to_owned();
        let enc = self.encoding();
        // INCREMENTAL sync: apply each edit in order under the doc lock, then
        // release it before publishing. A range-less change is a whole-document
        // replace (also covers full-sync clients).
        if let Some(mut doc) = self.documents.get_mut(&key) {
            for change in params.content_changes {
                match change.range {
                    Some(range) => doc.apply_incremental(range, &change.text, enc),
                    None => doc.apply_full(&change.text),
                }
            }
        }
        self.publish(uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        info!(uri = uri.as_str(), "did_close");
        self.documents.remove(uri.as_str());
        // Clear diagnostics for the closed document.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri.as_str().to_owned();
        let enc = self.encoding();
        let Some(doc) = self.documents.get(&uri) else {
            return Ok(None);
        };
        let tokens = semantic_tokens::compute(&doc.rope, &doc.tree, &self.highlight_query, enc);
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri.as_str().to_owned();
        let enc = self.encoding();
        let Some(doc) = self.documents.get(&uri) else {
            return Ok(None);
        };
        let symbols = syntax::document_symbols(&doc.rope, &doc.tree, enc);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri.as_str().to_owned();
        let Some(doc) = self.documents.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(syntax::folding_ranges(&doc.tree)))
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = params.text_document.uri.as_str().to_owned();
        let enc = self.encoding();
        let Some(doc) = self.documents.get(&uri) else {
            return Ok(None);
        };
        let ranges = syntax::selection_ranges(&doc.rope, &doc.tree, &params.positions, enc);
        Ok(Some(ranges))
    }
}
