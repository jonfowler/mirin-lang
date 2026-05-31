//! Late HIR-to-HIR lowering passes that run after type checking.
//!
//! `lower_block_expressions` flattens `Block`/`If`/`When` expressions
//! into statement-form, `method_lower` rewrites `MethodCall` into
//! `Call`, `out_args` desugars user-fn calls into out-arg form, and
//! `flatten` erases ports/structs at value positions into per-field
//! locals. After this stage the HIR is "thin": every call shape is a
//! direct `HirCall`, every aggregate has been split into leaves, and
//! the SV backend only needs to map structural shapes onto SV
//! constructs.

pub mod flatten;
pub mod lower_block_expressions;
pub mod method_lower;
pub mod out_args;
