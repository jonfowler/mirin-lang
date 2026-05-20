use std::{fmt, fs, path::Path};

use tree_sitter::{Language, Parser, Point};
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_polar() -> *const ();
}

const LANGUAGE_FN: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_polar) };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    pub row: usize,
    pub column: usize,
}

impl From<Point> for SourcePosition {
    fn from(value: Point) -> Self {
        Self {
            row: value.row,
            column: value.column,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start: SourcePosition,
    pub end: SourcePosition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CstChild {
    pub field_name: Option<String>,
    pub node: CstNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CstNode {
    pub kind: String,
    pub named: bool,
    pub extra: bool,
    pub span: SourceSpan,
    pub children: Vec<CstChild>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cst {
    pub root: CstNode,
}

#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    Language(tree_sitter::LanguageError),
    ParseFailed,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "failed to read source: {err}"),
            Self::Language(err) => write!(f, "failed to configure parser language: {err}"),
            Self::ParseFailed => f.write_str("tree-sitter returned no parse tree"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<std::io::Error> for ParseError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<tree_sitter::LanguageError> for ParseError {
    fn from(value: tree_sitter::LanguageError) -> Self {
        Self::Language(value)
    }
}

pub fn language() -> Language {
    Language::new(LANGUAGE_FN)
}

pub fn parse_source(source: &str) -> Result<Cst, ParseError> {
    let mut parser = Parser::new();
    parser.set_language(&language())?;
    let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
    Ok(Cst {
        root: CstNode::from_node(tree.root_node()),
    })
}

pub fn parse_file(path: impl AsRef<Path>) -> Result<Cst, ParseError> {
    let source = fs::read_to_string(path)?;
    parse_source(&source)
}

impl SourceSpan {
    fn from_node(node: tree_sitter::Node<'_>) -> Self {
        Self {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start: node.start_position().into(),
            end: node.end_position().into(),
        }
    }
}

impl CstNode {
    fn from_node(node: tree_sitter::Node<'_>) -> Self {
        let mut children = Vec::new();
        for index in 0..node.child_count() {
            if let Some(child) = node.child(index) {
                children.push(CstChild {
                    field_name: node.field_name_for_child(index as u32).map(str::to_owned),
                    node: CstNode::from_node(child),
                });
            }
        }

        Self {
            kind: node.kind().to_owned(),
            named: node.is_named(),
            extra: node.is_extra(),
            span: SourceSpan::from_node(node),
            children,
        }
    }

    fn fmt_with_indent(
        &self,
        f: &mut fmt::Formatter<'_>,
        indent: usize,
        field_name: Option<&str>,
    ) -> fmt::Result {
        for _ in 0..indent {
            f.write_str("  ")?;
        }

        if let Some(field_name) = field_name {
            write!(f, "{field_name}: ")?;
        }

        writeln!(
            f,
            "{} [{}:{}-{}:{} | bytes {}..{}{}{}]",
            self.kind,
            self.span.start.row,
            self.span.start.column,
            self.span.end.row,
            self.span.end.column,
            self.span.start_byte,
            self.span.end_byte,
            if self.named { ", named" } else { "" },
            if self.extra { ", extra" } else { "" },
        )?;

        for child in &self.children {
            child.node
                .fmt_with_indent(f, indent + 1, child.field_name.as_deref())?;
        }

        Ok(())
    }
}

impl fmt::Display for Cst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.root.fmt_with_indent(f, 0, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_component_examples_into_cst() {
        for source in [
            include_str!("../../../examples/add_constant.plr"),
            include_str!("../../../examples/mult_add.plr"),
            include_str!("../../../examples/counter.plr"),
            include_str!("../../../examples/shift_register.plr"),
        ] {
            let cst = parse_source(source).unwrap();
            assert_eq!(cst.root.kind, "source_file");
            assert!(
                cst.root
                    .children
                    .iter()
                    .any(|child| child.node.kind == "component_definition")
            );
        }
    }

    #[test]
    fn parses_parameterized_struct_and_port_examples() {
        for source in [
            include_str!("../../../examples/parameterized_struct.plr"),
            include_str!("../../../examples/parameterized_port.plr"),
        ] {
            let cst = parse_source(source).unwrap();
            assert_eq!(cst.root.kind, "source_file");
            assert!(!cst.root.children.is_empty());
        }
    }

    #[test]
    fn root_span_covers_full_source() {
        let source = include_str!("../../../examples/add_constant.plr");
        let cst = parse_source(source).unwrap();
        assert_eq!(cst.root.span.start_byte, 0);
        assert_eq!(cst.root.span.end_byte, source.len());
    }
}
