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
use crate::surface_ir::{Direction, NodeId};

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
    Struct(HirStruct),
    Port(HirPort),
    // Impl lands later — out of basic first-pass scope.
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirStruct {
    pub def_id: DefId,
    pub name: String,
    pub fields: Vec<HirStructField>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirStructField {
    pub name: String,
    pub ty: HirType,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirPort {
    pub def_id: DefId,
    pub name: String,
    /// Port-level named parameters (most commonly `#clk: Clock`). Lowered the
    /// same way as function named parameters; type-checking later validates
    /// uses inside field types.
    pub named_params: Vec<HirParam>,
    pub fields: Vec<HirPortField>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirPortField {
    pub direction: Direction,
    pub name: String,
    pub ty: HirType,
    pub span: SourceSpan,
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
    /// `in`/`out` annotation from the source. Currently only meaningful for
    /// port-typed positional parameters; preserved here so later passes can
    /// validate uses against the declared direction.
    pub direction: Option<Direction>,
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
    Record(HirRecord),
}

/// A record-literal constructor, e.g. `packet { valid: false, payload: 0 }`.
/// `struct_def` is the `DefId` of the struct type; `fields` carries values in
/// source order, keyed by textual field name. Field-name resolution against
/// the struct's declared fields happens in type-checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirRecord {
    pub struct_def: DefId,
    pub fields: Vec<HirRecordField>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirRecordField {
    pub name: String,
    pub value: HirExpr,
    pub span: SourceSpan,
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
    pub span: SourceSpan,
}

/// The top branch of HIR types. The factoring matters during type inference:
/// a `let x;` introduces `?T = Var(_)` and only narrows to `Value(...)` or
/// `Port(...)` once uses force the kind. Domains live only on `Value` because
/// ports have no top-level domain — clocking flows through their fields via
/// the port's clock parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirTypeKind {
    /// Type-inference variable. The unifier resolves it to one of the other
    /// branches; lowering never produces this directly.
    Var(TypeVar),
    /// Scalars (`uint(N)`, `bool`, `Reset`) and structs. Carries a single
    /// domain.
    Value(ValueType),
    /// Port type. Does not carry a top-level domain — clocking is parametric
    /// over the port's `#clk` (or similar) named parameter, which flows into
    /// the per-field types.
    Port(PortTypeRef),
    /// Meta-kind: a clock domain itself (e.g. `#clk: Clock`). Never the type
    /// of a value-level expression.
    Clock,
    /// Meta-kind: compile-time integer (e.g. `const bits: usize`). Never the
    /// type of a value-level expression.
    Usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueType {
    pub kind: ValueKind,
    pub domain: Domain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueKind {
    UInt {
        /// Width expression. Const-ness is checked by a dedicated pass; this
        /// keeps the HIR free of a parallel `ConstExpr` enum that would have to
        /// grow alongside `HirExpr`.
        width: Box<HirExpr>,
    },
    Bool,
    Reset,
    /// A user-defined struct.
    Struct {
        def: DefId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortTypeRef {
    pub def: DefId,
    // Future: type/clock arguments for parametric ports.
}

/// Type-inference variable for the value-vs-port-vs-meta branch. Produced by
/// the type checker; never by lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVar(pub u32);

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
