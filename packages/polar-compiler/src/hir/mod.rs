//! High-level Intermediate Representation.
//!
//! See `planning/hir.md` for the design rationale. HIR is the first IR that is
//! structured for semantic analysis rather than for matching source syntax:
//! name resolution is baked in, method-call sugar is desugared, named and
//! positional arguments are unified into a single per-callee slot list, and
//! `var` declarations are split from the equations that drive them.
//!
//! This module defines the data types only. Lowering from Surface IR lives in
//! the [`lower`] submodule.

pub mod lower;

pub use lower::{HirLowerError, HirLowerErrorKind, lower_to_hir};

use crate::SourceSpan;
use crate::resolve::{DefId, LocalKind};
use crate::surface_ir::NodeId;

/// Index into a function's `locals` table. Dense and per-function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// Identifier for an HIR node. Allocated from a per-source-file counter during
/// lowering. Distinct from surface `NodeId`: synthesized nodes (e.g. the
/// receiver moved into a method's `self` slot) get fresh `HirId`s with no
/// surface counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId(pub u32);

/// Per-local information owned by the enclosing `HirFn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirLocalInfo {
    pub kind: LocalKind,
    pub name: String,
    pub span: SourceSpan,
    /// Back-pointer to the surface identifier that introduced this local.
    /// Useful for diagnostics that want to refer back to the original token.
    pub surface_node: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSourceFile {
    pub items: Vec<HirItem>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirItem {
    Fn(HirFn),
    // Struct/Port/Impl land later — out of first-pass scope.
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFn {
    pub def_id: DefId,
    pub name: String,
    /// Function signature: named-section parameters first, in declaration order;
    /// positional-section parameters second, also in declaration order. Call
    /// sites slot arguments against this vector index-for-index.
    pub params: Vec<HirParam>,
    pub return_type: Option<HirType>,
    /// Locals owned by this function, indexed by `LocalId`. Includes
    /// parameters, `let`s, `var`s, and implicit vars introduced by `=>`.
    pub locals: Vec<HirLocalInfo>,
    pub body: HirBlock,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirParam {
    pub local: LocalId,
    pub section: ParamSection,
    pub inferable: bool,
    pub is_const: bool,
    pub ty: HirType,
    pub default: Option<HirExpr>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamSection {
    Named,
    Positional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirBlock {
    pub statements: Vec<HirStmt>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirStmt {
    Let(HirLet),
    VarDecl(HirVarDecl),
    Equation(HirEquation),
    Return(HirExpr),
    Expr(HirExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirLet {
    pub local: LocalId,
    pub value: HirExpr,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirVarDecl {
    pub local: LocalId,
    pub ty: Option<HirType>,
    pub span: SourceSpan,
}

/// A single driver for a `var` signal node. `var x: T = init;` lowers to a
/// `VarDecl` followed by an `Equation` whose `rhs` is the initializer. Plain
/// assignments (`x = expr;`) and source connections (`comp { out => x }()`)
/// also lower to `Equation`. The single-driver / undriven checks count
/// equations per `lhs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirEquation {
    pub lhs: LocalId,
    pub rhs: HirExpr,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirExpr {
    pub kind: HirExprKind,
    /// Filled by type inference. `None` after lowering.
    pub ty: Option<HirType>,
    pub span: SourceSpan,
    pub id: HirId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirExprKind {
    Const(ConstValue),
    Local(LocalId),
    Binary(BinOp, Box<HirExpr>, Box<HirExpr>),
    Call(HirCall),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Multiply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    Integer(u64),
    Bool(bool),
}

/// A fully-elaborated call. Every parameter of the callee has a corresponding
/// entry in `args`; method-style sugar has already been desugared into a
/// `Given` entry in the callee's `self` slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirCall {
    pub callee: DefId,
    pub args: Vec<HirArg>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirArg {
    /// User-supplied expression — either positional or named in source.
    Given(HirExpr),
    /// The callee's declared default substituted because no argument was supplied.
    Default(HirExpr),
    /// An inferable parameter (`#`-marked) the type checker must resolve.
    Inferable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirType {
    pub kind: HirTypeKind,
    /// `None` for the `Clock` and `Usize` meta-kinds; `Some(_)` for value types
    /// (`uint(N)`, `bool`, `Reset`).
    pub domain: Option<Domain>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirTypeKind {
    UInt {
        /// Width expression. Const-ness is checked by a dedicated pass; this
        /// keeps the HIR free of a parallel `ConstExpr` enum that would have to
        /// grow alongside `HirExpr`.
        width: Box<HirExpr>,
    },
    Bool,
    Reset,
    Clock,
    Usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Domain {
    /// The domain of compile-time-constant values. Supertype of every concrete
    /// clock domain in the value lattice; rejected at the kind level by
    /// operations that require a concrete `Clock` (see `planning/domain_checking.md`).
    Const,
    /// A concrete clock domain referenced via a `Clock`-typed local (typically
    /// a `#clk` parameter).
    Clock(LocalId),
    /// Domain not yet inferred. Type inference must replace this with `Const`
    /// or `Clock(_)` before subsequent passes run.
    Unspecified,
}
