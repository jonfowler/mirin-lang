//! Typed-HIR stage — type and domain checking.
//!
//! Inputs: untyped HIR + name resolution.
//! Outputs: a `TypeCheckResult` carrying per-expression / per-local types,
//! method dispatch resolutions, residual constraints, and obligation
//! queue. The HIR tree itself is not modified — type info lives in side
//! tables read by the late-lowering and backend passes.
//!
//! See `planning/type_inference.md` and `planning/parametricity.md`.

pub mod normal_const;
pub mod typeck;
