//! Lower a tree-sitter CST into a [`Doc`]. One method per grammar rule; the
//! shapes mirror `packages/tree-sitter-polar/grammar.js`.
//!
//! Two CST quirks to keep in mind:
//! * `expression` and `statement` are *wrapper* rules — e.g. a `value` field
//!   holds an `expression` node whose single named child is the real
//!   `binary_expression`/`path_expression`/… We unwrap them in [`Doc`] dispatch.
//! * Line comments are `extras`: they appear as named `comment` children at the
//!   point they occur. Container formatting ([`Formatter::sequence`]) threads
//!   them through; comments anywhere else fall back to verbatim node text.

use tree_sitter::Node;

use crate::doc::Doc::{HardLine, Line};
use crate::doc::{Doc, concat, group, group_capped, if_break, indent};

pub struct Formatter<'a> {
    src: &'a [u8],
}

fn text(s: impl Into<String>) -> Doc {
    Doc::text(s)
}

const NIL: Doc = Doc::Nil;

impl<'a> Formatter<'a> {
    pub fn new(src: &'a str) -> Self {
        Formatter {
            src: src.as_bytes(),
        }
    }

    pub fn format(&self, root: Node) -> Doc {
        self.sequence(root)
    }

    // ---- small CST helpers -------------------------------------------------

