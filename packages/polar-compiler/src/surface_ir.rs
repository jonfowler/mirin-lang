use std::path::Path;
use std::{fmt, fs};

use crate::{Cst, CstNode, ParseError, SourceSpan, parse_source};

/// Unique identifier for an AST node within a single `SourceFile`.
///
/// Assigned during lowering. Stable for the lifetime of the `SourceFile` value;
/// not stable across re-parses or across files. Modeled on rustc's `NodeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub span: SourceSpan,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Fn(FunctionDefinition),
    Struct(StructDefinition),
    Port(PortDefinition),
    Impl(ImplBlock),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    pub id: NodeId,
    pub span: SourceSpan,
    pub text: String,
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
    /// `in`/`out` annotation, currently meaningful for port- or struct-typed
    /// named parameters. Treated the same way as the corresponding flag on
    /// `Parameter`.
    pub direction: Option<Direction>,
    pub kind: ParamKind,
    pub name: Identifier,
    pub ty: Option<TypeExpression>,
    pub default: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub span: SourceSpan,
    pub direction: Option<Direction>,
    pub kind: ParamKind,
    pub name: Identifier,
    pub ty: TypeExpression,
    pub default: Option<Expression>,
}

/// What kind of binding a parameter introduces.
///
/// - `Value` — a runtime value, visible in the value environment only.
/// - `Param` — a compile-time parameter (e.g. `param N: usize`). Visible in
///   the type environment as well, so later types/widths can reference it.
/// - `Dom` — a domain (clock) binding (e.g. `dom clk: Clock`). Visible in
///   the type environment for `@clk` annotations.
///
/// Inferability is implicit: a `Param` or `Dom` *named* parameter with no
/// default is inferred from the call-site usage. A positional `Param`/`Dom`
/// (per the syntax design) must always be supplied explicitly. `Value`
/// parameters are never inferable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Value,
    Param,
    Dom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    /// Direction composition under the rule "`in` flips, `out` is identity".
    /// Used during aggregate flattening: descending through an `in` field of
    /// a port reverses the function-body-side direction; descending through
    /// an `out` field preserves it.
    pub fn flip(self) -> Direction {
        match self {
            Direction::In => Direction::Out,
            Direction::Out => Direction::In,
        }
    }
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
    /// Optional trailing expression — the block's value, à la Rust. For fn
    /// bodies, HIR lowering treats this as an implicit `return tail;`.
    /// `None` if every interior item ended with `;`.
    pub tail: Option<Expression>,
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
    pub init: Option<Expression>,
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
    /// A block used as an expression: `{ stmts; ...; tail }`. Value is the
    /// tail expression. HIR keeps this tree-shaped through type-checking; a
    /// late pass flattens it into a result-local plus inlined statements.
    Block(Box<Block>),
    /// `if cond { … } else { … }`. Both branches must produce the same type.
    /// Conditions are syntactically restricted in the grammar so a trailing
    /// `{` can't be parsed as a record-constructor; complex conditions go
    /// in parens.
    If(Box<IfExpression>),
    /// `when EVENT { … }` — Polar's primitive for registered state. The
    /// event slot is typed `Event @D` (typically `clk.posedge()`); the
    /// body is a block-expression whose tail value is the register's
    /// D-input. The expression's value is the held register output, in
    /// the same clock domain D.
    When(Box<WhenExpression>),
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
            Self::Block(node) => &node.span,
            Self::If(node) => &node.span,
            Self::When(node) => &node.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfExpression {
    pub span: SourceSpan,
    pub condition: Box<Expression>,
    pub then_branch: Block,
    pub else_branch: Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhenExpression {
    pub span: SourceSpan,
    pub event: Box<Expression>,
    pub body: Block,
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
    pub arguments: Vec<PositionalArgument>,
}

/// A positional argument at a call site. Either a plain value expression
/// (`f(x + 1)`) or an out-arg binding (`f(out => y)` / `f(=> y)`), which
/// connects a caller-side local to a callee's positional `out`-direction
/// parameter — the positional analogue of `f { name => target }(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PositionalArgument {
    Value(Expression),
    OutBind(OutArgument),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutArgument {
    pub span: SourceSpan,
    pub target: Identifier,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeIndex {
    pub span: SourceSpan,
    pub index: Expression,
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
    Lowerer::new(source).lower_cst(cst)
}

/// State threaded through lowering. Owns the source text and the `NodeId` counter
/// so every constructed `Identifier` gets a unique id without a separate pass.
struct Lowerer<'a> {
    source: &'a str,
    next_id: u32,
}

impl<'a> Lowerer<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, next_id: 0 }
    }

    fn next_node_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    fn lower_cst(&mut self, cst: &Cst) -> Result<SourceFile, LowerError> {
        expect_kind(&cst.root, "source_file")?;

        let mut items = Vec::new();
        for child in named_children(&cst.root) {
            items.push(self.lower_item(child)?);
        }

        Ok(SourceFile {
            span: cst.root.span.clone(),
            items,
        })
    }

    fn lower_item(&mut self, node: &CstNode) -> Result<Item, LowerError> {
        match node.kind.as_str() {
            "function_definition" => Ok(Item::Fn(self.lower_function_definition(node)?)),
            "struct_definition" => Ok(Item::Struct(self.lower_struct_definition(node)?)),
            "port_definition" => Ok(Item::Port(self.lower_port_definition(node)?)),
            "impl_block" => Ok(Item::Impl(self.lower_impl_block(node)?)),
            _ => Err(unexpected_node(node, "top-level declaration")),
        }
    }

    fn lower_struct_definition(&mut self, node: &CstNode) -> Result<StructDefinition, LowerError> {
        expect_kind(node, "struct_definition")?;
        let body = lower_required_child(node, "body", "record_type_body")?;
        Ok(StructDefinition {
            span: node.span.clone(),
            name: self.lower_required_identifier(node, "name")?,
            parameters: self.lower_parameter_section(node, "parameters")?,
            constructor: child_by_field(node, "constructor")
                .map(|child| self.lower_identifier(child))
                .transpose()?,
            fields: named_children(body)
                .map(|child| self.lower_record_field_type(child))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    fn lower_port_definition(&mut self, node: &CstNode) -> Result<PortDefinition, LowerError> {
        expect_kind(node, "port_definition")?;
        let body = lower_required_child(node, "body", "port_body")?;
        Ok(PortDefinition {
            span: node.span.clone(),
            name: self.lower_required_identifier(node, "name")?,
            named_parameters: self.lower_named_parameter_section(node, "named_parameters")?,
            parameters: self.lower_parameter_section(node, "parameters")?,
            constructor: child_by_field(node, "constructor")
                .map(|child| self.lower_identifier(child))
                .transpose()?,
            fields: named_children(body)
                .map(|child| self.lower_port_field(child))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    fn lower_impl_block(&mut self, node: &CstNode) -> Result<ImplBlock, LowerError> {
        expect_kind(node, "impl_block")?;
        let body = lower_required_child(node, "body", "impl_body")?;
        Ok(ImplBlock {
            span: node.span.clone(),
            name: self.lower_required_identifier(node, "name")?,
            named_parameters: self.lower_named_parameter_section(node, "named_parameters")?,
            parameters: self.lower_parameter_section(node, "parameters")?,
            functions: named_children(body)
                .map(|child| self.lower_function_definition(child))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    fn lower_function_definition(
        &mut self,
        node: &CstNode,
    ) -> Result<FunctionDefinition, LowerError> {
        expect_kind(node, "function_definition")?;
        Ok(FunctionDefinition {
            span: node.span.clone(),
            name: self.lower_required_identifier(node, "name")?,
            named_parameters: self.lower_named_parameter_section(node, "named_parameters")?,
            parameters: self.lower_parameter_section(node, "parameters")?,
            return_type: child_by_field(node, "return_type")
                .map(|child| self.lower_type_expression(child))
                .transpose()?,
            body: self.lower_required_block(node, "body")?,
        })
    }

    fn lower_named_parameter_section(
        &mut self,
        node: &CstNode,
        field_name: &str,
    ) -> Result<Vec<NamedParameter>, LowerError> {
        let Some(section) = child_by_field(node, field_name) else {
            return Ok(Vec::new());
        };
        expect_kind(section, "named_parameter_section")?;
        named_children(section)
            .map(|child| self.lower_named_parameter(child))
            .collect()
    }

    fn lower_parameter_section(
        &mut self,
        node: &CstNode,
        field_name: &str,
    ) -> Result<Vec<Parameter>, LowerError> {
        let Some(section) = child_by_field(node, field_name) else {
            return Ok(Vec::new());
        };
        expect_kind(section, "parameter_section")?;
        named_children(section)
            .map(|child| self.lower_parameter(child))
            .collect()
    }

    fn lower_named_parameter(&mut self, node: &CstNode) -> Result<NamedParameter, LowerError> {
        expect_kind(node, "named_parameter")?;
        Ok(NamedParameter {
            span: node.span.clone(),
            direction: child_by_field(node, "direction")
                .map(lower_direction)
                .transpose()?,
            kind: lower_param_kind(node)?,
            name: self.lower_required_identifier(node, "name")?,
            ty: child_by_field(node, "type")
                .map(|child| self.lower_type_expression(child))
                .transpose()?,
            default: child_by_field(node, "default")
                .map(|child| self.lower_expression(child))
                .transpose()?,
        })
    }

    fn lower_parameter(&mut self, node: &CstNode) -> Result<Parameter, LowerError> {
        expect_kind(node, "parameter")?;
        // Self parameter shorthand: `self @clk`. The grammar uses the literal
        // `"self"` for the name field, so the field exists but its child has
        // kind `"self"` (not `"identifier"`). Detect that to synthesise the
        // `Self` type with the given domain.
        let is_self = child_by_field(node, "name").is_some_and(|c| c.kind == "self");
        if is_self {
            let domain = child_by_field(node, "domain")
                .map(|child| self.lower_identifier(child))
                .transpose()?;
            let self_id = Identifier {
                id: self.next_node_id(),
                span: node.span.clone(),
                text: "self".to_owned(),
            };
            let self_type_name = Identifier {
                id: self.next_node_id(),
                span: node.span.clone(),
                text: "Self".to_owned(),
            };
            return Ok(Parameter {
                span: node.span.clone(),
                direction: None,
                kind: ParamKind::Value,
                name: self_id,
                ty: TypeExpression {
                    span: node.span.clone(),
                    name: self_type_name,
                    suffixes: Vec::new(),
                    domain,
                },
                default: None,
            });
        }
        Ok(Parameter {
            span: node.span.clone(),
            direction: child_by_field(node, "direction")
                .map(lower_direction)
                .transpose()?,
            kind: lower_param_kind(node)?,
            name: self.lower_required_identifier(node, "name")?,
            ty: self.lower_type_expression(lower_required_child(
                node,
                "type",
                "type_expression",
            )?)?,
            default: child_by_field(node, "default")
                .map(|child| self.lower_expression(child))
                .transpose()?,
        })
    }

    fn lower_record_field_type(&mut self, node: &CstNode) -> Result<RecordFieldType, LowerError> {
        expect_kind(node, "record_field_type")?;
        Ok(RecordFieldType {
            span: node.span.clone(),
            name: self.lower_required_identifier(node, "name")?,
            ty: self.lower_type_expression(lower_required_child(
                node,
                "type",
                "type_expression",
            )?)?,
        })
    }

    fn lower_port_field(&mut self, node: &CstNode) -> Result<PortField, LowerError> {
        expect_kind(node, "port_field")?;
        Ok(PortField {
            span: node.span.clone(),
            direction: lower_direction(lower_required_field(node, "direction")?)?,
            name: self.lower_required_identifier(node, "name")?,
            ty: self.lower_type_expression(lower_required_child(
                node,
                "type",
                "type_expression",
            )?)?,
        })
    }

    fn lower_required_block(
        &mut self,
        node: &CstNode,
        field_name: &str,
    ) -> Result<Block, LowerError> {
        self.lower_block(lower_required_child(node, field_name, "block")?)
    }

    fn lower_block(&mut self, node: &CstNode) -> Result<Block, LowerError> {
        expect_kind(node, "block")?;
        self.lower_block_like(node)
    }

    /// Shared body for `block` and `block_expression` (which have identical
    /// shape — both produce a `Block` AST node). Callers are responsible for
    /// checking the CST kind matches whichever they expect.
    fn lower_block_like(&mut self, node: &CstNode) -> Result<Block, LowerError> {
        // Grammar puts the optional tail-expression on a "tail" field; every
        // other named child is a statement.
        let mut statements = Vec::new();
        for child in &node.children {
            if !child.node.named || child.node.kind == "comment" {
                continue;
            }
            if child.field_name.as_deref() == Some("tail") {
                continue;
            }
            statements.push(self.lower_statement(&child.node)?);
        }
        let tail = match child_by_field(node, "tail") {
            Some(t) => Some(self.lower_expression(t)?),
            None => None,
        };
        Ok(Block {
            span: node.span.clone(),
            statements,
            tail,
        })
    }

    fn lower_statement(&mut self, node: &CstNode) -> Result<Statement, LowerError> {
        let node = unwrap_statement(node)?;
        match node.kind.as_str() {
            "let_statement" => Ok(Statement::Let(LetStatement {
                span: node.span.clone(),
                name: self.lower_required_identifier(node, "name")?,
                value: self.lower_expression(lower_required_field(node, "value")?)?,
            })),
            "return_statement" => Ok(Statement::Return(ReturnStatement {
                span: node.span.clone(),
                value: self.lower_expression(lower_required_field(node, "value")?)?,
            })),
            "var_statement" => Ok(Statement::Var(VarStatement {
                span: node.span.clone(),
                names: named_children(node)
                    .filter(|child| child.kind == "identifier")
                    .map(|child| self.lower_identifier(child))
                    .collect::<Result<Vec<_>, _>>()?,
                ty: child_by_field(node, "type")
                    .map(|child| self.lower_type_expression(child))
                    .transpose()?,
                init: child_by_field(node, "value")
                    .map(|child| self.lower_expression(child))
                    .transpose()?,
            })),
            "assignment_statement" => Ok(Statement::Assignment(AssignmentStatement {
                span: node.span.clone(),
                left: self.lower_expression(lower_required_field(node, "left")?)?,
                right: self.lower_expression(lower_required_field(node, "right")?)?,
            })),
            "expression_statement" => Ok(Statement::Expression(ExpressionStatement {
                span: node.span.clone(),
                value: self.lower_expression(
                    named_children(node)
                        .next()
                        .ok_or_else(|| missing_child(node, "expression"))?,
                )?,
            })),
            _ => Err(unexpected_node(node, "statement")),
        }
    }

    fn lower_expression(&mut self, node: &CstNode) -> Result<Expression, LowerError> {
        let node = unwrap_expression(node)?;
        match node.kind.as_str() {
            "identifier" => Ok(Expression::Identifier(self.lower_identifier(node)?)),
            "number" => Ok(Expression::Number(NumberLiteral {
                span: node.span.clone(),
                text: text(node, self.source)?.to_owned(),
            })),
            "path_expression" => Ok(Expression::Path(PathExpression {
                span: node.span.clone(),
                ty: self.lower_required_identifier(node, "type")?,
                member: self.lower_required_identifier(node, "member")?,
            })),
            "binary_expression" => Ok(Expression::Binary(BinaryExpression {
                span: node.span.clone(),
                left: Box::new(self.lower_expression(lower_required_field(node, "left")?)?),
                operator: lower_binary_operator(lower_required_field(node, "operator")?)?,
                right: Box::new(self.lower_expression(lower_required_field(node, "right")?)?),
            })),
            "postfix_expression" => {
                let receiver = self.lower_expression(lower_required_field(node, "receiver")?)?;
                let mut operations = Vec::new();
                for child in named_children(node) {
                    if child.field_name(node).is_some() {
                        continue;
                    }
                    operations.push(self.lower_postfix_operation(child)?);
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
                    constructor: self.lower_required_identifier(node, "constructor")?,
                    fields: self.lower_record_literal(lower_required_child(
                        node,
                        "body",
                        "record_literal",
                    )?)?,
                }))
            }
            "parenthesized_expression" => {
                let inner = named_children(node)
                    .next()
                    .ok_or_else(|| missing_child(node, "expression"))?;
                self.lower_expression(inner)
            }
            "block_expression" => {
                // `block_expression` has the same shape as `block`; reuse the
                // block lowering and wrap it in `Expression::Block`.
                let block = self.lower_block_like(node)?;
                Ok(Expression::Block(Box::new(block)))
            }
            "if_expression" => Ok(Expression::If(Box::new(IfExpression {
                span: node.span.clone(),
                condition: Box::new(
                    self.lower_expression(lower_required_field(node, "condition")?)?,
                ),
                then_branch: self.lower_required_block(node, "then_branch")?,
                else_branch: self.lower_required_block(node, "else_branch")?,
            }))),
            "when_expression" => Ok(Expression::When(Box::new(WhenExpression {
                span: node.span.clone(),
                event: Box::new(self.lower_expression(lower_required_field(node, "event")?)?),
                body: self.lower_required_block(node, "body")?,
            }))),
            _ => Err(unexpected_node(node, "expression")),
        }
    }

    fn lower_positional_argument(
        &mut self,
        node: &CstNode,
    ) -> Result<PositionalArgument, LowerError> {
        match node.kind.as_str() {
            "out_argument" => Ok(PositionalArgument::OutBind(OutArgument {
                span: node.span.clone(),
                target: self.lower_required_identifier(node, "target")?,
            })),
            _ => Ok(PositionalArgument::Value(self.lower_expression(node)?)),
        }
    }

    fn lower_postfix_operation(&mut self, node: &CstNode) -> Result<PostfixOperation, LowerError> {
        match node.kind.as_str() {
            "field_access" => Ok(PostfixOperation::Field(FieldAccess {
                span: node.span.clone(),
                field: self.lower_required_identifier(node, "field")?,
            })),
            "named_argument_list" => Ok(PostfixOperation::NamedArguments(NamedArgumentList {
                span: node.span.clone(),
                arguments: self.lower_named_arguments(node)?,
            })),
            "argument_list" => Ok(PostfixOperation::Arguments(ArgumentList {
                span: node.span.clone(),
                arguments: named_children(node)
                    .map(|child| self.lower_positional_argument(child))
                    .collect::<Result<Vec<_>, _>>()?,
            })),
            _ => Err(unexpected_node(node, "postfix operation")),
        }
    }

    fn lower_record_literal(
        &mut self,
        node: &CstNode,
    ) -> Result<Vec<RecordFieldValue>, LowerError> {
        expect_kind(node, "record_literal")?;
        named_children(node)
            .map(|child| self.lower_record_field_value(child))
            .collect()
    }

    fn lower_record_field_value(&mut self, node: &CstNode) -> Result<RecordFieldValue, LowerError> {
        expect_kind(node, "record_field_value")?;
        Ok(RecordFieldValue {
            span: node.span.clone(),
            name: self.lower_required_identifier(node, "name")?,
            value: self.lower_expression(lower_required_field(node, "value")?)?,
        })
    }

    fn lower_named_arguments(&mut self, node: &CstNode) -> Result<Vec<NamedArgument>, LowerError> {
        named_children(node)
            .map(|child| self.lower_named_argument(child))
            .collect()
    }

    fn lower_named_argument(&mut self, node: &CstNode) -> Result<NamedArgument, LowerError> {
        expect_kind(node, "named_or_shorthand_argument")?;
        let direction = child_by_field(node, "direction")
            .map(lower_connection_direction)
            .transpose()?;
        let name = self.lower_required_identifier(node, "name")?;

        if let Some(target_node) = child_by_field(node, "target") {
            let target = self.lower_identifier(target_node)?;
            return Ok(NamedArgument::Source(SourceArgument {
                span: node.span.clone(),
                direction,
                name,
                target,
            }));
        }

        let value = if let Some(value) = child_by_field(node, "value") {
            self.lower_expression(value)?
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

    fn lower_type_expression(&mut self, node: &CstNode) -> Result<TypeExpression, LowerError> {
        expect_kind(node, "type_expression")?;
        let mut suffixes = Vec::new();
        let mut domain = None;

        for child in &node.children {
            let child = &child.node;
            match child.kind.as_str() {
                "type_index" => suffixes.push(TypeSuffix::Index(TypeIndex {
                    span: child.span.clone(),
                    index: self.lower_expression(lower_required_field(child, "index")?)?,
                })),
                "identifier"
                    if child_by_field(node, "domain")
                        .map(|domain_node| domain_node.span == child.span)
                        .unwrap_or(false) =>
                {
                    domain = Some(self.lower_identifier(child)?);
                }
                _ => {}
            }
        }

        Ok(TypeExpression {
            span: node.span.clone(),
            name: self.lower_required_identifier(node, "name")?,
            suffixes,
            domain,
        })
    }

    fn lower_identifier(&mut self, node: &CstNode) -> Result<Identifier, LowerError> {
        expect_kind(node, "identifier")?;
        Ok(Identifier {
            id: self.next_node_id(),
            span: node.span.clone(),
            text: text(node, self.source)?.to_owned(),
        })
    }

    fn lower_required_identifier(
        &mut self,
        node: &CstNode,
        field_name: &str,
    ) -> Result<Identifier, LowerError> {
        self.lower_identifier(lower_required_field(node, field_name)?)
    }
}

fn lower_direction(node: &CstNode) -> Result<Direction, LowerError> {
    match node.kind.as_str() {
        "in" => Ok(Direction::In),
        "out" => Ok(Direction::Out),
        _ => Err(unexpected_node(node, "direction")),
    }
}

/// Read the optional `kind` field on a parameter CST node. Returns `Value`
/// when absent (an ordinary value binding) and the matching `Param` / `Dom`
/// when the keyword is present.
fn lower_param_kind(node: &CstNode) -> Result<ParamKind, LowerError> {
    let Some(kind_node) = child_by_field(node, "kind") else {
        return Ok(ParamKind::Value);
    };
    match kind_node.kind.as_str() {
        "param" => Ok(ParamKind::Param),
        "dom" => Ok(ParamKind::Dom),
        _ => Err(unexpected_node(kind_node, "param kind")),
    }
}

fn lower_binary_operator(node: &CstNode) -> Result<BinaryOperator, LowerError> {
    match node.kind.as_str() {
        "+" => Ok(BinaryOperator::Add),
        "*" => Ok(BinaryOperator::Multiply),
        _ => Err(unexpected_node(node, "binary operator")),
    }
}

fn lower_connection_direction(node: &CstNode) -> Result<ConnectionDirection, LowerError> {
    match node.kind.as_str() {
        "in" => Ok(ConnectionDirection::In),
        "out" => Ok(ConnectionDirection::Out),
        _ => Err(unexpected_node(node, "connection direction")),
    }
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
        .filter(|child| child.node.named && child.node.kind != "comment")
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
        let source = include_str!("../../../examples/working/mult_add.plr");
        let file = parse_surface_source(source).unwrap();

        assert_eq!(file.items.len(), 1);
        let Item::Fn(component) = &file.items[0] else {
            panic!("expected component");
        };
        assert_eq!(component.name.text, "multAdd");
        assert_eq!(component.named_parameters.len(), 3);
        assert_eq!(component.parameters.len(), 2);
        assert_eq!(component.body.statements.len(), 4);
    }

    #[test]
    fn lowers_reg_call_positional_arguments() {
        let source = include_str!("../../../examples/working/mult_add.plr");
        let file = parse_surface_source(source).unwrap();
        let Item::Fn(component) = &file.items[0] else {
            panic!("expected fn");
        };
        // statements[1] is `let mult = mult.reg(rstn, 0);`
        let Statement::Let(let_stmt) = &component.body.statements[1] else {
            panic!("expected let statement");
        };
        let Expression::Postfix(postfix) = &let_stmt.value else {
            panic!("expected postfix expression");
        };
        // .reg  →  field_access
        let PostfixOperation::Field(field) = &postfix.operations[0] else {
            panic!("expected field access");
        };
        assert_eq!(field.field.text, "reg");
        // (rstn, 0)  →  argument_list with two positional args
        let PostfixOperation::Arguments(args) = &postfix.operations[1] else {
            panic!("expected argument list");
        };
        assert_eq!(args.arguments.len(), 2);
        let PositionalArgument::Value(Expression::Identifier(rst)) = &args.arguments[0] else {
            panic!("expected identifier for rst");
        };
        assert_eq!(rst.text, "rstn");
        let PositionalArgument::Value(Expression::Number(reset_val)) = &args.arguments[1] else {
            panic!("expected number for reset_val");
        };
        assert_eq!(reset_val.text, "0");
    }
}
