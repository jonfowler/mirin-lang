//! `polar-db` — the foundational layer of the query-based compiler.
//!
//! The start of the front-to-back reimplementation in `planning/query_engine.md`.
//! Owns the **input** boundary ([`vfs`]) over a salsa database ([`db`]), the
//! stable syntactic identity layer ([`ast_id`]), and (Q1c) the per-file
//! `item_tree` syntactic firewall. Later slices stack name resolution
//! (`crate_def_map`) and per-def `sig_of`/`body`/`infer` on top, porting logic
//! from the `polar-compiler` reference oracle one stage at a time.
//!
//! Mirrors rust-analyzer's `base-db`: the VFS + database other crates build on.

pub mod ast_id;
pub mod db;
pub mod item_tree;
pub mod parser;
pub mod vfs;

pub use ast_id::{AstIdKind, AstIdMap, FileAstId, ast_id_map};
pub use db::{RootDatabase, SourceFile};
pub use item_tree::ItemTree; // the query is `item_tree::item_tree` (avoids a module/fn name clash)
pub use parser::{language, parse_text};
pub use vfs::Vfs;
