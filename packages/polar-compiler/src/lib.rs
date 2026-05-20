pub mod parser;
pub mod surface_ir;

pub use parser::tree_sitter::{
    Cst, CstChild, CstNode, ParseError, ParsedSource, SourceExcerpt, SourcePosition, SourceSpan,
    SyntaxDiagnostic, language, parse_file, parse_file_with_diagnostics, parse_source,
    parse_source_with_diagnostics, render_parse_error,
};
pub use surface_ir::{
    ArgumentList, AssignmentStatement, BinaryExpression, BinaryOperator, Block,
    ComponentDefinition, Expression, ExpressionStatement, FieldAccess, FunctionDefinition,
    Identifier, ImplBlock, Item, LetStatement, LowerError, NamedArgument, NamedArgumentList,
    NamedParameter, NumberLiteral, Parameter, PathExpression, PortDefinition, PortField,
    PostfixExpression, PostfixOperation, RecStatement, RecordConstructorExpression,
    RecordFieldType, RecordFieldValue, ReturnStatement, SliceExpression, SourceFile, Statement,
    StructDefinition, SurfaceIrError, TypeArgumentList, TypeExpression, TypeIndex, TypeSuffix,
    parse_surface_file, parse_surface_source,
};
