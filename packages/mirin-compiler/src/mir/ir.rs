//! The MIR data types — a typed, derived mid-level IR (`planning/mir.md`).
//!
//! MIR mirrors the HIR [`Body`](crate::hir::body::Body) one node at a time, but
//! with two deliberate differences that pay off downstream:
//!
//! 1. **Types ride on the nodes.** Every [`MExpr`] carries its resolved
//!    [`Type`], baked from `infer` once at lowering. Transforms (slice desugar,
//!    flatten, mono, inline) never reach back into the `ExprId`-keyed inference
//!    side-table — the type is local.
//! 2. **Dispatch is resolved.** The four HIR call shapes (plain `Call`,
//!    `MethodCall`, `TypePathCall`, and operator-as-call) collapse into one
//!    [`MExprKind::Call`] carrying the resolved callee [`DefId`] and its baked
//!    generic substitution. There is no method-dispatch left to do on MIR.
//!
//! What MIR does **not** do yet (scheduled slices, see planning/mir_progress.md):
//! places/projections (S2), flatten (S5), slice desugar (S4), mono (S6),
//! inline (S7). Until then MIR is a faithful structural mirror.

use crate::base::diagnostics::Span;
use crate::hir::body::{LocalKind, NumBase, VerilogTemplate};
use crate::hir::types::{ConstArg, LocalId, Term, Type};
use crate::nameres::ids::DefId;

/// Index into a [`Mir`]'s expression arena. Owner-relative, reset per def —
/// the MIR analogue of [`ExprId`](crate::hir::body::ExprId). MIR indices are
/// **not** the HIR `ExprId`s: lowering rebuilds the arena, so identity is fresh.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub struct MExprId(pub u32);

/// A function/method body lowered to MIR. The shape parallels `Body`: an
/// expression arena, typed locals (the first `param_count` are the value
/// params, ids matching `sig_of`/`body`), and a top-level block.
#[derive(Clone, PartialEq, Eq, Default, salsa::Update)]
pub struct Mir<'db> {
    pub(crate) exprs: Vec<MExpr<'db>>,
    pub(crate) locals: Vec<MLocal<'db>>,
    pub(crate) param_count: u32,
    pub(crate) block: MBlock,
    /// `Some` for an inline-verilog fn (`= verilog { … }`); `block` is empty.
    /// Carried through verbatim — MIR does not interpret the template.
    pub(crate) verilog: Option<VerilogTemplate<'db>>,
}

impl<'db> Mir<'db> {
    pub fn expr(&self, id: MExprId) -> &MExpr<'db> {
        &self.exprs[id.0 as usize]
    }

    pub fn exprs(&self) -> &[MExpr<'db>] {
        &self.exprs
    }

    pub fn local(&self, id: LocalId) -> &MLocal<'db> {
        &self.locals[id.0 as usize]
    }

    pub fn locals(&self) -> &[MLocal<'db>] {
        &self.locals
    }

    pub fn param_count(&self) -> u32 {
        self.param_count
    }

    pub fn block(&self) -> &MBlock {
        &self.block
    }

    pub fn verilog(&self) -> Option<&VerilogTemplate<'db>> {
        self.verilog.as_ref()
    }
}

/// A typed local: a value param, `let`, `var`, or `for`-bound. The type is
/// resolved from inference (no `declared_ty` vs inferred split — MIR carries the
/// one resolved type).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct MLocal<'db> {
    pub name: String,
    pub kind: LocalKind,
    pub ty: Type<'db>,
    /// For a result place (`return`, named result, tuple part), the SV port base
    /// its leaves emit under (`result`, `result__0`, …). Carried from HIR.
    pub result_base: Option<String>,
    pub mutable: bool,
}

