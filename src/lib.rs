use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub named_params: Vec<Parameter>,
    pub positional_params: Vec<Parameter>,
    pub return_type: Option<TypeRef>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub inferable: bool,
    pub is_const: bool,
    pub name: String,
    pub ty: TypeRef,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    pub name: String,
    pub width: Option<String>,
    pub domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub statements: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Let { name: String, value: Expr },
    Return(Expr),
    Rec { name: String, body: Block },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Ident(String),
    Integer(String),
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Slice {
        target: Box<Expr>,
        start: Box<Expr>,
        end: Box<Expr>,
    },
    Field {
        target: Box<Expr>,
        field: String,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        named_args: Vec<NamedArg>,
        positional_args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Multiply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedArg {
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    message: String,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse_component(source: &str) -> Result<Component, ParseError> {
    let tokens = Lexer::new(source).lex()?;
    let mut parser = Parser::new(tokens);
    let component = parser.parse_component()?;
    parser.expect(TokenKind::Eof)?;
    Ok(component)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Ident,
    Number,
    Cmp,
    Let,
    Return,
    Rec,
    Const,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Semicolon,
    Assign,
    At,
    Hash,
    Dot,
    Plus,
    Star,
    Arrow,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    text: String,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().peekable(),
        }
    }

    fn lex(mut self) -> Result<Vec<Token>, ParseError> {
        let mut tokens = Vec::new();
        while let Some(&ch) = self.chars.peek() {
            match ch {
                c if c.is_whitespace() => {
                    self.chars.next();
                }
                '/' => {
                    self.chars.next();
                    match self.chars.peek() {
                        Some('/') => {
                            for next in self.chars.by_ref() {
                                if next == '\n' {
                                    break;
                                }
                            }
                        }
                        _ => {
                            return Err(ParseError::new("unexpected '/'"));
                        }
                    }
                }
                '{' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::LBrace, "{"));
                }
                '}' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::RBrace, "}"));
                }
                '(' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::LParen, "("));
                }
                ')' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::RParen, ")"));
                }
                '[' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::LBracket, "["));
                }
                ']' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::RBracket, "]"));
                }
                ':' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Colon, ":"));
                }
                ',' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Comma, ","));
                }
                ';' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Semicolon, ";"));
                }
                '=' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Assign, "="));
                }
                '@' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::At, "@"));
                }
                '#' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Hash, "#"));
                }
                '.' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Dot, "."));
                }
                '+' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Plus, "+"));
                }
                '*' => {
                    self.chars.next();
                    tokens.push(Token::simple(TokenKind::Star, "*"));
                }
                '-' => {
                    self.chars.next();
                    if self.chars.peek() == Some(&'>') {
                        self.chars.next();
                        tokens.push(Token::simple(TokenKind::Arrow, "->"));
                    } else {
                        return Err(ParseError::new("unexpected '-'"));
                    }
                }
                c if c.is_ascii_digit() => tokens.push(self.lex_number()),
                c if is_ident_start(c) => tokens.push(self.lex_ident()),
                _ => {
                    return Err(ParseError::new(format!("unexpected character '{ch}'")));
                }
            }
        }
        tokens.push(Token::simple(TokenKind::Eof, ""));
        Ok(tokens)
    }

    fn lex_number(&mut self) -> Token {
        let mut text = String::new();
        while let Some(&ch) = self.chars.peek() {
            if ch.is_ascii_digit() {
                text.push(ch);
                self.chars.next();
            } else {
                break;
            }
        }
        Token {
            kind: TokenKind::Number,
            text,
        }
    }

    fn lex_ident(&mut self) -> Token {
        let mut text = String::new();
        while let Some(&ch) = self.chars.peek() {
            if is_ident_continue(ch) {
                text.push(ch);
                self.chars.next();
            } else {
                break;
            }
        }
        let kind = match text.as_str() {
            "cmp" => TokenKind::Cmp,
            "let" => TokenKind::Let,
            "return" => TokenKind::Return,
            "rec" => TokenKind::Rec,
            "const" => TokenKind::Const,
            _ => TokenKind::Ident,
        };
        Token { kind, text }
    }
}

