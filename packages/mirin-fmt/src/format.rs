//! Lower a tree-sitter CST into a [`Doc`]. One method per grammar rule; the
//! shapes mirror `packages/tree-sitter-mirin/grammar.js`.
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
            "expression" | "statement" | "type_argument" | "const_expression" => {
                match n.named_child(0) {
                    Some(inner) => self.doc(inner),
                    None => text(self.text(n)),
                }
            }
            "const_binary" => {
                let l = self.doc(self.field(n, "left").unwrap());
                let op = self.text(self.field(n, "operator").unwrap());
                let r = self.doc(self.field(n, "right").unwrap());
                concat([l, text(format!(" {op} ")), r])
            }
            "const_field" => {
                let base = self.text(self.field(n, "base").unwrap());
                let fields: Vec<&str> = self
                    .fields(n, "field")
                    .into_iter()
                    .map(|c| self.text(c))
                    .collect();
                text(format!("{base}.{f}", f = fields.join(".")))
            }
            "const_paren" => {
                let inner = n.named_child(0).unwrap();
                concat([text("("), self.doc(inner), text(")")])
            }

            "function_definition" => {
                // An inline-verilog fn passes through verbatim — raw SV is
                // not ours to reformat.
                if n.child_by_field_name("verilog_body").is_some() {
                    self.verbatim(n)
                } else {
                    self.fn_def(n)
                }
            }
            "struct_definition" => self.struct_def(n),
            "port_definition" => self.port_def(n),
            "trait_definition" => self.trait_def(n),
            "trait_method" => self.trait_method(n),
            "trait_const" | "impl_const" => self.assoc_const(n),
            "impl_block" => self.impl_block(n),
            "module_definition" => self.module_def(n),
            "use_declaration" => self.use_decl(n),

            "vec_literal" => {
                if let Some(elem) = self.field(n, "elem") {
                    return concat([
                        text("["),
                        self.doc(elem),
                        text("; "),
                        self.doc(self.field(n, "len").unwrap()),
                        text("]"),
                    ]);
                }
                let mut cursor = n.walk();
                let elems: Vec<Doc> = n
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "expression")
                    .map(|c| self.doc(c))
                    .collect();
                let mut parts = vec![text("[")];
                for (i, e) in elems.into_iter().enumerate() {
                    if i > 0 {
                        parts.push(text(", "));
                    }
                    parts.push(e);
                }
                parts.push(text("]"));
                concat(parts)
            }
            "index_access" => concat([
                text("["),
                self.doc(self.field(n, "index").unwrap()),
                text("]"),
            ]),
            // `x[lo..hi]` / `x[off..+w]` / elided ends (planning/slicing.md).
            "slice_access" => {
                let lo = self.field(n, "low").map(|c| self.doc(c)).unwrap_or(NIL);
                let after = match (self.field(n, "high"), self.field(n, "width")) {
                    (Some(h), _) => self.doc(h),
                    (_, Some(w)) => concat([text("+"), self.doc(w)]),
                    _ => NIL,
                };
                concat([text("["), lo, text(".."), after, text("]")])
            }
            "typed_literal" => concat([
                self.doc(self.field(n, "type").unwrap()),
                text("::"),
                text(self.text(self.field(n, "value").unwrap())),
            ]),
            "type_path_call" => concat([
                self.doc(self.field(n, "type").unwrap()),
                text("::"),
                text(self.text(self.field(n, "method").unwrap())),
                self.argument_list(self.field(n, "arguments").unwrap()),
            ]),
            "unary_expression" => {
                let op = self.text(self.field(n, "operator").unwrap());
                concat([text(op), self.doc(self.field(n, "operand").unwrap())])
            }
            "const_path" => {
                let base = self.text(self.field(n, "base").unwrap());
                let item = self.text(self.field(n, "item").unwrap());
                text(format!("{base}::{item}"))
            }
            "trait_bound" => {
                let name = self.text(self.field(n, "name").unwrap());
                match n
                    .named_children(&mut n.walk())
                    .find(|c| c.kind() == "type_index")
                {
                    Some(ix) => concat([text(name), self.doc(ix)]),
                    None => text(name),
                }
            }
            "where_clause" => self.where_clause(n),

            "for_statement" => {
                let mut parts = vec![text("for ")];
                parts.push(self.doc(self.field(n, "pattern").unwrap()));
                parts.push(text(" in "));
                parts.push(self.doc(self.field(n, "iter").unwrap()));
                parts.push(text(" "));
                parts.push(self.block_doc(self.field(n, "body").unwrap()));
                concat(parts)
            }
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
            "if_expression" => self.if_expr(n, "if "),
            "const_if_expression" => self.if_expr(n, "const if "),
            "when_expression" => self.when_expr(n),

            "type_expression" => self.type_expr(n),
            "return_type_expression" => self.return_type_expr(n),
            "tuple_expression" => self.tuple_like(n, &["expression"]),
            "tuple_pattern" => {
                self.tuple_like(n, &["identifier", "tuple_pattern", "struct_pattern"])
            }
            "struct_pattern" => self.struct_pattern(n),
            "tuple_type" => self.tuple_type(n),
            "named_return" => self.named_return(n),
            "parenthesized_return_type" => concat([
                text("("),
                self.doc(self.field(n, "type").unwrap()),
                text(")"),
            ]),

            "visibility_modifier" => self.visibility(n),
            "comment" => text(self.text(n).trim_end()),
            "identifier" | "number" => text(self.text(n)),

            // Unknown / unhandled: re-emit the original bytes verbatim so we
            // never silently drop or mangle content.
            _ => self.verbatim(n),
        }
    }

    /// Re-emit a node's source bytes exactly, preserving its original layout.
    /// Used as a safe fallback for shapes we don't reformat — chiefly lists
    /// carrying comments, which rustfmt likewise leaves untouched rather than
    /// risk moving a comment away from what it annotates.
    fn verbatim(&self, n: Node) -> Doc {
        Doc::Raw(self.text(n).trim_end().to_string())
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

    /// A struct/port *definition* body. Per the Rust Style Guide, definition
    /// fields are always one-per-line with a trailing comma and the brace on
    /// the header line — never collapsed onto one line even when short (unlike
    /// record *literals*, which do collapse). Laying the body out vertically
    /// also means a long header is decided independently of its body.
    fn def_body(&self, node: Node, elem_kind: &str) -> Doc {
        let kids = self.named_children(node);
        if !kids.iter().any(|k| k.kind() == elem_kind) {
            // No fields (possibly only stray comments): keep braces together.
            if kids.is_empty() {
                return text("{}");
            }
            return self.verbatim(node);
        }

        // Thread fields and comments, mirroring `sequence` but appending a
        // trailing comma after each field.
        let mut parts = Vec::new();
        let mut prev_end: Option<usize> = None;
        for k in kids {
            let start = k.start_position().row;
            let is_comment = k.kind() == "comment";
            match prev_end {
                None => parts.push(self.field_with_comma(k, is_comment)),
                Some(pe) => {
                    if is_comment && start == pe {
                        parts.push(text(" "));
                        parts.push(self.doc(k));
                    } else {
                        parts.push(HardLine);
                        if start > pe + 1 {
                            parts.push(HardLine);
                        }
                        parts.push(self.field_with_comma(k, is_comment));
                    }
                }
            }
            prev_end = Some(k.end_position().row);
        }

        concat([
            text("{"),
            indent(concat([HardLine, concat(parts)])),
            HardLine,
            text("}"),
        ])
    }

    fn field_with_comma(&self, k: Node, is_comment: bool) -> Doc {
        if is_comment {
            self.doc(k)
        } else {
            concat([self.elem(k), text(",")])
        }
    }

    fn named_section(&self, n: Node) -> Doc {
        // No inner padding — the named-parameter section mirrors the positional
        // `(…)` section: `{dom clk: Clock}`, not `{ dom clk: Clock }`.
        self.delimited(n, "{", "}", "named_parameter", false)
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

    /// Outer attributes (`#[inline]`, …), each on its own line before the item.
    fn attrs_prefix(&self, n: Node) -> Doc {
        let attrs = self.fields(n, "attribute");
        concat(
            attrs
                .iter()
                .flat_map(|a| [self.attr(*a), Doc::HardLine])
                .collect::<Vec<_>>(),
        )
    }

    fn attr(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let args = match self.field(n, "arguments") {
            Some(a) => {
                let ids = self.children_of_kind(a, "identifier");
                let mut parts = vec![text("(")];
                for (i, c) in ids.iter().enumerate() {
                    if i > 0 {
                        parts.push(text(", "));
                    }
                    parts.push(text(self.text(*c)));
                }
                parts.push(text(")"));
                concat(parts)
            }
            None => NIL,
        };
        concat([text("#["), text(name), args, text("]")])
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

        let where_doc = match self.field(n, "where") {
            Some(w) => concat([text(" "), self.where_clause(w)]),
            None => NIL,
        };
        concat([
            self.attrs_prefix(n),
            sig,
            where_doc,
            text(" "),
            self.block_doc(self.field(n, "body").unwrap()),
        ])
    }

    fn struct_def(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let name = self.text(self.field(n, "name").unwrap());
        let named = self.field(n, "named_parameters");
        let params = self.field(n, "parameters");
        let ctor = self.text(self.field(n, "constructor").unwrap());
        let body = self.def_body(self.field(n, "body").unwrap(), "record_field_type");

        // A struct may declare a named (`{ dom clk, param N }`) section like a
        // port (planning/structs_as_ports.md); lay it out the same way.
        let header = if let Some(named) = named {
            let mut sections = vec![Line, self.named_section(named)];
            if let Some(p) = params {
                sections.push(Line);
                sections.push(self.params_section(p));
            }
            group(concat([
                vis,
                text("struct "),
                text(name),
                indent(concat(sections)),
            ]))
        } else {
            let params_doc = params.map(|p| self.params_section(p)).unwrap_or(NIL);
            concat([vis, text("struct "), text(name), params_doc])
        };

        concat([header, text(" = "), text(ctor), text(" "), body])
    }

    fn port_def(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let name = self.text(self.field(n, "name").unwrap());
        let named = self.field(n, "named_parameters");
        let params = self.field(n, "parameters");
        let ctor = self.text(self.field(n, "constructor").unwrap());
        let body = self.def_body(self.field(n, "body").unwrap(), "port_field");

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
        // Binder-first: `impl {dom clk: Clock} Stream8 { … }`; a trait impl
        // adds `for SelfType`.
        let mut parts = vec![self.attrs_prefix(n), text("impl ")];
        if let Some(named) = self.field(n, "named_parameters") {
            parts.push(self.named_section(named));
            parts.push(text(" "));
        }
        parts.push(text(name));
        if let Some(st) = self.field(n, "self_type") {
            parts.push(text(" for "));
            parts.push(self.doc(st));
        }
        if let Some(w) = self.field(n, "where") {
            parts.push(text(" "));
            parts.push(self.where_clause(w));
        }
        parts.push(text(" "));
        parts.push(self.braced_items(self.field(n, "body").unwrap()));
        concat(parts)
    }

    fn trait_def(&self, n: Node) -> Doc {
        let vis = self.vis_prefix(n);
        let name = self.text(self.field(n, "name").unwrap());
        concat([
            vis,
            text("trait "),
            text(name),
            text(" "),
            self.braced_items(self.field(n, "body").unwrap()),
        ])
    }

    /// A trait's method signature: fn-shaped, no body, trailing `;`.
    fn trait_method(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let mut parts = vec![text("fn "), text(name)];
        if let Some(named) = self.field(n, "named_parameters") {
            parts.push(self.named_section(named));
        }
        parts.push(self.params_section(self.field(n, "parameters").unwrap()));
        if let Some(r) = self.field(n, "return_type") {
            parts.push(text(" -> "));
            parts.push(self.doc(r));
        }
        parts.push(text(";"));
        concat(parts)
    }

    /// `const width: integer;` (trait) / `const width: integer = 1;` (impl).
    fn assoc_const(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let mut parts = vec![
            text("const "),
            text(name),
            text(": "),
            self.doc(self.field(n, "type").unwrap()),
        ];
        if let Some(v) = self.field(n, "value") {
            parts.push(text(" = "));
            parts.push(self.doc(v));
        }
        parts.push(text(";"));
        concat(parts)
    }

    fn where_clause(&self, n: Node) -> Doc {
        let mut cursor = n.walk();
        let preds: Vec<Doc> = n
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "where_predicate")
            .map(|c| {
                let name = self.text(self.field(c, "name").unwrap());
                concat([text(name), text(": "), self.bounds(c)])
            })
            .collect();
        let mut parts = vec![text("where ")];
        for (i, p) in preds.into_iter().enumerate() {
            if i > 0 {
                parts.push(text(", "));
            }
            parts.push(p);
        }
        concat(parts)
    }

    /// The `+`-joined `bound` fields of a node (`Add + Bits`).
    fn bounds(&self, n: Node) -> Doc {
        let mut cursor = n.walk();
        let bounds: Vec<Doc> = n
            .children_by_field_name("bound", &mut cursor)
            .map(|b| self.doc(b))
            .collect();
        let mut parts = Vec::new();
        for (i, b) in bounds.into_iter().enumerate() {
            if i > 0 {
                parts.push(text(" + "));
            }
            parts.push(b);
        }
        concat(parts)
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

    // The right-hand side is kept on the same line as `=`; any breaking happens
    // *inside* the value (e.g. a method chain or a call's argument list). This
    // matches rustfmt, which only drops the RHS to its own line as a last
    // resort — a case we don't yet handle (a long unbreakable RHS overflows).

    fn let_stmt(&self, n: Node) -> Doc {
        let pattern = self.doc(self.field(n, "pattern").unwrap());
        let kw = if self.field(n, "modifier").is_some() {
            "let mut "
        } else {
            "let "
        };
        let mut parts = vec![text(kw), pattern];
        if let Some(ty) = self.field(n, "type") {
            parts.push(concat([text(": "), self.doc(ty)]));
        }
        // The initialiser is optional: `let x;` is a declaration-only binding.
        if let Some(val) = self.field(n, "value") {
            parts.push(concat([text(" = "), self.doc(val)]));
        }
        parts.push(text(";"));
        concat(parts)
    }

    fn return_stmt(&self, n: Node) -> Doc {
        let value = self.doc(self.field(n, "value").unwrap());
        concat([text("return "), value, text(";")])
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
            parts.push(concat([text(" = "), self.doc(val)]));
        }
        parts.push(text(";"));
        concat(parts)
    }

    fn assignment_stmt(&self, n: Node) -> Doc {
        let left = self.doc(self.field(n, "left").unwrap());
        let right = self.doc(self.field(n, "right").unwrap());
        concat([left, text(" = "), right, text(";")])
    }

    // ---- expressions -------------------------------------------------------

    fn binary(&self, n: Node) -> Doc {
        let left = self.doc(self.field(n, "left").unwrap());
        let op = self.text(self.field(n, "operator").unwrap());
        let right = self.doc(self.field(n, "right").unwrap());
        concat([left, text(" "), text(op), text(" "), right])
    }

    /// A receiver followed by a run of `.field`, `(args)`, and `{named}`
    /// suffixes. Each `.field` (with its attached call args) is a *link*. When
    /// there are two or more links and the whole thing doesn't fit, rustfmt
    /// breaks before each `.` — receiver and first link on line one, the rest
    /// indented one level below. `(args)` never detaches from its method.
    fn postfix(&self, n: Node) -> Doc {
        let children = self.named_children(n);
        let receiver = self.doc(children[0]);

        // `base` is the receiver plus any calls applied directly to it (before
        // the first `.field`). Each later `.field`+args becomes a link.
        let mut base = vec![receiver];
        let mut links: Vec<Doc> = Vec::new();
        let mut cur: Option<Vec<Doc>> = None;
        for &child in &children[1..] {
            match child.kind() {
                "field_access" => {
                    if let Some(link) = cur.take() {
                        links.push(concat(link));
                    }
                    let field = self.text(self.field(child, "field").unwrap());
                    cur = Some(vec![text("."), text(field)]);
                }
                "argument_list" | "named_argument_list" => {
                    let d = if child.kind() == "argument_list" {
                        self.argument_list(child)
                    } else {
                        self.named_argument_list(child)
                    };
                    match cur.as_mut() {
                        Some(link) => link.push(d),
                        None => base.push(d),
                    }
                }
                _ => base.push(self.doc(child)),
            }
        }
        if let Some(link) = cur.take() {
            links.push(concat(link));
        }

        // Fewer than two links is a plain call/field access, not a chain: keep
        // it inline and let the argument lists break on their own.
        if links.len() < 2 {
            base.extend(links);
            return concat(base);
        }

        let first = links.remove(0);
        let head = concat([concat(base), first]);
        let tail: Vec<Doc> = links
            .into_iter()
            .map(|link| concat([Doc::SoftLine, link]))
            .collect();
        group(concat([head, indent(concat(tail))]))
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

    /// `pair { a = x, b = y }` — a struct destructuring pattern.
    fn struct_pattern(&self, n: Node) -> Doc {
        let ctor = self.text(self.field(n, "constructor").unwrap());
        let body = self.delimited(n, "{", "}", "struct_pattern_field", true);
        concat([text(ctor), text(" "), body])
    }

    /// `name = binding` — one field of a struct pattern.
    fn struct_pattern_field(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let binding = self.doc(self.field(n, "binding").unwrap());
        concat([text(name), text(" = "), binding])
    }

    fn path(&self, n: Node) -> Doc {
        let segs: Vec<&str> = self
            .fields(n, "segment")
            .into_iter()
            .map(|c| self.text(c))
            .collect();
        text(segs.join("::"))
    }

    fn if_expr(&self, n: Node, keyword: &str) -> Doc {
        let cond = self.doc(self.field(n, "condition").unwrap());
        let (then_inner, then_multi) = self.block_inner(self.field(n, "then_branch").unwrap());
        let (else_inner, else_multi) = self.block_inner(self.field(n, "else_branch").unwrap());
        let doc = concat([
            text(keyword),
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
        let init = match self.field(n, "init") {
            Some(v) => concat([text("init "), self.doc(v), text(" ")]),
            None => NIL,
        };
        let event = self.doc(self.field(n, "event").unwrap());
        let (inner, multi) = self.block_inner(self.field(n, "body").unwrap());
        let doc = concat([
            init,
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
    /// `(a, b)` tuple forms — expression and pattern: a parenthesised comma
    /// list over the given element kinds.
    fn tuple_like(&self, n: Node, kinds: &[&str]) -> Doc {
        if self.has_child_kind(n, "comment") {
            return self.verbatim(n);
        }
        let items: Vec<Doc> = self
            .named_children(n)
            .into_iter()
            .filter(|c| kinds.contains(&c.kind()))
            .map(|c| self.doc(c))
            .collect();
        self.delimited_items("(", ")", items, false)
    }

    /// `(output: DF @clk)` / `(sum: uint(8), carry: bool)` — named result(s).
    fn named_return(&self, n: Node) -> Doc {
        let items: Vec<Doc> = self
            .children_of_kind(n, "named_result")
            .into_iter()
            .map(|c| self.named_result(c))
            .collect();
        self.delimited_items("(", ")", items, false)
    }

    /// `name: T` — one named result.
    fn named_result(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        let ty = self.doc(self.field(n, "type").unwrap());
        concat([text(name), text(": "), ty])
    }

    /// `(A, B) @clk` — a tuple type with its optional trailing domain.
    fn tuple_type(&self, n: Node) -> Doc {
        if self.has_child_kind(n, "comment") {
            return self.verbatim(n);
        }
        let items: Vec<Doc> = self
            .named_children(n)
            .into_iter()
            .filter(|c| matches!(c.kind(), "type_expression" | "tuple_type"))
            .map(|c| self.doc(c))
            .collect();
        let mut parts = vec![self.delimited_items("(", ")", items, false)];
        if let Some(d) = self.field(n, "domain") {
            parts.push(text(" @"));
            parts.push(text(self.text(d)));
        }
        concat(parts)
    }

    fn elem(&self, n: Node) -> Doc {
        match n.kind() {
            "named_parameter" => self.named_parameter(n),
            "parameter" => self.parameter(n),
            "record_field_type" => self.record_field_type(n),
            "record_field_value" => self.record_field_value(n),
            "struct_pattern_field" => self.struct_pattern_field(n),
            "port_field" => self.port_field(n),
            "named_or_shorthand_argument" => self.named_or_shorthand(n),
            "use_tree" => self.use_tree(n),
            _ => self.doc(n),
        }
    }

    fn record_field_value(&self, n: Node) -> Doc {
        let name = self.text(self.field(n, "name").unwrap());
        if let Some(target) = self.field(n, "target") {
            return concat([text(name), text(" => "), text(self.text(target))]);
        }
        let value = self.doc(self.field(n, "value").unwrap());
        concat([text(name), text(" = "), value])
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
            if self.field(n, "bound").is_some() {
                parts.push(concat([text(" + "), self.bounds(n)]));
            }
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
            if self.field(n, "bound").is_some() {
                parts.push(concat([text(" + "), self.bounds(n)]));
            }
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
