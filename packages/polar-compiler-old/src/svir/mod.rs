//! SystemVerilog backend — IR data types, HIR-to-SV lowering, and the
//! deterministic pretty-printer that emits `.sv` text.
//!
//! `ir` defines the structural SV IR. `lower` walks flattened HIR and
//! builds an `SvFile`; `emit` produces the on-disk SV text. See
//! `planning/ir_pipeline.md` for the surrounding stages.

pub mod emit;
pub mod ir;
pub mod lower;
