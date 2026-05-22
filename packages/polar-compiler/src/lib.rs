pub mod parser;
pub mod resolve;
pub mod surface_ir;

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
    Identifier, ImplBlock, Item, LetStatement, LowerError, NamedArgument, NamedArgumentList,
    NamedParameter, NodeId, NumberLiteral, Parameter, PathExpression, PortDefinition, PortField,
    PostfixExpression, PostfixOperation, RecordConstructorExpression, RecordFieldType,
    RecordFieldValue, ReturnStatement, SinkArgument, SourceArgument, SourceFile, Statement,
    StructDefinition, SurfaceIrError, TypeExpression, TypeIndex, TypeSuffix, VarStatement,
    lower_cst, parse_surface_file, parse_surface_source,
};
