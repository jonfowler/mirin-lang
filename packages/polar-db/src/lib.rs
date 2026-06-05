//! `polar-db` — the query-based compiler front end (`planning/query_engine.md`).
//!
//! Structured by conceptual layer / IR, mirroring rust-analyzer's crate split
//! (`base-db` → `hir-def` → `hir-ty`) and the old compiler's by-IR modules:
//!
//! - [`base`] — the inputs and parsing: the salsa database, the [`vfs`](base::vfs)
//!   overlay, and the tree-sitter [`parser`](base::parser).
//! - [`syntax`] — the per-file **syntactic firewall**: stable
//!   [`ast_id`](syntax::ast_id)s and the lean [`item_tree`](syntax::item_tree).
//! - [`nameres`] — def **identity** ([`ids`](nameres::ids): `DefId`/`DefPath`)
//!   and **name resolution** ([`def_map`](nameres::def_map): the module tree,
//!   imports, prelude).
//! - [`hir`] — the typed-HIR layer: the type vocabulary ([`types`](hir::types))
//!   and the signature query ([`sig`](hir::sig)). Grows `body`/`infer` (Q3c–d).
//!
//! Front-to-back, each stage ports logic from the `polar-compiler` reference
//! oracle one slice at a time.

pub mod base;
pub mod hir;
pub mod nameres;
pub mod syntax;

pub use base::db::{RootDatabase, SourceFile, SourceRoot};
pub use base::parser::{language, parse_text};
pub use base::vfs::Vfs;
pub use hir::body::{Block, Body, BodyDiagnostic, Expr, ExprId, ExprKind, LocalKind, Stmt, body};
pub use hir::infer::{InferDiagnostic, Inference, infer};
pub use hir::sig::{Field, Param, Signature, sig_of};
pub use hir::types::{
    ConstArg, Direction, Domain, GenericArg, GenericArgs, GenericParam, GenericParamKind, LocalId,
    Type, ValueKind,
};
pub use nameres::def_map::{
    Binding, BindingSource, CrateDefMap, DefData, DefDiagnostic, ModuleData, ModuleId, ModuleKind,
    Visibility, crate_def_map,
};
pub use nameres::ids::{
    AnonConstRole, DefId, DefKind, DefPath, DefPathHash, DefPathSegment, DefPathSegmentKind,
    DefRole, Namespace, StableCrateId,
};
pub use syntax::ast_id::{AstIdKind, AstIdMap, FileAstId, ast_id_map};
pub use syntax::item_tree::ItemTree; // query is `syntax::item_tree::item_tree`
