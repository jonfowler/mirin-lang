//! A small Wadler/Prettier-style document algebra and a width-aware renderer.
//!
//! The formatter lowers the CST into a [`Doc`] and the renderer turns it into
//! text. The key primitive is [`Doc::Group`]: a group renders *flat* (every
//! `Line` inside it becomes a space) when it fits on the remaining line,
//! otherwise it *breaks* (every `Line` becomes a newline + indent). This is how
//! we get rustfmt's "collapse to one line when it fits, break when it doesn't"
//! behaviour without hand-written width arithmetic at each call site.
//!
//! Differences from textbook Wadler:
//! * `Group` carries a `must_break` flag, set automatically when its contents
//!   contain a [`Doc::HardLine`]; the break propagates out through enclosing
//!   groups (a hard line anywhere forces every ancestor group to break, like
//!   Prettier).
//! * `Group` carries an optional `flat_max`: even if it fits the line, the flat
//!   form is only used when its width is within this cap. This implements
//!   rustfmt's `single_line_if_else_max_width`-style sub-width limits.
//! * `IfBreak` emits different content depending on the enclosing group's mode
//!   (used for trailing commas that appear only when a list breaks).

/// The indentation unit, in spaces. rustfmt's default.
pub const INDENT: usize = 4;

/// The target maximum line width. rustfmt's default.
pub const MAX_WIDTH: usize = 100;

#[derive(Debug, Clone)]
pub enum Doc {
    Nil,
    /// Literal text. Must not contain newlines.
    Text(String),
    /// A space when flat, a newline + indent when broken.
    Line,
    /// Nothing when flat, a newline + indent when broken.
    SoftLine,
    /// Always a newline + indent. Forces every enclosing group to break.
    HardLine,
    Concat(Vec<Doc>),
    /// Increase the indent of everything inside by one [`INDENT`] unit.
    Indent(Box<Doc>),
    Group {
        doc: Box<Doc>,
        /// Forced to break (contains a hard line). Precomputed at build time.
        must_break: bool,
        /// If set, the flat form is only chosen when its width is within this.
        flat_max: Option<usize>,
    },
    /// `broken` when the enclosing group breaks, `flat` when it is flat.
    IfBreak {
        broken: Box<Doc>,
        flat: Box<Doc>,
    },
}

impl Doc {
    pub fn text(s: impl Into<String>) -> Doc {
        Doc::Text(s.into())
    }
}

/// Concatenate, dropping `Nil`s. Flattens nested concats lightly.
pub fn concat(parts: impl IntoIterator<Item = Doc>) -> Doc {
    let v: Vec<Doc> = parts
        .into_iter()
        .filter(|d| !matches!(d, Doc::Nil))
        .collect();
    match v.len() {
        0 => Doc::Nil,
        1 => v.into_iter().next().unwrap(),
        _ => Doc::Concat(v),
    }
}

pub fn indent(doc: Doc) -> Doc {
    Doc::Indent(Box::new(doc))
}

/// Build a group, precomputing `must_break` by scanning for hard lines.
pub fn group(doc: Doc) -> Doc {
    let must_break = contains_hardline(&doc);
    Doc::Group {
        doc: Box::new(doc),
        must_break,
        flat_max: None,
    }
}

/// A group whose flat form is only used when its flat width is `<= flat_max`.
pub fn group_capped(doc: Doc, flat_max: usize) -> Doc {
    let must_break = contains_hardline(&doc);
    Doc::Group {
        doc: Box::new(doc),
        must_break,
        flat_max: Some(flat_max),
    }
}

pub fn if_break(broken: Doc, flat: Doc) -> Doc {
    Doc::IfBreak {
        broken: Box::new(broken),
        flat: Box::new(flat),
    }
}

