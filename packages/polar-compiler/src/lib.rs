pub mod hir;
pub mod hirt;
pub mod hirtl;
pub mod resolve;
pub mod surface;
pub mod svir;

#[cfg(test)]
pub mod test_support;
#[cfg(test)]
mod verilator_lint;

pub use hir::{DriverError, DriverErrorKind, check_drivers, lower_to_hir, render_driver_errors};
pub use hirt::typeck::{WidthCheckResult, check_width_obligations};
pub use hirtl::flatten::{
    FlattenError, FlattenErrorKind, flatten_aggregates, render_flatten_errors,
};
pub use hirtl::lower_block_expressions::{BlockExprLowering, lower_block_expressions};
pub use hirtl::method_lower::lower_method_calls;
pub use hirtl::out_args::{OutArgsError, OutArgsErrorKind, desugar_user_calls};
pub use resolve::{
    DefId, DefInfo, DefKind, LocalInfo, LocalKind, Res, ResolveError, ResolveErrorKind,
    ResolveResult, render_resolve_errors, resolve_file,
};
pub use surface::direction::{
    DirectionError, DirectionErrorKind, NamedArgumentOperator, check_directions,
    render_direction_errors,
};
pub use surface::ir::{
    ArgumentList, AssignmentStatement, BinaryExpression, BinaryOperator, Block,
    ConnectionDirection, Expression, ExpressionStatement, FieldAccess, FunctionDefinition,
    Identifier, IfExpression, ImplBlock, Item, LetStatement, LowerError, ModuleBody,
    ModuleDefinition, NamedArgument, NamedArgumentList, NamedParameter, NodeId, NumberLiteral,
    ParamKind, Parameter, PathExpression, PortDefinition, PortField, PostfixExpression,
    PostfixOperation, RecordConstructorExpression, RecordFieldType, RecordFieldValue,
    ReturnStatement, SinkArgument, SourceArgument, SourceFile, Statement, StructDefinition,
    SurfaceIrError, TypeExpression, TypeIndex, TypeSuffix, VarStatement, WhenExpression, lower_cst,
    parse_surface_file, parse_surface_source,
};
pub use surface::loader::{
    FsProvider, LoadError, LoadedCrate, MapProvider, SourceProvider, load_crate, load_crate_from_fs,
};
pub use surface::parser::tree_sitter::{
    Cst, CstChild, CstNode, ParseError, ParsedSource, SourceExcerpt, SourcePosition, SourceSpan,
    SyntaxDiagnostic, language, parse_file, parse_file_with_diagnostics, parse_source,
    parse_source_with_diagnostics, render_parse_error,
};
pub use svir::emit::{EmitError, EmitErrorKind, emit as emit_sv, render_emit_errors};
pub use svir::lower::lower_to_sv;
