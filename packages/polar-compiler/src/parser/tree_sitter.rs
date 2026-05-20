use std::{fmt, fs, path::Path};

use tree_sitter::{Language, Parser, Point, Tree};
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
pub struct SourceExcerpt {
    pub line_number: usize,
    pub line_text: String,
    pub highlight_start: usize,
    pub highlight_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxDiagnostic {
    pub message: String,
    pub span: SourceSpan,
    pub excerpt: Option<SourceExcerpt>,
    pub note: Option<String>,
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
    pub missing: bool,
    pub error: bool,
    pub span: SourceSpan,
    pub children: Vec<CstChild>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cst {
    pub root: CstNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSource {
    pub cst: Cst,
    pub diagnostics: Vec<SyntaxDiagnostic>,
}

#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    Language(tree_sitter::LanguageError),
    ParseFailed,
    Syntax(Vec<SyntaxDiagnostic>),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "failed to read source: {err}"),
            Self::Language(err) => write!(f, "failed to configure parser language: {err}"),
            Self::ParseFailed => f.write_str("tree-sitter returned no parse tree"),
            Self::Syntax(diagnostics) => {
                write!(f, "found {} syntax error(s)", diagnostics.len())
            }
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
    let parsed = parse_source_with_diagnostics(source)?;
    if !parsed.diagnostics.is_empty() {
        return Err(ParseError::Syntax(parsed.diagnostics));
    }

    Ok(parsed.cst)
}

pub fn parse_source_with_diagnostics(source: &str) -> Result<ParsedSource, ParseError> {
    let tree = parse_tree(source)?;
    Ok(ParsedSource {
        cst: Cst {
            root: CstNode::from_node(tree.root_node()),
        },
        diagnostics: collect_syntax_diagnostics(tree.root_node(), source),
    })
}

pub fn parse_file(path: impl AsRef<Path>) -> Result<Cst, ParseError> {
    let source = fs::read_to_string(path)?;
    parse_source(&source)
}

pub fn parse_file_with_diagnostics(path: impl AsRef<Path>) -> Result<ParsedSource, ParseError> {
    let source = fs::read_to_string(path)?;
    parse_source_with_diagnostics(&source)
}

pub fn render_parse_error(
    error: &ParseError,
    path: Option<&Path>,
    f: &mut impl fmt::Write,
) -> fmt::Result {
    match error {
        ParseError::Syntax(diagnostics) => {
            for (index, diagnostic) in diagnostics.iter().enumerate() {
                if index > 0 {
                    writeln!(f)?;
                }

                writeln!(f, "error: {}", diagnostic.message)?;
                if let Some(path) = path {
                    writeln!(
                        f,
                        " --> {}:{}:{}",
                        path.display(),
                        diagnostic.span.start.row + 1,
                        diagnostic.span.start.column + 1
                    )?;
                } else {
                    writeln!(
                        f,
                        " --> {}:{}",
                        diagnostic.span.start.row + 1,
                        diagnostic.span.start.column + 1
                    )?;
                }

                if let Some(excerpt) = &diagnostic.excerpt {
                    writeln!(f, "  |")?;
                    writeln!(f, "{:>2} | {}", excerpt.line_number, excerpt.line_text)?;
                    writeln!(
                        f,
                        "  | {}{}",
                        " ".repeat(excerpt.highlight_start),
                        "^".repeat(
                            excerpt
                                .highlight_end
                                .saturating_sub(excerpt.highlight_start)
                        ),
                    )?;
                }

                if let Some(note) = &diagnostic.note {
                    writeln!(f, "  = note: {note}")?;
                }
            }
            Ok(())
        }
        other => write!(f, "{other}"),
    }
}

fn parse_tree(source: &str) -> Result<Tree, ParseError> {
    let mut parser = Parser::new();
    parser.set_language(&language())?;
    parser.parse(source, None).ok_or(ParseError::ParseFailed)
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
            missing: node.is_missing(),
            error: node.is_error(),
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
            "{} [{}:{}-{}:{} | bytes {}..{}{}{}{}{}]",
            self.kind,
            self.span.start.row,
            self.span.start.column,
            self.span.end.row,
            self.span.end.column,
            self.span.start_byte,
            self.span.end_byte,
            if self.named { ", named" } else { "" },
            if self.extra { ", extra" } else { "" },
            if self.missing { ", missing" } else { "" },
            if self.error { ", error" } else { "" },
        )?;