/// Hard lines propagate out through nested groups, so we scan through them.
fn contains_hardline(doc: &Doc) -> bool {
    match doc {
        Doc::HardLine => true,
        Doc::Concat(v) => v.iter().any(contains_hardline),
        Doc::Indent(d) => contains_hardline(d),
        Doc::Group { doc, .. } => contains_hardline(doc),
        // An IfBreak's hard line only fires in one mode; conservatively treat a
        // hard line in either branch as forcing a break.
        Doc::IfBreak { broken, flat } => contains_hardline(broken) || contains_hardline(flat),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

/// Render `doc` to a string at the given max `width`.
pub fn render(doc: &Doc, width: usize) -> String {
    let mut out = String::new();
    // Work stack of (indent, mode, doc), processed LIFO.
    let mut stack: Vec<(usize, Mode, &Doc)> = vec![(0, Mode::Break, doc)];
    // Current column (printed width since the last newline).
    let mut col = 0usize;

    while let Some((ind, mode, d)) = stack.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => {
                out.push_str(s);
                col += s.chars().count();
            }
            Doc::Concat(parts) => {
                for p in parts.iter().rev() {
                    stack.push((ind, mode, p));
                }
            }
            Doc::Indent(inner) => stack.push((ind + INDENT, mode, inner)),
            Doc::Line => match mode {
                Mode::Flat => {
                    out.push(' ');
                    col += 1;
                }
                Mode::Break => {
                    out.push('\n');
                    out.push_str(&" ".repeat(ind));
                    col = ind;
                }
            },
            Doc::SoftLine => match mode {
                Mode::Flat => {}
                Mode::Break => {
                    out.push('\n');
                    out.push_str(&" ".repeat(ind));
                    col = ind;
                }
            },
            Doc::HardLine => {
                out.push('\n');
                out.push_str(&" ".repeat(ind));
                col = ind;
            }
            Doc::IfBreak { broken, flat } => {
                let chosen = match mode {
                    Mode::Break => broken,
                    Mode::Flat => flat,
                };
                stack.push((ind, mode, chosen));
            }
            Doc::Group {
                doc,
                must_break,
                flat_max,
            } => {
                let flat_ok = !*must_break
                    && flat_max.is_none_or(|cap| flat_width(doc).is_some_and(|w| w <= cap))
                    && fits(width.saturating_sub(col), ind, doc, &stack);
                let m = if flat_ok { Mode::Flat } else { Mode::Break };
                stack.push((ind, m, doc));
            }
        }
    }

    normalize(&out)
}

/// Does `doc`, rendered flat at column offset `remaining`, plus the already
/// queued work, fit before the next forced newline? Standard Lindig `fits`.
fn fits(remaining: usize, ind: usize, doc: &Doc, rest: &[(usize, Mode, &Doc)]) -> bool {
    let mut remaining = remaining as isize;
    // Local stack seeded with `doc` (flat), then the pending work in order.
    let mut local: Vec<(usize, Mode, &Doc)> = Vec::new();
    // `rest` is a LIFO stack; its top is the last element. Push our doc so it
    // is processed first, then the rest in the order they'd be popped.
    for item in rest.iter() {
        local.push(*item);
    }
    local.push((ind, Mode::Flat, doc));

    while let Some((i, mode, d)) = local.pop() {
        if remaining < 0 {
            return false;
        }
        match d {
            Doc::Nil => {}
            Doc::Text(s) => remaining -= s.chars().count() as isize,
            Doc::Concat(parts) => {
                for p in parts.iter().rev() {
                    local.push((i, mode, p));
                }
            }
            Doc::Indent(inner) => local.push((i + INDENT, mode, inner)),
            Doc::Line => match mode {
                Mode::Flat => remaining -= 1,
                // A break-mode line ends the current line: everything so far fit.
                Mode::Break => return true,
            },
            Doc::SoftLine => match mode {
                Mode::Flat => {}
                Mode::Break => return true,
            },
            // A hard line ends the line; whatever came before it fit.
            Doc::HardLine => return true,
            Doc::IfBreak { broken, flat } => {
                let chosen = match mode {
                    Mode::Break => broken,
                    Mode::Flat => flat,
                };
                local.push((i, mode, chosen));
            }
            Doc::Group { doc, .. } => local.push((i, Mode::Flat, doc)),
        }
    }
    remaining >= 0
}

/// The width of `doc` rendered fully flat, or `None` if it contains a forced
/// newline (a hard line) and therefore has no single-line width.
fn flat_width(doc: &Doc) -> Option<usize> {
    match doc {
        Doc::Nil | Doc::SoftLine => Some(0),
        Doc::Line => Some(1),
        Doc::HardLine => None,
        Doc::Text(s) => Some(s.chars().count()),
        Doc::Concat(parts) => {
            let mut total = 0;
            for p in parts {
                total += flat_width(p)?;
            }
            Some(total)
        }
        Doc::Indent(inner) => flat_width(inner),
        Doc::Group { doc, .. } => flat_width(doc),
        Doc::IfBreak { flat, .. } => flat_width(flat),
    }
}

/// Strip trailing whitespace from every line and guarantee a single final
/// newline. The renderer emits indent spaces eagerly after each newline, which
/// leaves trailing spaces on otherwise-blank lines; this cleans them up.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 1);
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    while out.ends_with('\n') {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}
