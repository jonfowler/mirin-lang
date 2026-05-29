pub mod direction;
pub mod hir;
pub mod parser;
pub mod resolve;
pub mod surface_ir;
pub mod sv_emit;
pub mod sv_ir;
pub mod sv_lower;
#[cfg(test)]
pub mod test_support;
pub mod typeck;
#[cfg(test)]
mod verilator_lint;

pub use direction::{
    DirectionError, DirectionErrorKind, NamedArgumentOperator, check_directions,
    render_direction_errors,
};
pub use hir::lower_block_expressions::{BlockExprLowering, lower_block_expressions};
pub use hir::{
    DriverError, DriverErrorKind, FlattenError, FlattenErrorKind, OutArgsError, OutArgsErrorKind,
    check_drivers, desugar_user_calls, flatten_aggregates, lower_method_calls,
    render_driver_errors, render_flatten_errors,
};
pub use parser::tree_sitter::{
    Cst, CstChild, CstNode, ParseError, ParsedSource, SourceExcerpt, SourcePosition, SourceSpan,
    SyntaxDiagnostic, language, parse_file, parse_file_with_diagnostics, parse_source,
    parse_source_with_diagnostics, render_parse_error,
};
pub use resolve::{
    DefId, DefInfo, DefKind, LocalInfo, LocalKind, Res, ResolveError, ResolveErrorKind,
    ResolveResult, render_resolve_errors, resolve_file,
};
pub use surface_ir::{
    ArgumentList, AssignmentStatement, BinaryExpression, BinaryOperator, Block,
    ConnectionDirection, Expression, ExpressionStatement, FieldAccess, FunctionDefinition,
    Identifier, IfExpression, ImplBlock, Item, LetStatement, LowerError, NamedArgument,
    NamedArgumentList, NamedParameter, NodeId, NumberLiteral, ParamKind, Parameter, PathExpression,
    PortDefinition, PortField, PostfixExpression, PostfixOperation, RecordConstructorExpression,
    RecordFieldType, RecordFieldValue, ReturnStatement, SinkArgument, SourceArgument, SourceFile,
    Statement, StructDefinition, SurfaceIrError, TypeExpression, TypeIndex, TypeSuffix,
    VarStatement, lower_cst, parse_surface_file, parse_surface_source,
};
pub use sv_emit::{EmitError, EmitErrorKind, emit as emit_sv, render_emit_errors};
pub use sv_lower::lower_to_sv;
pub use typeck::{WidthCheckResult, check_width_obligations};
