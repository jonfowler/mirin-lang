//! Source spans and diagnostic rendering.
//!
//! A [`Span`] is a byte range into a file's source. Front-end queries attach one
//! to each diagnostic so the CLI / LSP can point at the offending source;
//! [`render`] turns a span + message into the familiar `error: … --> file:L:C`
//! block with a caret, matching the reference compiler's style.

use std::fmt::Write;

/// A half-open byte range `[start, end)` into a file's source text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, salsa::Update)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }
}

/// 1-based line/column of a byte offset, counting columns in characters.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// The text of the 1-based `line`.
fn line_text(source: &str, line: usize) -> &str {
    source.lines().nth(line - 1).unwrap_or("")
}

/// Render a diagnostic with a source location and caret:
///
/// ```text
/// error: undefined name `offset`
///  --> t.plr:2:22
///   |
/// 2 |     let result = a + offset;
///   |                      ^^^^^^
/// ```
pub fn render(path: &str, source: &str, span: Span, message: &str) -> String {
    let (line, col) = line_col(source, span.start as usize);
    let text = line_text(source, line);
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    // Caret run: from the start column, as many chars as the span covers on this
    // line (at least one), counted in characters for alignment with the source.
    let span_chars = source
        .get(span.start as usize..span.end as usize)
        .map(|s| s.chars().take_while(|&c| c != '\n').count())
        .unwrap_or(0)
        .max(1);
    let mut out = String::new();
    let _ = writeln!(out, "error: {message}");
    let _ = writeln!(out, "{pad}--> {path}:{line}:{col}");
    let _ = writeln!(out, "{pad} |");
    let _ = writeln!(out, "{gutter} | {text}");
    let _ = write!(
        out,
        "{pad} | {}{}",
        " ".repeat(col - 1),
        "^".repeat(span_chars)
    );
    out
}
