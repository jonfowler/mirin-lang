//! Surface stage — concrete syntax tree, surface IR, direction checking.
//!
//! Inputs: a `.plr` text file.
//! Outputs: a `SourceFile` (surface IR) ready for name resolution and HIR
//! lowering. The direction pass runs over the surface IR to flag
//! connection-operator misuse before the more expensive HIR work begins.

pub mod direction;
pub mod ir;
pub mod parser;