/// A typed MIR expression. The type is baked from inference; the span is
/// def-relative (the renderer adds the def start), as in HIR.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct MExpr<'db> {
    pub kind: MExprKind<'db>,
    pub ty: Type<'db>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum MExprKind<'db> {
    /// An unresolved / error expression. Kept so lowering of an error body stays
    /// total; the node's `ty` is `Type::Error`.
    Missing,
    /// A numeric literal. Subsumes HIR `TypedLiteral` — the explicit type is on
    /// the [`MExpr::ty`], so there is no separate typed-literal node.
    Number(i128, NumBase),
    /// A boolean literal.
    Bool(bool),
    /// A resolved local (param / let / var / for-bound).
    Local(LocalId),
    /// The enclosing def's i-th generic (Const-kind) parameter, as a value.
    ConstParam(u32),
    /// An associated-const projection in value position (`A::bit_size`).
    ConstAssoc {
        item: DefId<'db>,
        self_ty: Type<'db>,
    },
    /// A resolved item reference (constructor, builtin, fn-as-value).
    Def(DefId<'db>),
    /// `[a, b, c]` — vector construction.
    VecLit(Vec<MExprId>),
    /// `(a, b)` — tuple construction. Arity ≥ 2.
    TupleLit(Vec<MExprId>),
    /// `[e; N]` — repeat construction; the length is a const expression.
    VecRepeat { elem: MExprId, len: ConstArg<'db> },
    /// `v[i]` — single-element indexing.
    Index { base: MExprId, index: MExprId },
    /// Field access `recv.field`.
    Field { receiver: MExprId, field: String },
    /// A resolved call. Unifies plain calls, method calls, type-path calls, and
    /// operators. `callee` is the resolved def. `substs` is the **inference-
    /// recorded** call subst (callee-param order, deep-resolved, possibly empty)
    /// — *not* the final ground/mono subst: mono (S6) resolves trait-instance
    /// overrides and fills unbound type generics from it (cf. `backend::lower`'s
    /// `match_type` + `node_subst`). A `range`-builtin plain call records no
    /// subst (empty) — recognised by name downstream. `receiver` is `Some` for
    /// method calls (`recv.m(args)`), `None` otherwise.
    Call {
        callee: DefId<'db>,
        substs: Vec<Term<'db>>,
        receiver: Option<MExprId>,
        args: Vec<Conn>,
        named: Vec<MNamedArg>,
    },
    /// A builtin method that is **not** a resolved def — `reg`, `posedge`,
    /// `replace`, `enumerate`. Inference types these structurally and the backend
    /// recognises them by name (see `infer`/`backend::lower`), so they have no
    /// `DefId` to fold into [`MExprKind::Call`]. Kept as a named primitive.
    Builtin {
        method: BuiltinMethod,
        receiver: MExprId,
        args: Vec<Conn>,
    },
    /// `Ctor { field = value, field => target, … }`.
    Record {
        ctor: Option<DefId<'db>>,
        fields: Vec<MRecordField>,
    },
    /// `if cond { … } else { … }` — runtime conditional.
    If {
        cond: MExprId,
        then_branch: MBlock,
        else_branch: MBlock,
    },
    /// `const if cond { … } else { … }` — compile-time conditional. Folded by a
    /// later MIR pass; kept structural here.
    ConstIf {
        cond: MExprId,
        then_branch: MBlock,
        else_branch: MBlock,
    },
    /// `x[lo..hi]` / `x[off..+w]` — a slice. Desugared by the slice pass (S4);
    /// kept structural here so the type-directed desugar has the operand type.
    Slice {
        base: MExprId,
        lo: Option<MExprId>,
        hi: Option<MExprId>,
        width: Option<MExprId>,
    },
    /// `when event { … }` — registered-state primitive.
    When {
        event: MExprId,
        body: MBlock,
        init: Option<MExprId>,
    },
    /// A block in expression position.
    Block(MBlock),
}

/// The closed set of builtin methods inference handles structurally (no def).
/// A closed enum makes the assumption explicit: lowering maps the method name
/// here and panics on anything outside the set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::Update)]
pub enum BuiltinMethod {
    /// `self.reg(rstn, init)` — the clocked register primitive.
    Reg,
    /// `clk.posedge()` — a clock-edge event.
    Posedge,
    /// `v.replace(i, x)` — functional single-element update.
    Replace,
    /// `v.enumerate()` — `Vec(N,A)` → `Vec(N,(integer,A))`.
    Enumerate,
}

/// An addressable location — the target of a driving equation. Roots at a local
/// and applies projections. HDL drive targets are exactly `Local`/`Field`/
/// `Index` chains (cf. the backend's `backend_root_local`), so a place always
/// has a `Local` base; a non-place LHS is a lowering invariant violation.
///
/// S2b will extend places to out-connection / out-record / out-arg targets;
/// S4 will add a `BitRange` projection for slice-set.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct Place {
    pub base: LocalId,
    pub projections: Vec<Projection>,
}

/// One step of a place projection, applied base→leaf order.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum Projection {
    /// `.field` — also tuple parts (`x.0` is `Field("0")`, reusing field machinery).
    Field(String),
    /// `[index]` — an element index. In a drive target this is a genvar/const
    /// (or a runtime index for a partial drive); kept as an expression.
    Index(MExprId),
}

/// One connection at a call/record site, carrying its direction. `In` flows a
/// value into the callee/constructor; `Out` (`=> target`, or an `in`-direction
/// record field) is a caller [`Place`] the callee drives back. This single
/// direction-carrying model unifies every connection site (positional, named,
/// record field) — the substrate the emission retarget (S3) and named-args
/// handling build on, replacing the backend's per-site direction re-derivation.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum Conn {
    /// A value flowing into the callee (`f(v)`, `f{ x = v }`).
    In(MExprId),
    /// A caller place the callee drives (`f{ x => target }`).
    Out(Place),
}

/// A named-section connection (`f{ name = v, name => target, name }`).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct MNamedArg {
    pub name: String,
    pub conn: Conn,
}

/// A record/constructor field connection.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct MRecordField {
    pub name: String,
    pub conn: Conn,
}

/// A block: a sequence of statements and an optional tail expression.
#[derive(Clone, PartialEq, Eq, Default, salsa::Update)]
pub struct MBlock {
    pub stmts: Vec<MStmt>,
    pub tail: Option<MExprId>,
}

/// A MIR statement. Mirrors HIR `Stmt`; an equation's `lhs` is a [`Place`] (S2).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum MStmt {
    /// `let x = value;`
    Let { local: LocalId, value: MExprId },
    /// `var x;` declaration.
    VarDecl { local: LocalId },
    /// A driving equation / connection: `lhs = rhs;`. The LHS is a [`Place`]
    /// (a resolved drive target), not a value expression.
    Equation { lhs: Place, rhs: MExprId },
    /// `return value;`
    Return { value: MExprId },
    /// A bare expression statement.
    Expr(MExprId),
    /// Statement-form `when`: the body's equations are clocked partial drives.
    When {
        event: MExprId,
        body: MBlock,
        init: Option<MBlock>,
    },
    /// `for x in v { … }` — structural replication.
    For {
        index: Option<LocalId>,
        elem: LocalId,
        iter: MExprId,
        body: MBlock,
    },
}