        for child in &self.children {
            child
                .node
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

fn collect_syntax_diagnostics(root: tree_sitter::Node<'_>, source: &str) -> Vec<SyntaxDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut ancestors = Vec::new();
    walk_syntax_errors(root, source, &mut ancestors, &mut diagnostics);
    deduplicate_syntax_diagnostics(diagnostics)
}

fn walk_syntax_errors(
    node: tree_sitter::Node<'_>,
    source: &str,
    ancestors: &mut Vec<String>,
    diagnostics: &mut Vec<SyntaxDiagnostic>,
) {
    if node.is_error() {
        diagnostics.push(error_diagnostic(node, source, ancestors));
        return;
    }

    if node.is_missing() {
        diagnostics.push(missing_diagnostic(node, source, ancestors));
    }

    ancestors.push(node.kind().to_owned());
    for index in 0..node.child_count() {
        if let Some(child) = node.child(index) {
            walk_syntax_errors(child, source, ancestors, diagnostics);
        }
    }
    ancestors.pop();
}

fn error_diagnostic(
    node: tree_sitter::Node<'_>,
    source: &str,
    ancestors: &[String],
) -> SyntaxDiagnostic {
    let span = span_for_error_node(node);
    let excerpt = excerpt_for_span(source, &span);
    let context = context_from_ancestors(ancestors);
    let unexpected = node_text(node, source)
        .filter(|text| !text.trim().is_empty())
        .map(|text| compact_snippet(&text));

    let message = match unexpected {
        Some(text) => format!("unexpected `{text}` while parsing {context}"),
        None => format!("unexpected end of input while parsing {context}"),
    };

    SyntaxDiagnostic {
        message,
        span,
        excerpt,
        note: note_from_ancestors(ancestors),
    }
}

fn missing_diagnostic(
    node: tree_sitter::Node<'_>,
    source: &str,
    ancestors: &[String],
) -> SyntaxDiagnostic {
    let span = SourceSpan::from_node(node);
    let kind = node.kind().trim_matches('"');
    SyntaxDiagnostic {
        message: format!(
            "expected `{kind}` while parsing {}",
            context_from_ancestors(ancestors)
        ),
        span: span.clone(),
        excerpt: excerpt_for_span(source, &span),
        note: note_from_ancestors(ancestors),
    }
}

fn deduplicate_syntax_diagnostics(diagnostics: Vec<SyntaxDiagnostic>) -> Vec<SyntaxDiagnostic> {
    let mut deduplicated = Vec::new();
    let mut pending = diagnostics.into_iter().peekable();

    while let Some(current) = pending.next() {
        if let Some(next) = pending.peek()
            && should_prefer_missing_diagnostic(&current, next)
        {
            deduplicated.push(pending.next().expect("peeked diagnostic must exist"));
            continue;
        }

        deduplicated.push(current);
    }

    deduplicated
}

fn should_prefer_missing_diagnostic(current: &SyntaxDiagnostic, next: &SyntaxDiagnostic) -> bool {
    current.message.starts_with("unexpected ")
        && next.message.starts_with("expected ")
        && current.note == next.note
        && current.span.start.row == next.span.start.row
        && current.span.end_byte <= next.span.start_byte
}

fn span_for_error_node(node: tree_sitter::Node<'_>) -> SourceSpan {
    let span = SourceSpan::from_node(node);
    if span.start_byte != span.end_byte {
        return span;
    }

    SourceSpan {
        start_byte: span.start_byte,
        end_byte: span.end_byte + 1,
        start: span.start,
        end: SourcePosition {
            row: span.end.row,
            column: span.end.column + 1,
        },
    }
}

fn excerpt_for_span(source: &str, span: &SourceSpan) -> Option<SourceExcerpt> {
    let line_text = source.lines().nth(span.start.row)?.to_owned();
    let start = span.start.column.min(line_text.len());
    let end = if span.start.row == span.end.row {
        span.end
            .column
            .max(start + 1)
            .min(line_text.len().max(start + 1))
    } else {
        line_text.len().max(start + 1)
    };

    Some(SourceExcerpt {
        line_number: span.start.row + 1,
        line_text,
        highlight_start: start,
        highlight_end: end,
    })
}

fn node_text(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes()).ok().map(str::to_owned)
}

fn compact_snippet(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= 32 {
        compact
    } else {
        format!("{}…", &compact[..32])
    }
}

fn context_from_ancestors(ancestors: &[String]) -> &'static str {
    if ancestors
        .iter()
        .any(|kind| kind == "named_parameter_section")
    {
        "named parameter section"
    } else if ancestors.iter().any(|kind| kind == "parameter_section") {
        "parameter list"
    } else if ancestors.iter().any(|kind| kind == "port_body") {
        "port body"
    } else if ancestors.iter().any(|kind| kind == "record_type_body") {
        "struct body"
    } else if ancestors.iter().any(|kind| kind == "block") {
        "block"
    } else if ancestors.iter().any(|kind| kind == "type_arguments") {
        "type arguments"
    } else {
        "source file"
    }
}

