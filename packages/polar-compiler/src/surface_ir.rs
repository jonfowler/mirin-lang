use std::{fmt, fs, path::Path};

use crate::{Cst, CstNode, ParseError, SourceSpan, parse_source};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub span: SourceSpan,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Component(ComponentDefinition),
    Struct(StructDefinition),
    Port(PortDefinition),
    Impl(ImplBlock),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    pub span: SourceSpan,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentDefinition {
    pub span: SourceSpan,
    pub name: Identifier,
    pub named_parameters: Vec<NamedParameter>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<TypeExpression>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDefinition {
    pub span: SourceSpan,
    pub name: Identifier,
    pub parameters: Vec<Parameter>,
    pub constructor: Option<Identifier>,
    pub fields: Vec<RecordFieldType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDefinition {
    pub span: SourceSpan,
    pub name: Identifier,
    pub named_parameters: Vec<NamedParameter>,
    pub parameters: Vec<Parameter>,
    pub constructor: Option<Identifier>,
    pub fields: Vec<PortField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplBlock {
    pub span: SourceSpan,
    pub name: Identifier,
    pub named_parameters: Vec<NamedParameter>,
    pub parameters: Vec<Parameter>,
    pub functions: Vec<FunctionDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDefinition {
    pub span: SourceSpan,
    pub name: Identifier,
    pub named_parameters: Vec<NamedParameter>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<TypeExpression>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedParameter {
    pub span: SourceSpan,
    pub inferable: bool,
    pub is_const: bool,
    pub name: Identifier,
    pub ty: Option<TypeExpression>,
    pub default: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub span: SourceSpan,
    pub direction: Option<Direction>,
    pub inferable: bool,
    pub is_const: bool,
    pub name: Identifier,
    pub ty: TypeExpression,
    pub default: Option<Expression>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordFieldType {
    pub span: SourceSpan,
    pub name: Identifier,
    pub ty: TypeExpression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortField {
    pub span: SourceSpan,
    pub direction: Direction,
    pub name: Identifier,
    pub ty: TypeExpression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub span: SourceSpan,
    pub statements: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Let(LetStatement),
    Return(ReturnStatement),
    Var(VarStatement),
    Assignment(AssignmentStatement),
    Expression(ExpressionStatement),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetStatement {
    pub span: SourceSpan,
    pub name: Identifier,
    pub value: Expression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnStatement {
    pub span: SourceSpan,
    pub value: Expression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarStatement {
    pub span: SourceSpan,
    pub names: Vec<Identifier>,
    pub ty: Option<TypeExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentStatement {
    pub span: SourceSpan,
    pub left: Expression,
    pub right: Expression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionStatement {
    pub span: SourceSpan,
    pub value: Expression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expression {
    Identifier(Identifier),
    Number(NumberLiteral),
    Path(PathExpression),
    Binary(BinaryExpression),
    Postfix(PostfixExpression),
    RecordConstructor(RecordConstructorExpression),
}

impl Expression {
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Identifier(node) => &node.span,
            Self::Number(node) => &node.span,
            Self::Path(node) => &node.span,
            Self::Binary(node) => &node.span,
            Self::Postfix(node) => &node.span,
            Self::RecordConstructor(node) => &node.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberLiteral {
    pub span: SourceSpan,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathExpression {
    pub span: SourceSpan,
    pub ty: Identifier,
    pub member: Identifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryExpression {
    pub span: SourceSpan,
    pub left: Box<Expression>,
    pub operator: BinaryOperator,
    pub right: Box<Expression>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Add,
    Multiply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostfixExpression {
    pub span: SourceSpan,
    pub receiver: Box<Expression>,
    pub operations: Vec<PostfixOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostfixOperation {
    Field(FieldAccess),
    NamedArguments(NamedArgumentList),
    Arguments(ArgumentList),
    Slice(SliceExpression),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldAccess {
    pub span: SourceSpan,
    pub field: Identifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordConstructorExpression {
    pub span: SourceSpan,
    pub constructor: Identifier,
    pub fields: Vec<RecordFieldValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordFieldValue {
    pub span: SourceSpan,
    pub name: Identifier,
    pub value: Expression,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedArgument {
    Sink(SinkArgument),
    Source(SourceArgument),
}

impl NamedArgument {
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Sink(a) => &a.span,
            Self::Source(a) => &a.span,
        }
    }
}

/// A sink connection: `[in] field = expr`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkArgument {
    pub span: SourceSpan,
    pub direction: Option<ConnectionDirection>,
    pub name: Identifier,
    pub value: Expression,
}

/// A source connection: `[out] field => name`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceArgument {
    pub span: SourceSpan,
    pub direction: Option<ConnectionDirection>,
    pub name: Identifier,
    pub target: Identifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedArgumentList {
    pub span: SourceSpan,
    pub arguments: Vec<NamedArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentList {
    pub span: SourceSpan,
    pub arguments: Vec<Expression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExpression {
    pub span: SourceSpan,
    pub name: Identifier,
    pub suffixes: Vec<TypeSuffix>,
    pub domain: Option<Identifier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeSuffix {
    Index(TypeIndex),
    NamedArguments(NamedArgumentList),
    Arguments(TypeArgumentList),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeIndex {
    pub span: SourceSpan,
    pub index: Expression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceExpression {
    pub span: SourceSpan,
    pub start: Expression,
    pub end: Expression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeArgumentList {
    pub span: SourceSpan,
    pub arguments: Vec<TypeExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError {
    pub message: String,
    pub span: Option<SourceSpan>,
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(span) = &self.span {
            write!(
                f,
                "{} at {}:{}",
                self.message,
                span.start.row + 1,
                span.start.column + 1
            )
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for LowerError {}

#[derive(Debug)]
pub enum SurfaceIrError {
    Parse(ParseError),
    Lower(LowerError),
}

impl fmt::Display for SurfaceIrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "{error}"),
            Self::Lower(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SurfaceIrError {}

impl From<ParseError> for SurfaceIrError {
    fn from(value: ParseError) -> Self {
        Self::Parse(value)
    }
}

pub fn parse_surface_source(source: &str) -> Result<SourceFile, SurfaceIrError> {
    let cst = parse_source(source)?;
    lower_cst(&cst, source).map_err(SurfaceIrError::Lower)
}

pub fn parse_surface_file(path: impl AsRef<Path>) -> Result<SourceFile, SurfaceIrError> {
    let source = fs::read_to_string(path.as_ref()).map_err(ParseError::from)?;
    parse_surface_source(&source)
}

pub fn lower_cst(cst: &Cst, source: &str) -> Result<SourceFile, LowerError> {
    expect_kind(&cst.root, "source_file")?;

    let mut items = Vec::new();
    for child in named_children(&cst.root) {
        items.push(lower_item(child, source)?);
    }

    Ok(SourceFile {
        span: cst.root.span.clone(),
        items,
    })
}

fn lower_item(node: &CstNode, source: &str) -> Result<Item, LowerError> {
    match node.kind.as_str() {
        "component_definition" => Ok(Item::Component(lower_component_definition(node, source)?)),
        "struct_definition" => Ok(Item::Struct(lower_struct_definition(node, source)?)),
        "port_definition" => Ok(Item::Port(lower_port_definition(node, source)?)),
        "impl_block" => Ok(Item::Impl(lower_impl_block(node, source)?)),
        _ => Err(unexpected_node(node, "top-level declaration")),
    }
}

fn lower_component_definition(
    node: &CstNode,
    source: &str,
) -> Result<ComponentDefinition, LowerError> {
    expect_kind(node, "component_definition")?;
    Ok(ComponentDefinition {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        named_parameters: lower_named_parameter_section(node, "named_parameters", source)?,
        parameters: lower_parameter_section(node, "parameters", source)?,
        return_type: child_by_field(node, "return_type")
            .map(|child| lower_type_expression(child, source))
            .transpose()?,
        body: lower_required_block(node, "body", source)?,
    })
}

fn lower_struct_definition(node: &CstNode, source: &str) -> Result<StructDefinition, LowerError> {
    expect_kind(node, "struct_definition")?;
    let body = lower_required_child(node, "body", "record_type_body")?;
    Ok(StructDefinition {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        parameters: lower_parameter_section(node, "parameters", source)?,
        constructor: child_by_field(node, "constructor")
            .map(|child| lower_identifier(child, source))
            .transpose()?,
        fields: named_children(body)
            .map(|child| lower_record_field_type(child, source))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn lower_port_definition(node: &CstNode, source: &str) -> Result<PortDefinition, LowerError> {
    expect_kind(node, "port_definition")?;
    let body = lower_required_child(node, "body", "port_body")?;
    Ok(PortDefinition {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        named_parameters: lower_named_parameter_section(node, "named_parameters", source)?,
        parameters: lower_parameter_section(node, "parameters", source)?,
        constructor: child_by_field(node, "constructor")
            .map(|child| lower_identifier(child, source))
            .transpose()?,
        fields: named_children(body)
            .map(|child| lower_port_field(child, source))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn lower_impl_block(node: &CstNode, source: &str) -> Result<ImplBlock, LowerError> {
    expect_kind(node, "impl_block")?;
    let body = lower_required_child(node, "body", "impl_body")?;
    Ok(ImplBlock {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        named_parameters: lower_named_parameter_section(node, "named_parameters", source)?,
        parameters: lower_parameter_section(node, "parameters", source)?,
        functions: named_children(body)
            .map(|child| lower_function_definition(child, source))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn lower_function_definition(
    node: &CstNode,
    source: &str,
) -> Result<FunctionDefinition, LowerError> {
    expect_kind(node, "function_definition")?;
    Ok(FunctionDefinition {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        named_parameters: lower_named_parameter_section(node, "named_parameters", source)?,
        parameters: lower_parameter_section(node, "parameters", source)?,
        return_type: child_by_field(node, "return_type")
            .map(|child| lower_type_expression(child, source))
            .transpose()?,
        body: lower_required_block(node, "body", source)?,
    })
}

fn lower_named_parameter_section(
    node: &CstNode,
    field_name: &str,
    source: &str,
) -> Result<Vec<NamedParameter>, LowerError> {
    let Some(section) = child_by_field(node, field_name) else {
        return Ok(Vec::new());
    };
    expect_kind(section, "named_parameter_section")?;
    named_children(section)
        .map(|child| lower_named_parameter(child, source))
        .collect()
}

fn lower_parameter_section(
    node: &CstNode,
    field_name: &str,
    source: &str,
) -> Result<Vec<Parameter>, LowerError> {
    let Some(section) = child_by_field(node, field_name) else {
        return Ok(Vec::new());
    };
    expect_kind(section, "parameter_section")?;
    named_children(section)
        .map(|child| lower_parameter(child, source))
        .collect()
}

fn lower_named_parameter(node: &CstNode, source: &str) -> Result<NamedParameter, LowerError> {
    expect_kind(node, "named_parameter")?;
    Ok(NamedParameter {
        span: node.span.clone(),
        inferable: child_by_field(node, "inferable").is_some(),
        is_const: child_by_field(node, "const").is_some(),
        name: lower_required_identifier(node, "name", source)?,
        ty: child_by_field(node, "type")
            .map(|child| lower_type_expression(child, source))
            .transpose()?,
        default: child_by_field(node, "default")
            .map(|child| lower_expression(child, source))
            .transpose()?,
    })
}

fn lower_parameter(node: &CstNode, source: &str) -> Result<Parameter, LowerError> {
    expect_kind(node, "parameter")?;
    Ok(Parameter {
        span: node.span.clone(),
        direction: child_by_field(node, "direction")
            .map(lower_direction)
            .transpose()?,
        inferable: child_by_field(node, "inferable").is_some(),
        is_const: child_by_field(node, "const").is_some(),
        name: lower_required_identifier(node, "name", source)?,
        ty: lower_type_expression(
            lower_required_child(node, "type", "type_expression")?,
            source,
        )?,
        default: child_by_field(node, "default")
            .map(|child| lower_expression(child, source))
            .transpose()?,
    })
}

fn lower_direction(node: &CstNode) -> Result<Direction, LowerError> {
    match node.kind.as_str() {
        "in" => Ok(Direction::In),
        "out" => Ok(Direction::Out),
        _ => Err(unexpected_node(node, "direction")),
    }
}

fn lower_record_field_type(node: &CstNode, source: &str) -> Result<RecordFieldType, LowerError> {
    expect_kind(node, "record_field_type")?;
    Ok(RecordFieldType {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        ty: lower_type_expression(
            lower_required_child(node, "type", "type_expression")?,
            source,
        )?,
    })
}

fn lower_port_field(node: &CstNode, source: &str) -> Result<PortField, LowerError> {
    expect_kind(node, "port_field")?;
    Ok(PortField {
        span: node.span.clone(),
        direction: lower_direction(lower_required_field(node, "direction")?)?,
        name: lower_required_identifier(node, "name", source)?,
        ty: lower_type_expression(
            lower_required_child(node, "type", "type_expression")?,
            source,
        )?,
    })
}

fn lower_required_block(
    node: &CstNode,
    field_name: &str,
    source: &str,
) -> Result<Block, LowerError> {
    lower_block(lower_required_child(node, field_name, "block")?, source)
}

fn lower_block(node: &CstNode, source: &str) -> Result<Block, LowerError> {
    expect_kind(node, "block")?;
    Ok(Block {
        span: node.span.clone(),
        statements: named_children(node)
            .map(|child| lower_statement(child, source))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn lower_statement(node: &CstNode, source: &str) -> Result<Statement, LowerError> {
    let node = unwrap_statement(node)?;
    match node.kind.as_str() {
        "let_statement" => Ok(Statement::Let(LetStatement {
            span: node.span.clone(),
            name: lower_required_identifier(node, "name", source)?,
            value: lower_expression(lower_required_field(node, "value")?, source)?,
        })),
        "return_statement" => Ok(Statement::Return(ReturnStatement {
            span: node.span.clone(),
            value: lower_expression(lower_required_field(node, "value")?, source)?,
        })),
        "var_statement" => Ok(Statement::Var(VarStatement {
            span: node.span.clone(),
            names: named_children(node)
                .filter(|child| child.kind == "identifier")
                .map(|child| lower_identifier(child, source))
                .collect::<Result<Vec<_>, _>>()?,
            ty: child_by_field(node, "type")
                .map(|child| lower_type_expression(child, source))
                .transpose()?,
        })),
        "assignment_statement" => Ok(Statement::Assignment(AssignmentStatement {
            span: node.span.clone(),
            left: lower_expression(lower_required_field(node, "left")?, source)?,
            right: lower_expression(lower_required_field(node, "right")?, source)?,
        })),
        "expression_statement" => Ok(Statement::Expression(ExpressionStatement {
            span: node.span.clone(),
            value: lower_expression(
                named_children(node)
                    .next()
                    .ok_or_else(|| missing_child(node, "expression"))?,
                source,
            )?,
        })),
        _ => Err(unexpected_node(node, "statement")),
    }
}

fn lower_expression(node: &CstNode, source: &str) -> Result<Expression, LowerError> {
    let node = unwrap_expression(node)?;
    match node.kind.as_str() {
        "identifier" => Ok(Expression::Identifier(lower_identifier(node, source)?)),
        "number" => Ok(Expression::Number(NumberLiteral {
            span: node.span.clone(),
            text: text(node, source)?.to_owned(),
        })),
        "path_expression" => Ok(Expression::Path(PathExpression {
            span: node.span.clone(),
            ty: lower_required_identifier(node, "type", source)?,
            member: lower_required_identifier(node, "member", source)?,
        })),
        "binary_expression" => Ok(Expression::Binary(BinaryExpression {
            span: node.span.clone(),
            left: Box::new(lower_expression(
                lower_required_field(node, "left")?,
                source,
            )?),
            operator: lower_binary_operator(lower_required_field(node, "operator")?)?,
            right: Box::new(lower_expression(
                lower_required_field(node, "right")?,
                source,
            )?),
        })),
        "postfix_expression" => {
            let receiver = lower_expression(lower_required_field(node, "receiver")?, source)?;
            let mut operations = Vec::new();
            for child in named_children(node) {
                if child.field_name(node).is_some() {
                    continue;
                }
                operations.push(lower_postfix_operation(child, source)?);
            }
            Ok(Expression::Postfix(PostfixExpression {
                span: node.span.clone(),
                receiver: Box::new(receiver),
                operations,
            }))
        }
        "record_constructor_expression" => {
            Ok(Expression::RecordConstructor(RecordConstructorExpression {
                span: node.span.clone(),
                constructor: lower_required_identifier(node, "constructor", source)?,
                fields: lower_record_literal(
                    lower_required_child(node, "body", "record_literal")?,
                    source,
                )?,
            }))
        }
        "parenthesized_expression" => {
            let inner = named_children(node)
                .next()
                .ok_or_else(|| missing_child(node, "expression"))?;
            lower_expression(inner, source)
        }
        _ => Err(unexpected_node(node, "expression")),
    }
}

fn lower_binary_operator(node: &CstNode) -> Result<BinaryOperator, LowerError> {
    match node.kind.as_str() {
        "+" => Ok(BinaryOperator::Add),
        "*" => Ok(BinaryOperator::Multiply),
        _ => Err(unexpected_node(node, "binary operator")),
    }
}

fn lower_postfix_operation(node: &CstNode, source: &str) -> Result<PostfixOperation, LowerError> {
    match node.kind.as_str() {
        "field_access" => Ok(PostfixOperation::Field(FieldAccess {
            span: node.span.clone(),
            field: lower_required_identifier(node, "field", source)?,
        })),
        "named_argument_list" => Ok(PostfixOperation::NamedArguments(NamedArgumentList {
            span: node.span.clone(),
            arguments: lower_named_arguments(node, source)?,
        })),
        "argument_list" => Ok(PostfixOperation::Arguments(ArgumentList {
            span: node.span.clone(),
            arguments: named_children(node)
                .map(|child| lower_expression(child, source))
                .collect::<Result<Vec<_>, _>>()?,
        })),
        "slice_expression" => Ok(PostfixOperation::Slice(SliceExpression {
            span: node.span.clone(),
            start: lower_expression(lower_required_field(node, "start")?, source)?,
            end: lower_expression(lower_required_field(node, "end")?, source)?,
        })),
        _ => Err(unexpected_node(node, "postfix operation")),
    }
}

fn lower_record_literal(node: &CstNode, source: &str) -> Result<Vec<RecordFieldValue>, LowerError> {
    expect_kind(node, "record_literal")?;
    named_children(node)
        .map(|child| lower_record_field_value(child, source))
        .collect()
}

fn lower_record_field_value(node: &CstNode, source: &str) -> Result<RecordFieldValue, LowerError> {
    expect_kind(node, "record_field_value")?;
    Ok(RecordFieldValue {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        value: lower_expression(lower_required_field(node, "value")?, source)?,
    })
}

fn lower_named_arguments(node: &CstNode, source: &str) -> Result<Vec<NamedArgument>, LowerError> {
    named_children(node)
        .map(|child| lower_named_argument(child, source))
        .collect()
}

fn lower_named_argument(node: &CstNode, source: &str) -> Result<NamedArgument, LowerError> {
    expect_kind(node, "named_or_shorthand_argument")?;
    let direction = child_by_field(node, "direction")
        .map(lower_connection_direction)
        .transpose()?;
    let name = lower_required_identifier(node, "name", source)?;

    if let Some(target_node) = child_by_field(node, "target") {
        let target = lower_identifier(target_node, source)?;
        return Ok(NamedArgument::Source(SourceArgument {
            span: node.span.clone(),
            direction,
            name,
            target,
        }));
    }

    let value = if let Some(value) = child_by_field(node, "value") {
        lower_expression(value, source)?
    } else {
        Expression::Identifier(name.clone())
    };
    Ok(NamedArgument::Sink(SinkArgument {
        span: node.span.clone(),
        direction,
        name,
        value,
    }))
}

fn lower_connection_direction(node: &CstNode) -> Result<ConnectionDirection, LowerError> {
    match node.kind.as_str() {
        "in" => Ok(ConnectionDirection::In),
        "out" => Ok(ConnectionDirection::Out),
        _ => Err(unexpected_node(node, "connection direction")),
    }
}

fn lower_type_expression(node: &CstNode, source: &str) -> Result<TypeExpression, LowerError> {
    if node.kind != "type_expression" && node.kind != "return_type_expression" {
        return Err(LowerError {
            message: format!("expected type_expression, found {}", node.kind),
            span: Some(node.span.clone()),
        });
    }
    let mut suffixes = Vec::new();
    let mut domain = None;

    for child in &node.children {
        let child = &child.node;
        match child.kind.as_str() {
            "type_index" => suffixes.push(TypeSuffix::Index(TypeIndex {
                span: child.span.clone(),
                index: lower_expression(lower_required_field(child, "index")?, source)?,
            })),
            "type_named_arguments" => {
                suffixes.push(TypeSuffix::NamedArguments(NamedArgumentList {
                    span: child.span.clone(),
                    arguments: lower_named_arguments(child, source)?,
                }));
            }
            "type_arguments" => {
                suffixes.push(TypeSuffix::Arguments(TypeArgumentList {
                    span: child.span.clone(),
                    arguments: named_children(child)
                        .map(|grandchild| lower_type_expression(grandchild, source))
                        .collect::<Result<Vec<_>, _>>()?,
                }));
            }
            "identifier"
                if child_by_field(node, "domain")
                    .map(|domain_node| domain_node.span == child.span)
                    .unwrap_or(false) =>
            {
                domain = Some(lower_identifier(child, source)?);
            }
            _ => {}
        }
    }

    Ok(TypeExpression {
        span: node.span.clone(),
        name: lower_required_identifier(node, "name", source)?,
        suffixes,
        domain,
    })
}

fn lower_identifier(node: &CstNode, source: &str) -> Result<Identifier, LowerError> {
    expect_kind(node, "identifier")?;
    Ok(Identifier {
        span: node.span.clone(),
        text: text(node, source)?.to_owned(),
    })
}

fn lower_required_identifier(
    node: &CstNode,
    field_name: &str,
    source: &str,
) -> Result<Identifier, LowerError> {
    lower_identifier(lower_required_field(node, field_name)?, source)
}

fn unwrap_statement(node: &CstNode) -> Result<&CstNode, LowerError> {
    if node.kind == "statement" {
        named_children(node)
            .next()
            .ok_or_else(|| missing_child(node, "statement body"))
    } else {
        Ok(node)
    }
}

fn unwrap_expression(node: &CstNode) -> Result<&CstNode, LowerError> {
    if node.kind == "expression" {
        named_children(node)
            .next()
            .ok_or_else(|| missing_child(node, "expression body"))
    } else {
        Ok(node)
    }
}

fn text<'a>(node: &CstNode, source: &'a str) -> Result<&'a str, LowerError> {
    source
        .get(node.span.start_byte..node.span.end_byte)
        .ok_or_else(|| LowerError {
            message: "node span does not align with source text".to_owned(),
            span: Some(node.span.clone()),
        })
}

fn named_children(node: &CstNode) -> impl Iterator<Item = &CstNode> {
    node.children
        .iter()
        .filter(|child| child.node.named)
        .map(|child| &child.node)
}

fn child_by_field<'a>(node: &'a CstNode, field_name: &str) -> Option<&'a CstNode> {
    node.children
        .iter()
        .find(|child| child.field_name.as_deref() == Some(field_name))
        .map(|child| &child.node)
}

fn lower_required_field<'a>(
    node: &'a CstNode,
    field_name: &str,
) -> Result<&'a CstNode, LowerError> {
    child_by_field(node, field_name).ok_or_else(|| missing_child(node, field_name))
}

fn lower_required_child<'a>(
    node: &'a CstNode,
    field_name: &str,
    expected_kind: &str,
) -> Result<&'a CstNode, LowerError> {
    let child = lower_required_field(node, field_name)?;
    expect_kind(child, expected_kind)?;
    Ok(child)
}

fn expect_kind(node: &CstNode, expected: &str) -> Result<(), LowerError> {
    if node.kind == expected {
        Ok(())
    } else {
        Err(LowerError {
            message: format!("expected {expected}, found {}", node.kind),
            span: Some(node.span.clone()),
        })
    }
}

fn missing_child(node: &CstNode, field_name: &str) -> LowerError {
    LowerError {
        message: format!("missing `{field_name}` child"),
        span: Some(node.span.clone()),
    }
}

fn unexpected_node(node: &CstNode, expected: &str) -> LowerError {
    LowerError {
        message: format!("expected {expected}, found {}", node.kind),
        span: Some(node.span.clone()),
    }
}

trait FieldNameLookup {
    fn field_name<'a>(&'a self, parent: &'a CstNode) -> Option<&'a str>;
}

impl FieldNameLookup for CstNode {
    fn field_name<'a>(&'a self, parent: &'a CstNode) -> Option<&'a str> {
        parent
            .children
            .iter()
            .find(|child| std::ptr::eq(&child.node, self))
            .and_then(|child| child.field_name.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_component_example_to_surface_ir() {
        let source = include_str!("../../../examples/mult_add.plr");
        let file = parse_surface_source(source).unwrap();

        assert_eq!(file.items.len(), 1);
        let Item::Component(component) = &file.items[0] else {
            panic!("expected component");
        };
        assert_eq!(component.name.text, "multAdd");
        assert_eq!(component.named_parameters.len(), 3);
        assert_eq!(component.parameters.len(), 2);
        assert_eq!(component.body.statements.len(), 5);
    }

    #[test]
    fn lowers_shorthand_named_arguments_to_explicit_values() {
        let source = include_str!("../../../examples/mult_add.plr");
        let file = parse_surface_source(source).unwrap();
        let Item::Component(component) = &file.items[0] else {
            panic!("expected component");
        };
        let Statement::Let(let_statement) = &component.body.statements[2] else {
            panic!("expected let statement");
        };
        let Expression::Postfix(postfix) = &let_statement.value else {
            panic!("expected postfix expression");
        };
        let PostfixOperation::NamedArguments(args) = &postfix.operations[1] else {
            panic!("expected named arguments");
        };
        assert_eq!(args.arguments.len(), 1);
        let NamedArgument::Sink(arg) = &args.arguments[0] else {
            panic!("expected sink argument");
        };
        assert_eq!(arg.name.text, "rstn");
        let Expression::Identifier(value) = &arg.value else {
            panic!("expected shorthand identifier value");
        };
        assert_eq!(value.text, "rstn");
    }

    #[test]
    fn lowers_parameterized_struct_example() {
        let source = include_str!("../../../examples/parameterized_struct.plr");
        let file = parse_surface_source(source).unwrap();

        assert_eq!(file.items.len(), 2);
        let Item::Struct(structure) = &file.items[0] else {
            panic!("expected struct");
        };
        assert_eq!(structure.name.text, "Bus");
        assert_eq!(structure.parameters.len(), 1);
        assert_eq!(structure.fields.len(), 2);
    }

    #[test]
    fn lowers_parameterized_port_example() {
        let source = include_str!("../../../examples/parameterized_port.plr");
        let file = parse_surface_source(source).unwrap();

        let Item::Port(port) = &file.items[0] else {
            panic!("expected port");
        };
        assert_eq!(port.name.text, "DF");
        assert_eq!(port.named_parameters.len(), 1);
        assert_eq!(port.parameters.len(), 1);
        assert_eq!(port.fields.len(), 3);
    }
}
