//! `polar-db` — the foundational layer of the query-based compiler.
//!
//! This crate is the start of the front-to-back reimplementation described in
//! `planning/query_engine.md`. It owns the **input** boundary ([`vfs`]) and the
//! query **database** ([`db`]), beginning with a single query — `parse`. Later
//! slices stack the syntactic firewall (`item_tree`), name resolution
//! (`crate_def_map`), and per-def `sig_of`/`body`/`infer` on top, porting logic
//! from the `polar-compiler` reference oracle one stage at a time.
//!
//! Mirrors rust-analyzer's `base-db`: the VFS + database other crates build on.

pub mod db;
pub mod parser;
pub mod vfs;

#[cfg(feature = "spike-salsa")]
pub mod salsa_spike;

pub use db::{Db, ParseTree};
pub use parser::{language, parse_text};
pub use vfs::{FileId, Revision, Vfs};