    fn text(&self, n: Node) -> &'a str {
        std::str::from_utf8(&self.src[n.byte_range()]).unwrap_or("")
    }

    fn field<'t>(&self, n: Node<'t>, name: &str) -> Option<Node<'t>> {
        n.child_by_field_name(name)
    }

    /// All children carrying field `name`, in order.
    fn fields<'t>(&self, n: Node<'t>, name: &str) -> Vec<Node<'t>> {
        let mut c = n.walk();
        n.children_by_field_name(name, &mut c).collect()
    }

    fn named_children<'t>(&self, n: Node<'t>) -> Vec<Node<'t>> {
        let mut c = n.walk();
        n.named_children(&mut c).collect()
    }

    /// The first named child of a given kind.
    fn child_of_kind<'t>(&self, n: Node<'t>, kind: &str) -> Option<Node<'t>> {
        self.named_children(n)
            .into_iter()
            .find(|c| c.kind() == kind)
    }

    /// Named children of a given kind.
    fn children_of_kind<'t>(&self, n: Node<'t>, kind: &str) -> Vec<Node<'t>> {
        self.named_children(n)
            .into_iter()
            .filter(|c| c.kind() == kind)
            .collect()
    }

    fn has_child_kind(&self, n: Node, kind: &str) -> bool {
        let mut c = n.walk();
        n.children(&mut c).any(|ch| ch.kind() == kind)
    }

    // ---- dispatch ----------------------------------------------------------

    fn doc(&self, n: Node) -> Doc {
        match n.kind() {
            "expression" | "statement" | "type_argument" => match n.named_child(0) {
                Some(inner) => self.doc(inner),
                None => text(self.text(n)),
            },

            "function_definition" => self.fn_def(n),
            "struct_definition" => self.struct_def(n),
            "port_definition" => self.port_def(n),
            "impl_block" => self.impl_block(n),
            "module_definition" => self.module_def(n),
            "use_declaration" => self.use_decl(n),

            "let_statement" => self.let_stmt(n),
            "return_statement" => self.return_stmt(n),
            "var_statement" => self.var_stmt(n),
            "assignment_statement" => self.assignment_stmt(n),
            "expression_statement" => {
                let inner = n.named_child(0).unwrap();
                concat([self.doc(inner), text(";")])
            }

            "binary_expression" => self.binary(n),
            "postfix_expression" => self.postfix(n),
            "record_constructor_expression" => self.record_ctor(n),
            "record_literal" => self.record_literal(n),
            "path_expression" => self.path(n),
            "parenthesized_expression" => {
                let inner = n.named_child(0).unwrap();
                concat([text("("), self.doc(inner), text(")")])
            }
            "if_expression" => self.if_expr(n),
            "when_expression" => self.when_expr(n),

            "type_expression" => self.type_expr(n),
            "return_type_expression" => self.return_type_expr(n),

            "visibility_modifier" => self.visibility(n),
            "comment" => text(self.text(n).trim_end()),
            "identifier" | "number" => text(self.text(n)),

            // Unknown / unhandled: re-emit the original bytes verbatim so we
            // never silently drop or mangle content.
            _ => self.verbatim(n),
        }
    }

    /// Re-emit a node's source bytes, preserving its lines (each becomes a
    /// hard line). Used as a safe fallback for shapes we don't format.
    fn verbatim(&self, n: Node) -> Doc {
        let raw = self.text(n);
        let mut parts = Vec::new();
        for (i, line) in raw.split('\n').enumerate() {
            if i > 0 {
                parts.push(HardLine);
            }
            parts.push(text(line.trim_end()));
        }
        concat(parts)
    }

    // ---- comma-separated sections -----------------------------------------

    /// A delimited, comma-separated list with rustfmt semantics: one line when
    /// it fits, otherwise one element per line with a trailing comma. `pad`
    /// adds inner spaces in the flat form (`{ a, b }` vs `(a, b)`).
    ///
    /// If the list node contains comments we can't place safely, fall back to
    /// verbatim so nothing is dropped.
    fn delimited(&self, node: Node, open: &str, close: &str, elem_kind: &str, pad: bool) -> Doc {
        if self.has_child_kind(node, "comment") {
            return self.verbatim(node);
        }
        let items: Vec<Doc> = self
            .children_of_kind(node, elem_kind)
            .into_iter()
            .map(|c| self.elem(c))
            .collect();
        self.delimited_items(open, close, items, pad)
    }

    fn delimited_items(&self, open: &str, close: &str, items: Vec<Doc>, pad: bool) -> Doc {
        if items.is_empty() {
            return concat([text(open), text(close)]);
        }
        let edge = if pad { Line } else { Doc::SoftLine };
        let mut inner = Vec::new();
        for (i, it) in items.into_iter().enumerate() {
            if i > 0 {
                inner.push(concat([text(","), Line]));
            }
            inner.push(it);
        }
        group(concat([
            text(open),
            indent(concat([edge.clone(), concat(inner)])),
            if_break(text(","), NIL),
            edge,
            text(close),
        ]))
    }

    fn named_section(&self, n: Node) -> Doc {
        self.delimited(n, "{", "}", "named_parameter", true)
    }

    fn params_section(&self, n: Node) -> Doc {
        self.delimited(n, "(", ")", "parameter", false)
    }

    // ---- items -------------------------------------------------------------

    fn vis_prefix(&self, n: Node) -> Doc {
        match self.field(n, "visibility") {
            Some(v) => concat([self.doc(v), text(" ")]),
            None => NIL,
        }
    }

    fn fn_def(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let name = self.text(self.field(n, "name").unwrap());
        let named = self.field(n, "named_parameters");
        let params = self.field(n, "parameters").unwrap();
        let ret = self.field(n, "return_type");

        let ret_doc = ret.map(|r| concat([text("-> "), self.doc(r)]));

        let sig = if let Some(named) = named {
            // Two-section signature: sections break onto their own lines.
            let mut sections = vec![
                Line,
                self.named_section(named),
                Line,
                self.params_section(params),
            ];
            if let Some(rd) = ret_doc {
                sections.push(Line);
                sections.push(rd);
            }
            group(concat([
                vis,
                text("fn "),
                text(name),
                indent(concat(sections)),
            ]))
        } else {
            // rustfmt-style: parameters glued to the name, return type glued.
            let mut parts = vec![vis, text("fn "), text(name), self.params_section(params)];
            if let Some(rd) = ret_doc {
                parts.push(text(" "));
                parts.push(rd);
            }
            concat(parts)
        };

        concat([
            sig,
            text(" "),
            self.block_doc(self.field(n, "body").unwrap()),
        ])
    }

    fn struct_def(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let name = self.text(self.field(n, "name").unwrap());
        let params = self
            .field(n, "parameters")
            .map(|p| self.params_section(p))
            .unwrap_or(NIL);
        let ctor = self.text(self.field(n, "constructor").unwrap());
        let body = self.delimited(
            self.field(n, "body").unwrap(),
            "{",
            "}",
            "record_field_type",
            true,
        );
        concat([
            vis,
            text("struct "),
            text(name),
            params,
            text(" = "),
            text(ctor),
            text(" "),
            body,
        ])
    }

    fn port_def(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let name = self.text(self.field(n, "name").unwrap());
        let named = self.field(n, "named_parameters");
        let params = self.field(n, "parameters");
        let ctor = self.text(self.field(n, "constructor").unwrap());
        let body = self.delimited(self.field(n, "body").unwrap(), "{", "}", "port_field", true);

        let header = if let Some(named) = named {
            let mut sections = vec![Line, self.named_section(named)];
            if let Some(p) = params {
                sections.push(Line);
                sections.push(self.params_section(p));
            }
            group(concat([
                vis,
                text("port "),
                text(name),
                indent(concat(sections)),
            ]))
        } else {
            let params_doc = params.map(|p| self.params_section(p)).unwrap_or(NIL);
            concat([vis, text("port "), text(name), params_doc])
        };

        concat([header, text(" = "), text(ctor), text(" "), body])
    }

    fn impl_block(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let named = self.field(n, "named_parameters");
        let params = self.field(n, "parameters");

        let header = if let Some(named) = named {
            let mut sections = vec![Line, self.named_section(named)];
            if let Some(p) = params {
                sections.push(Line);
                sections.push(self.params_section(p));
            }
            group(concat([
                text("impl "),
                text(name),
                indent(concat(sections)),
            ]))
        } else {
            let params_doc = params.map(|p| self.params_section(p)).unwrap_or(NIL);
            concat([text("impl "), text(name), params_doc])
        };

        concat([
            header,
            text(" "),
            self.braced_items(self.field(n, "body").unwrap()),
        ])
    }

    fn module_def(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let name = self.text(self.field(n, "name").unwrap());
        match self.field(n, "body") {
            Some(body) => concat([
                vis,
                text("mod "),
                text(name),
                text(" "),
                self.braced_items(body),
            ]),
            None => concat([vis, text("mod "), text(name), text(";")]),
        }
    }

    /// A `{ items… }` block of items (module/impl bodies), always multi-line
    /// unless empty.
    fn braced_items(&self, body: Node) -> Doc {
        if self.named_children(body).is_empty() {
            return text("{}");
        }
        group(concat([
            text("{"),
            indent(concat([HardLine, self.sequence(body)])),
            HardLine,
            text("}"),
        ]))
    }

    fn use_decl(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let tree = self.use_tree(self.field(n, "tree").unwrap());
        concat([vis, text("use "), tree, text(";")])
    }

    fn use_tree(&self, n: Node) -> Doc {
        let path = self.child_of_kind(n, "use_path").map(|p| self.use_path(p));
        if let Some(alias) = self.field(n, "alias") {
            return concat([path.unwrap_or(NIL), text(" as "), text(self.text(alias))]);
        }
        if let Some(group_node) = self.field(n, "group") {
            let g = self.delimited(group_node, "{", "}", "use_tree", false);
            return match path {
                Some(p) => concat([p, text("::"), g]),
                None => g,
            };
        }
        if self.field(n, "glob").is_some() {
            return match path {
                Some(p) => concat([p, text("::*")]),
                None => text("*"),
            };
        }
        path.unwrap_or(NIL)
    }

    fn use_path(&self, n: Node) -> Doc {
        let segs: Vec<&str> = self
            .named_children(n)
            .into_iter()
            .filter(|c| c.kind() == "identifier")
            .map(|c| self.text(c))
            .collect();
        text(segs.join("::"))
    }

    fn visibility(&self, n: Node) -> Doc {
        if !self.has_child_kind(n, "(") {
            return text("pub");
        }
        if self.has_child_kind(n, "crate") {
            return text("pub(crate)");
        }
        if self.has_child_kind(n, "super") {
            return text("pub(super)");
        }
        // pub(in path)
        if let Some(p) = self.child_of_kind(n, "use_path") {
            return concat([text("pub(in "), self.use_path(p), text(")")]);
        }
        text("pub")
    }

    // ---- statements --------------------------------------------------------

    fn let_stmt(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let value = self.doc(self.field(n, "value").unwrap());
        group(concat([
            text("let "),
            text(name),
            text(" ="),
            indent(concat([Line, value])),
            text(";"),
        ]))
    }

    fn return_stmt(&self, n: Node) -> Doc {
        let value = self.doc(self.field(n, "value").unwrap());
        group(concat([
            text("return"),
            indent(concat([Line, value])),
            text(";"),
        ]))
    }

    fn var_stmt(&self, n: Node) -> Doc {
        let names: Vec<&str> = self
            .fields(n, "name")
            .into_iter()
            .map(|c| self.text(c))
            .collect();
        let mut parts = vec![text("var "), text(names.join(", "))];
        if let Some(ty) = self.field(n, "type") {
            parts.push(concat([text(": "), self.doc(ty)]));
        }
        if let Some(val) = self.field(n, "value") {
            return group(concat([
                concat(parts),
                text(" ="),
                indent(concat([Line, self.doc(val)])),
                text(";"),
            ]));
        }
        concat([concat(parts), text(";")])
    }

    fn assignment_stmt(&self, n: Node) -> Doc {
        let left = self.doc(self.field(n, "left").unwrap());
        let right = self.doc(self.field(n, "right").unwrap());
        group(concat([
            left,
            text(" ="),
            indent(concat([Line, right])),
            text(";"),
        ]))
    }

    // ---- expressions -------------------------------------------------------

    fn binary(&self, n: Node) -> Doc {
        let left = self.doc(self.field(n, "left").unwrap());
        let op = self.text(self.field(n, "operator").unwrap());
        let right = self.doc(self.field(n, "right").unwrap());
        concat([left, text(" "), text(op), text(" "), right])
    }

    fn postfix(&self, n: Node) -> Doc {
        let mut parts = Vec::new();
        for child in self.named_children(n) {
            match child.kind() {
                "field_access" => {
                    let field = self.text(self.field(child, "field").unwrap());
                    parts.push(concat([text("."), text(field)]));
                }
                "argument_list" => parts.push(self.argument_list(child)),
                "named_argument_list" => parts.push(self.named_argument_list(child)),
                // The receiver (path_expression / number / parenthesized).
                _ => parts.push(self.doc(child)),
            }
        }
        concat(parts)
    }

    fn argument_list(&self, n: Node) -> Doc {
        if self.has_child_kind(n, "comment") {
            return self.verbatim(n);
        }
        let items: Vec<Doc> = self
            .named_children(n)
            .into_iter()
            .map(|c| match c.kind() {
                "out_argument" => self.out_argument(c),
                _ => self.doc(c),
            })
            .collect();
        self.delimited_items("(", ")", items, false)
    }

    fn named_argument_list(&self, n: Node) -> Doc {
        self.delimited(n, "{", "}", "named_or_shorthand_argument", true)
    }

    fn out_argument(&self, n: Node) -> Doc {
        let dir = self
            .field(n, "direction")
            .map(|_| text("out "))
            .unwrap_or(NIL);
        let target = self.text(self.field(n, "target").unwrap());
        concat([dir, text("=> "), text(target)])
    }

    fn record_ctor(&self, n: Node) -> Doc {
        let ctor = self.text(self.field(n, "constructor").unwrap());
        let body = self.doc(self.field(n, "body").unwrap());
        concat([text(ctor), text(" "), body])
    }

    fn record_literal(&self, n: Node) -> Doc {
        self.delimited(n, "{", "}", "record_field_value", true)
    }

    fn path(&self, n: Node) -> Doc {
        let segs: Vec<&str> = self
            .fields(n, "segment")
            .into_iter()
            .map(|c| self.text(c))
            .collect();
        text(segs.join("::"))
    }

    fn if_expr(&self, n: Node) -> Doc {
        let cond = self.doc(self.field(n, "condition").unwrap());
        let (then_inner, then_multi) = self.block_inner(self.field(n, "then_branch").unwrap());
        let (else_inner, else_multi) = self.block_inner(self.field(n, "else_branch").unwrap());
        let doc = concat([
            text("if "),
            cond,
            text(" {"),
            indent(concat([Line, then_inner])),
            Line,
            text("} else {"),
            indent(concat([Line, else_inner])),
            Line,
            text("}"),
        ]);
        if then_multi || else_multi {
            group(doc)
        } else {
            group_capped(doc, SINGLE_LINE_IF_MAX)
        }
    }

    fn when_expr(&self, n: Node) -> Doc {
        let event = self.doc(self.field(n, "event").unwrap());
        let (inner, multi) = self.block_inner(self.field(n, "body").unwrap());
        let doc = concat([
            text("when "),
            event,
            text(" {"),
            indent(concat([Line, inner])),
            Line,
            text("}"),
        ]);
        if multi {
            group(doc)
        } else {
            group_capped(doc, SINGLE_LINE_IF_MAX)
        }
    }

    // ---- types -------------------------------------------------------------

    fn type_expr(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let mut parts = vec![text(name)];
        if let Some(na) = self.child_of_kind(n, "type_named_args") {
            parts.push(self.delimited(na, "{", "}", "type_argument", false));
        }
        if let Some(ti) = self.child_of_kind(n, "type_index") {
            parts.push(self.delimited(ti, "(", ")", "type_argument", false));
        }
        if let Some(dom) = self.field(n, "domain") {
            parts.push(concat([text(" @"), text(self.text(dom))]));
        }
        concat(parts)
    }

    fn return_type_expr(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let mut parts = vec![text(name)];
        if let Some(ti) = self.child_of_kind(n, "type_index") {
            parts.push(self.delimited(ti, "(", ")", "type_argument", false));
        }
        if let Some(dom) = self.field(n, "domain") {
            parts.push(concat([text(" @"), text(self.text(dom))]));
        }
        concat(parts)
    }

    // ---- parameters --------------------------------------------------------

    /// Dispatcher for comma-separated list elements — kinds that only ever
    /// appear inside a delimited section and so aren't in `doc`'s match.
    fn elem(&self, n: Node) -> Doc {
        match n.kind() {
            "named_parameter" => self.named_parameter(n),
            "parameter" => self.parameter(n),
            "record_field_type" => self.record_field_type(n),
            "record_field_value" => self.record_field_value(n),
            "port_field" => self.port_field(n),
            "named_or_shorthand_argument" => self.named_or_shorthand(n),
            "use_tree" => self.use_tree(n),
            _ => self.doc(n),
        }
    }

    fn record_field_value(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let value = self.doc(self.field(n, "value").unwrap());
        concat([text(name), text(": "), value])
    }

    fn named_parameter(&self, n: Node) -> Doc {
        let mut parts = Vec::new();
        if let Some(d) = self.field(n, "direction") {
            parts.push(text(format!("{} ", self.text(d))));
        }
        if let Some(k) = self.field(n, "kind") {
            parts.push(text(format!("{} ", self.text(k))));
        }
        parts.push(text(self.text(self.field(n, "name").unwrap())));
        if let Some(ty) = self.field(n, "type") {
            parts.push(concat([text(": "), self.doc(ty)]));
        }
        if let Some(def) = self.field(n, "default") {
            parts.push(concat([text(" = "), self.doc(def)]));
        }
        concat(parts)
    }

    fn parameter(&self, n: Node) -> Doc {
        // `self` form: `self` with an optional `@domain`, no `: type`.
        if self.field(n, "type").is_none() && self.has_child_kind(n, "self") {
            let dom = self
                .field(n, "domain")
                .map(|d| concat([text(" @"), text(self.text(d))]))
                .unwrap_or(NIL);
            return concat([text("self"), dom]);
        }
        let mut parts = Vec::new();
        if let Some(d) = self.field(n, "direction") {
            parts.push(text(format!("{} ", self.text(d))));
        }
        if let Some(k) = self.field(n, "kind") {
            parts.push(text(format!("{} ", self.text(k))));
        }
        parts.push(text(self.text(self.field(n, "name").unwrap())));
        if let Some(ty) = self.field(n, "type") {
            parts.push(concat([text(": "), self.doc(ty)]));
        }
        if let Some(def) = self.field(n, "default") {
            parts.push(concat([text(" = "), self.doc(def)]));
        }
        concat(parts)
    }

    fn record_field_type(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let ty = self.doc(self.field(n, "type").unwrap());
        concat([text(name), text(": "), ty])
    }

    fn port_field(&self, n: Node) -> Doc {
        let dir = self.text(self.field(n, "direction").unwrap());
        let name = self.text(self.field(n, "name").unwrap());
        let ty = self.doc(self.field(n, "type").unwrap());
        concat([text(format!("{dir} ")), text(name), text(": "), ty])
    }

    fn named_or_shorthand(&self, n: Node) -> Doc {
        let dir = self
            .field(n, "direction")
            .map(|d| text(format!("{} ", self.text(d))))
            .unwrap_or(NIL);
        let name = self.text(self.field(n, "name").unwrap());
        if let Some(target) = self.field(n, "target") {
            return concat([dir, text(name), text(" => "), text(self.text(target))]);
        }
        if let Some(value) = self.field(n, "value") {
            return concat([dir, text(name), text(" = "), self.doc(value)]);
        }
        // Shorthand: just the name.
        concat([dir, text(name)])
    }

    // ---- blocks & sequences ------------------------------------------------

    /// A function body block: braces, always multi-line unless empty.
    fn block_doc(&self, block: Node) -> Doc {
        if self.named_children(block).is_empty() {
            return text("{}");
        }
        group(concat([
            text("{"),
            indent(concat([HardLine, self.sequence(block)])),
            HardLine,
            text("}"),
        ]))
    }

    /// The inner contents of a block (statements + tail), without braces, plus
    /// whether the block has statements (so the caller can force a break).
    fn block_inner(&self, block: Node) -> (Doc, bool) {
        let has_stmts = self.has_child_kind(block, "statement");
        (self.sequence(block), has_stmts)
    }

    /// Format the named children of a container in order, threading line
    /// comments and collapsing runs of blank lines to a single blank line.
    fn sequence(&self, parent: Node) -> Doc {
        let kids = self.named_children(parent);
        let mut parts = Vec::new();
        let mut prev_end: Option<usize> = None;
        for k in kids {
            let start = k.start_position().row;
            let is_comment = k.kind() == "comment";
            match prev_end {
                None => parts.push(self.doc_in_sequence(k)),
                Some(pe) => {
                    if is_comment && start == pe {
                        // Trailing comment: stays on the previous line.
                        parts.push(text(" "));
                        parts.push(self.doc(k));
                    } else {
                        parts.push(HardLine);
                        if start > pe + 1 {
                            parts.push(HardLine);
                        }
                        parts.push(self.doc_in_sequence(k));
                    }
                }
            }
            prev_end = Some(k.end_position().row);
        }
        concat(parts)
    }

    /// In a block, a bare `expression` child is the tail value; statements wrap
    /// their own node. Both go through `doc`, but we keep this seam in case the
    /// tail ever needs distinct handling.
    fn doc_in_sequence(&self, n: Node) -> Doc {
        self.doc(n)
    }
}

/// rustfmt's `single_line_if_else_max_width`: an if/when whose one-line form is
/// within this stays on one line; otherwise the braces expand.
const SINGLE_LINE_IF_MAX: usize = 50;