impl Token {
    fn simple(kind: TokenKind, text: &str) -> Self {
        Self {
            kind,
            text: text.to_owned(),
        }
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, index: 0 }
    }

    fn parse_component(&mut self) -> Result<Component, ParseError> {
        self.expect(TokenKind::Cmp)?;
        let name = self.expect_ident()?;
        let named_params = if self.peek_kind() == TokenKind::LBrace {
            self.parse_named_params()?
        } else {
            Vec::new()
        };
        let positional_params = if self.peek_kind() == TokenKind::LParen {
            self.parse_positional_params()?
        } else {
            Vec::new()
        };
        let return_type = if self.peek_kind() == TokenKind::Arrow {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        Ok(Component {
            name,
            named_params,
            positional_params,
            return_type,
            body,
        })
    }

    fn parse_named_params(&mut self) -> Result<Vec<Parameter>, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut params = Vec::new();
        while self.peek_kind() != TokenKind::RBrace {
            params.push(self.parse_parameter(true)?);
            if self.peek_kind() == TokenKind::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(params)
    }

    fn parse_positional_params(&mut self) -> Result<Vec<Parameter>, ParseError> {
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        while self.peek_kind() != TokenKind::RParen {
            params.push(self.parse_parameter(false)?);
            if self.peek_kind() == TokenKind::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(params)
    }

    fn parse_parameter(&mut self, allow_inferable: bool) -> Result<Parameter, ParseError> {
        let inferable = if allow_inferable && self.peek_kind() == TokenKind::Hash {
            self.bump();
            true
        } else {
            false
        };
        let is_const = if self.peek_kind() == TokenKind::Const {
            self.bump();
            true
        } else {
            false
        };
        let name = self.expect_ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.parse_type()?;
        let default = if self.peek_kind() == TokenKind::Assign {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Parameter {
            inferable,
            is_const,
            name,
            ty,
            default,
        })
    }

    fn parse_type(&mut self) -> Result<TypeRef, ParseError> {
        let name = self.expect_ident()?;
        let width = if self.peek_kind() == TokenKind::LBracket {
            Some(self.parse_bracket_payload()?)
        } else {
            None
        };
        let domain = if self.peek_kind() == TokenKind::At {
            self.bump();
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(TypeRef {
            name,
            width,
            domain,
        })
    }

    fn parse_bracket_payload(&mut self) -> Result<String, ParseError> {
        self.expect(TokenKind::LBracket)?;
        let mut parts = Vec::new();
        while self.peek_kind() != TokenKind::RBracket {
            let token = self.bump();
            parts.push(token.text);
        }
        self.expect(TokenKind::RBracket)?;
        Ok(parts.join(" "))
    }

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut statements = Vec::new();
        while self.peek_kind() != TokenKind::RBrace {
            statements.push(self.parse_stmt()?);
        }
        self.expect(TokenKind::RBrace)?;
        Ok(Block { statements })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek_kind() {
            TokenKind::Let => {
                self.bump();
                let name = self.expect_ident()?;
                self.expect(TokenKind::Assign)?;
                let value = self.parse_expr()?;
                self.expect(TokenKind::Semicolon)?;
                Ok(Stmt::Let { name, value })
            }
            TokenKind::Return => {
                self.bump();
                let value = self.parse_expr()?;
                self.expect(TokenKind::Semicolon)?;
                Ok(Stmt::Return(value))
            }
            TokenKind::Rec => {
                self.bump();
                let name = self.expect_ident()?;
                self.expect(TokenKind::Assign)?;
                let body = self.parse_block()?;
                Ok(Stmt::Rec { name, body })
            }
            _ => Err(self.error_here("expected statement")),
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_binary_expr(0)
    }

    fn parse_binary_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_postfix_expr()?;
        loop {
            let (op, left_bp, right_bp) = match self.peek_kind() {
                TokenKind::Plus => (BinaryOp::Add, 1, 2),
                TokenKind::Star => (BinaryOp::Multiply, 3, 4),
                _ => break,
            };
            if left_bp < min_bp {
                break;
            }
            self.bump();
            let rhs = self.parse_binary_expr(right_bp)?;
            lhs = Expr::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_postfix_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary_expr()?;
        loop {
            match self.peek_kind() {
                TokenKind::LBracket => {
                    self.bump();
                    let start = self.parse_expr()?;
                    self.expect(TokenKind::Colon)?;
                    let end = self.parse_expr()?;
                    self.expect(TokenKind::RBracket)?;
                    expr = Expr::Slice {
                        target: Box::new(expr),
                        start: Box::new(start),
                        end: Box::new(end),
                    };
                }
                TokenKind::Dot => {
                    self.bump();
                    let field_or_method = self.expect_ident()?;
                    if self.peek_kind() == TokenKind::LBrace
                        || self.peek_kind() == TokenKind::LParen
                    {
                        let named_args = if self.peek_kind() == TokenKind::LBrace {
                            self.parse_named_call_args()?
                        } else {
                            Vec::new()
                        };
                        let positional_args = self.parse_call_args()?;
                        expr = Expr::MethodCall {
                            receiver: Box::new(expr),
                            method: field_or_method,
                            named_args,
                            positional_args,
                        };
                    } else {
                        expr = Expr::Field {
                            target: Box::new(expr),
                            field: field_or_method,
                        };
                    }
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_named_call_args(&mut self) -> Result<Vec<NamedArg>, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut args = Vec::new();
        while self.peek_kind() != TokenKind::RBrace {
            let name = self.expect_ident()?;
            let value = if self.peek_kind() == TokenKind::Assign {
                self.bump();
                self.parse_expr()?
            } else {
                Expr::Ident(name.clone())
            };
            args.push(NamedArg { name, value });
            if self.peek_kind() == TokenKind::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(args)
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        while self.peek_kind() != TokenKind::RParen {
            args.push(self.parse_expr()?);
            if self.peek_kind() == TokenKind::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(args)
    }

    fn parse_primary_expr(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            TokenKind::Ident => Ok(Expr::Ident(self.bump().text)),
            TokenKind::Number => Ok(Expr::Integer(self.bump().text)),
            TokenKind::LParen => {
                self.bump();
                let expr = self.parse_expr()?;
                self.expect(TokenKind::RParen)?;
                Ok(expr)
            }
            _ => Err(self.error_here("expected expression")),
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<(), ParseError> {
        if self.peek_kind() == kind {
            self.bump();
            Ok(())
        } else {
            Err(self.error_here(format!("expected {:?}", kind)))
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        if self.peek_kind() == TokenKind::Ident {
            Ok(self.bump().text)
        } else {
            Err(self.error_here("expected identifier"))
        }
    }

    fn peek_kind(&self) -> TokenKind {
        self.tokens[self.index].kind.clone()
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.index].clone();
        self.index += 1;
        token
    }

    fn error_here(&self, message: impl Into<String>) -> ParseError {
        let token = &self.tokens[self.index];
        ParseError::new(format!("{} near '{}'", message.into(), token.text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_constant_example() {
        let component = parse_component(include_str!("../examples/add_constant.plr")).unwrap();
        assert_eq!(component.name, "addConstant");
        assert_eq!(component.named_params.len(), 1);
        assert_eq!(component.positional_params.len(), 1);
        assert_eq!(component.body.statements.len(), 2);
    }

    #[test]
    fn parses_mult_add_example() {
        let component = parse_component(include_str!("../examples/mult_add.plr")).unwrap();
        assert_eq!(component.name, "multAdd");
        assert_eq!(component.named_params.len(), 3);
        assert_eq!(component.positional_params.len(), 2);
        assert!(matches!(
            component.body.statements[1],
            Stmt::Let {
                value: Expr::Slice { .. },
                ..
            }
        ));
        assert!(matches!(
            component.body.statements[2],
            Stmt::Let {
                value: Expr::MethodCall { .. },
                ..
            }
        ));
    }

    #[test]
    fn parses_shift_register_example() {
        let component = parse_component(include_str!("../examples/shift_register.plr")).unwrap();
        assert_eq!(component.name, "shiftRegister");
        assert_eq!(component.body.statements.len(), 3);
    }

    #[test]
    fn parses_counter_rec_block() {
        let component = parse_component(include_str!("../examples/counter.plr")).unwrap();
        assert_eq!(component.name, "counter");
        assert!(matches!(component.body.statements[0], Stmt::Rec { .. }));
    }
}
