//! The **MIR** layer — a typed, derived mid-level IR between the HIR
//! (`body` + `infer`) and SystemVerilog emission.
//!
//! - [`ir`] — the MIR data types ([`Mir`](ir::Mir), [`MExpr`](ir::MExpr), …):
//!   a faithful typed mirror of the HIR body, with types baked on the nodes and
//!   dispatch resolved.
//! - [`lower`] — the [`mir_of`](lower::mir_of) query: HIR→MIR lowering.
//! - [`const_eval`] — compile-time evaluation over MIR value expressions
//!   (slice endpoints, `const if`).

pub mod const_eval;
pub mod ir;
pub mod lower;
pub mod pretty;