fn note_from_ancestors(ancestors: &[String]) -> Option<String> {
    if ancestors
        .iter()
        .any(|kind| kind == "named_parameter_section")
    {
        Some("check for a missing `}` or `,` in the named parameter section".to_owned())
    } else if ancestors.iter().any(|kind| kind == "parameter_section") {
        Some("check for a missing `)` or `,` in the parameter list".to_owned())
    } else if ancestors.iter().any(|kind| kind == "port_body") {
        Some("check for a missing `,` or `}` in the port body".to_owned())
    } else if ancestors.iter().any(|kind| kind == "record_type_body") {
        Some("check for a missing `:` or `}` in the struct body".to_owned())
    } else if ancestors.iter().any(|kind| kind == "block") {
        Some("check for a missing `;` or `}` in the block".to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_component_examples_into_cst() {
        for source in [
            include_str!("../../../../examples/add_constant.plr"),
            include_str!("../../../../examples/mult_add.plr"),
            include_str!("../../../../examples/counter.plr"),
            include_str!("../../../../examples/shift_register.plr"),
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
            include_str!("../../../../examples/parameterized_struct.plr"),
            include_str!("../../../../examples/parameterized_port.plr"),
        ] {
            let cst = parse_source(source).unwrap();
            assert_eq!(cst.root.kind, "source_file");
            assert!(!cst.root.children.is_empty());
        }
    }

    #[test]
    fn root_span_covers_full_source() {
        let source = include_str!("../../../../examples/add_constant.plr");
        let cst = parse_source(source).unwrap();
        assert_eq!(cst.root.span.start_byte, 0);
        assert_eq!(cst.root.span.end_byte, source.len());
    }

    #[test]
    fn reports_missing_named_section_closer_once() {
        let source = include_str!("../../../../fail-examples/missing-parenthesis.plr");
        let error = parse_source(source).unwrap_err();
        let ParseError::Syntax(diagnostics) = error else {
            panic!("expected syntax diagnostics");
        };

        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("named parameter section"));
        assert!(diagnostics[0].message.contains("expected `}`"));
        assert!(
            diagnostics[0]
                .note
                .as_deref()
                .unwrap_or_default()
                .contains("missing `}`")
        );
    }

    #[test]
    fn rejects_all_failure_examples() {
        for source in [
            include_str!("../../../../fail-examples/missing-parenthesis.plr"),
            include_str!("../../../../fail-examples/missing-semicolon.plr"),
            include_str!("../../../../fail-examples/missing-struct-colon.plr"),
            include_str!("../../../../fail-examples/missing-port-comma.plr"),
        ] {
            let error = parse_source(source).unwrap_err();
            let ParseError::Syntax(diagnostics) = error else {
                panic!("expected syntax diagnostics");
            };
            assert!(!diagnostics.is_empty());
        }
    }

    #[test]
    fn preserves_cst_when_reporting_diagnostics() {
        let source = include_str!("../../../../fail-examples/missing-semicolon.plr");
        let parsed = parse_source_with_diagnostics(source).unwrap();

        assert_eq!(parsed.cst.root.kind, "source_file");
        assert!(!parsed.diagnostics.is_empty());
    }
}
