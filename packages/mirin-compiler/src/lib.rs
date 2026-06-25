//! `mirin-compiler` — the query-based compiler (`planning/query_engine.md`).
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
//! Front-to-back, each stage ported logic from the original whole-crate-pass
//! compiler (now `mirin-compiler-old`, kept as a parity oracle) one slice at a
//! time, reaching corpus parity at Q5-mono.

pub mod backend;
pub mod base;
pub mod hir;
pub mod mir;
pub mod nameres;
pub mod syntax;

pub use backend::ir::{SvFile, SvModule};
pub use backend::lower::{sv_file, sv_module, verilog};
pub use backend::mono_check::{MonoDiagnostic, mono_check};
pub use backend::reserved::reserved_words;
pub use base::db::{RootDatabase, SourceFile, SourceRoot};
pub use base::diagnostics::{Span, render};
pub use base::loader::load_crate;
pub use base::parser::{language, parse_text};
pub use base::vfs::Vfs;
pub use hir::body::{
    Block, Body, BodyDiagnostic, BodyDiagnosticKind, Expr, ExprId, ExprKind, LocalKind, Stmt, body,
};
pub use hir::check::{
    DirectionDiagnostic, DirectionDiagnosticKind, DriverDiagnostic, DriverDiagnosticKind,
    check_drivers, completeness, directions,
};
pub use hir::infer::{InferDiagnostic, InferDiagnosticKind, Inference, infer};
pub use hir::sig::{Field, Param, Signature, sig_of};
pub use hir::types::{
    ConstArg, Direction, Domain, DomainSort, GenericArgs, GenericParam, InferVar, LocalId, Term,
    TermKind, Type, ValueKind,
};
pub use mir::ir::{MExpr, MExprId, MExprKind, Mir, Place, Projection};
pub use mir::lower::mir_of;
pub use mir::pretty::pretty as pretty_mir;
pub use nameres::def_map::{
    Binding, BindingSource, CrateDefMap, DefData, DefDiagnostic, DefDiagnosticKind, ModuleData,
    ModuleId, ModuleKind, Visibility, builtin_type_names, crate_def_map,
};
pub use nameres::ids::{
    AnonConstRole, DefId, DefKind, DefPath, DefPathHash, DefPathSegment, DefPathSegmentKind,
    DefRole, Namespace, StableCrateId,
};
pub use syntax::ast_id::{AstIdKind, AstIdMap, FileAstId, ast_id_map};
pub use syntax::item_tree::ItemTree; // query is `syntax::item_tree::item_tree`
pub use syntax::syntax_errors::{SyntaxError, syntax_errors};
