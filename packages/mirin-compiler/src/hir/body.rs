//! `body(def)` — the per-def **body** HIR (`planning/q3_typed_hir.md` §2).
//!
//! A function body lowered to a name-resolved HIR over **owner-relative** ids:
//! an [`Arena`](Body)-style `Vec<Expr>` indexed by [`ExprId`] and a `Vec` of
//! locals indexed by [`LocalId`], both reset to 0 per body (RA's `Body`/`ExprId`
//! shape). A body edit rebuilds only this def's arena and renumbers nothing else.
//!
//! This is where the old `resolve_file` **phase 2** (body name resolution) is
//! folded in: bare names and paths resolve inline against the def map's
//! [`resolve_in_scope`](crate::nameres::def_map::CrateDefMap::resolve_in_scope) /
//! [`resolve_path`](crate::nameres::def_map::CrateDefMap::resolve_path) plus a
//! local scope, and `var` declarations are split from their driving equations
//! (`HirVarDecl` + `HirEquation`). Method dispatch is **deferred** — a
//! [`ExprKind::MethodCall`] survives to `infer` (Q3d), which resolves it via the
//! receiver's type + the impl-method index.
//!
//! Depends on `sig_of(self)` for the value-param locals (so body `LocalId`s line
//! up with the signature) and the generic params (to lower `var x: T`
//! annotations). Never reads another def's body.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::base::parser;
use crate::hir::sig::{lower_type_expr, sig_of};
use crate::hir::types::{
    ConstArg, ConstOp, Domain, GenericParam, LocalId, TermKind, Type, ValueKind,
};
use crate::nameres::def_map::{CrateDefMap, ModuleId, crate_def_map};
use crate::nameres::ids::{DefId, DefKind, Namespace};
use crate::syntax::ast_id;

/// Index into a [`Body`]'s expression arena. Owner-relative; reset per body.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, salsa::Update)]
pub struct ExprId(u32);

/// How a local was introduced.
#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::Update)]
pub enum LocalKind {
    /// A value parameter (from the signature).
    Param,
    /// `let x = …` — sequential, forward-only scope, shadows.
    Let,
    /// `var x` — block-scoped (pre-scanned), participates in equations.
    Var,
    /// A `for` loop's elaboration-time binding (the enumerate index, or the
    /// element of a `range(n)` loop): the genvar. An indexed drive through
    /// it covers the WHOLE place (the loop spans every index).
    ForBound,
}

/// One local binding in a body's local arena.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct LocalData<'db> {
    pub name: String,
    pub kind: LocalKind,
    /// The declared type, if written (`var x: T`, or a param's type). `None` for
    /// `let`/`var` left to inference.
    pub declared_ty: Option<Type<'db>>,
}

/// A lowered expression. (Spans land with the diagnostics infra, Q6.)
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct Expr<'db> {
    pub kind: ExprKind<'db>,
}

/// A literal's written base — carried so emitted SV preserves it
/// (`0xFF` → `8'hFF`; planning/numeric_literals.md L6).
#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::Update)]
pub enum NumBase {
    Dec,
    Hex,
    Bin,
}

#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub enum ExprKind<'db> {
    /// An unresolved / not-yet-lowered expression. Keeps lowering total.
    Missing,
    /// A numeric literal.
    Number(i128, NumBase),
    /// `uint(6)::4` — a literal at an explicitly written type
    /// (planning/numeric_literals.md L4). The fit check is direct.
    TypedLiteral {
        value: i128,
        base: NumBase,
        ty: Type<'db>,
    },
    /// `[a, b, c]` — vector construction (planning/vectors.md).
    VecLit(Vec<ExprId>),
    /// `(a, b)` — tuple construction (planning/tuples.md). Arity ≥ 2.
    /// Projection reuses `Field` with a numeric name (`p.0`).
    TupleLit(Vec<ExprId>),
    /// `[e; N]` — repeat construction; the length is a const expression.
    VecRepeat { elem: ExprId, len: ConstArg<'db> },
    /// `v[i]` — single-element indexing (Vec → elem; bits → bool).
    Index { base: ExprId, index: ExprId },
    /// A boolean literal (`true` / `false`).
    Bool(bool),
    /// A resolved local (param / let / var).
    Local(LocalId),
    /// A resolved item reference (fn, constructor, builtin).
    Def(DefId<'db>),
    /// A call. Operators (`+`, `*`) lower here too (callee = the prelude op).
    /// Connection shapes are recorded **as written** — positional and named args,
    /// with out-connections (`=>`) explicit. Matching named→params and out-args
    /// to the callee's signature is `infer`/`directions`' job (they have the
    /// sig); the body never looks a callee up (`planning/q5_backend.md`).
    Call {
        callee: ExprId,
        /// Positional args, in source order.
        args: Vec<ConnArg>,
        /// Named-section args (`f{ name = v, name => target, name }`).
        named: Vec<NamedArg>,
    },
    /// Field access `recv.field`.
    Field { receiver: ExprId, field: String },
    /// `recv.method(args)` — dispatch deferred to `infer` (Q3d).
    MethodCall {
        receiver: ExprId,
        method: String,
        args: Vec<ConnArg>,
    },
    /// `Ctor { field = value, field => target, … }`. `ctor` is `None` if the name did not resolve.
    Record {
        ctor: Option<DefId<'db>>,
        fields: Vec<RecordField>,
    },
    /// `if cond { … } else { … }` — flattened to statement form later (Q5).
    If {
        cond: ExprId,
        then_branch: Block,
        else_branch: Block,
    },
    /// `when event { … }` — Mirin's registered-state primitive.
    When {
        event: ExprId,
        body: Block,
        /// `init VALUE when …` — power-on state for the produced register.
        init: Option<ExprId>,
    },
    /// A block in expression position.
    Block(Block),
}

/// A positional call argument. `out` distinguishes a value (`expr` flows *into*
/// the callee) from an out-connection `[out] => target` (`expr` is the caller
/// place the callee's `out` param/field flows *into*).
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct ConnArg {
    pub out: bool,
    pub expr: ExprId,
}

/// A named-section call argument: a name plus a [`ConnArg`]-style connection
/// (`name = value` / shorthand `name` → value; `name => target` → out-connection).
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct NamedArg {
    pub name: String,
    pub out: bool,
    pub expr: ExprId,
}

/// One field of a record constructor (`name = value` supplies the field;
/// `name => target` is an out-connection binding an opposite-direction port
/// field to a place, mirroring `NamedArg`'s `=>` form).
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct RecordField {
    pub name: String,
    pub out: bool,
    pub value: ExprId,
}

/// An inline-verilog fn body (`= verilog { … }`): raw text split at `${…}`
/// splices, resolved against the signature (`planning/inline_verilog.md`).
#[derive(Clone, PartialEq, Eq, Debug, Default, salsa::Update)]
pub struct VerilogTemplate<'db> {
    pub segments: Vec<VerilogSegment<'db>>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum VerilogSegment<'db> {
    /// Raw verilog text, emitted verbatim (bare `$` included).
    Text(String),
    /// `${p}` — a scalar value param; renders as its port's SV name.
    Param(LocalId),
    /// `${clk}` — a dom generic; renders as the clock port's name.
    Dom(u32),
    /// `${result}` — the return port's name.
    ResultPort,
    /// `${n + 1}` — a const expression over literals and Const-kind
    /// generics; renders as an SV constant expression.
    Const(ConstArg<'db>),
}

/// A block: a sequence of statements and an optional tail expression (its value).
#[derive(Clone, PartialEq, Eq, Debug, Default, salsa::Update)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<ExprId>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum Stmt {
    /// `let x = value;`
    Let { local: LocalId, value: ExprId },
    /// `var x;` declaration (its driving equation is a separate `Equation`).
    VarDecl { local: LocalId },
    /// A driving equation / connection: `lhs = rhs;` (or a `var x = e;` init).
    Equation { lhs: ExprId, rhs: ExprId },
    /// `return value;`
    Return { value: ExprId },
    /// A bare expression statement.
    Expr(ExprId),
    /// `for x in v { … }` — structural replication (planning/for_loops.md).
    /// `index` is bound for the `for i, x in v.enumerate()` form; the elem
    /// local is "let x = v[i]" per iteration.
    For {
        index: Option<LocalId>,
        elem: LocalId,
        iter: ExprId,
        body: Block,
    },
}

/// A body-resolution diagnostic. The [`Span`] is **def-relative** (a byte offset
/// from the start of the owning def) so it survives edits to other defs; the
/// renderer adds the def's current start to get an absolute location.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct BodyDiagnostic {
    pub span: Span,
    pub kind: BodyDiagnosticKind,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum BodyDiagnosticKind {
    /// A name reference that did not resolve to a local or a def.
    UnresolvedName {
        name: String,
    },
    /// An expression form not yet lowered (e.g. a named-argument instantiation
    /// call, deferred to a later slice).
    Unsupported {
        what: String,
    },
    /// The same name is declared as `var` more than once in one block.
    DuplicateVar {
        name: String,
    },
    /// A `var x` declared after a `let x` binding in the same block.
    VarAfterLet {
        name: String,
    },
    /// A type name in a `let`/`var` ascription that resolved to nothing.
    UnresolvedType {
        name: String,
    },
    ForEnumerateForm,
    /// A numeric literal that does not fit in 128 bits.
    NumberTooLarge {
        text: String,
    },
    /// An `in`/`out` prefix on a named argument that disagrees with its
    /// connector (`in` supplies a value with `=`; `out` receives with `=>`).
    DirectionPrefixMismatch {
        direction: String,
    },
}

impl BodyDiagnostic {
    /// A human-readable message (no location — the renderer adds that).
    pub fn message(&self) -> String {
        match &self.kind {
            BodyDiagnosticKind::UnresolvedName { name } => format!("undefined name `{name}`"),
            BodyDiagnosticKind::Unsupported { what } => format!("unsupported syntax: {what}"),
            BodyDiagnosticKind::UnresolvedType { name } => format!("cannot find type `{name}`"),
            BodyDiagnosticKind::ForEnumerateForm => {
                "`.enumerate()` loops bind `for (i, x)` — a 2-tuple whose first element names the index".to_owned()
            }
            BodyDiagnosticKind::NumberTooLarge { text } => {
                format!("numeric literal `{text}` does not fit in 128 bits")
            }
            BodyDiagnosticKind::DirectionPrefixMismatch { direction } => match direction.as_str() {
                "out" => {
                    "`out` argument must be an out-connection: `out name => target`".to_owned()
                }
                _ => "`in` argument supplies a value: `in name = value`, not `=>`".to_owned(),
            },
            BodyDiagnosticKind::DuplicateVar { name } => {
                format!("`{name}` is declared more than once as `var` in this block")
            }
            BodyDiagnosticKind::VarAfterLet { name } => {
                format!(
                    "cannot declare `var {name}` after a `let {name}` binding in the same block"
                )
            }
        }
    }
}

/// A function's lowered body: its locals (params first), its top-level block,
/// and the diagnostics produced while lowering it.
#[derive(Clone, PartialEq, Eq, Default, salsa::Update)]
pub struct Body<'db> {
    exprs: Vec<Expr<'db>>,
    /// Def-relative source span per expression (the body's source map). Parallel
    /// to `exprs`; let `infer`/`check` locate an expression for diagnostics.
    expr_spans: Vec<Span>,
    locals: Vec<LocalData<'db>>,
    /// Def-relative span per local (its declaration site), parallel to `locals`.
    local_spans: Vec<Span>,
    /// The first `param_count` locals are the value params (ids match `sig_of`).
    param_count: u32,
    block: Block,
    /// `Some` for an inline-verilog fn (`= verilog { … }`); the block is empty.
    verilog: Option<VerilogTemplate<'db>>,
    diagnostics: Vec<BodyDiagnostic>,
}

impl<'db> Body<'db> {
    pub fn expr(&self, id: ExprId) -> &Expr<'db> {
        &self.exprs[id.0 as usize]
    }

    /// The inline-verilog template, for a `= verilog { … }` fn.
    pub fn verilog(&self) -> Option<&VerilogTemplate<'db>> {
        self.verilog.as_ref()
    }

    /// The def-relative span of an expression (the renderer adds the def start).
    pub fn expr_span(&self, id: ExprId) -> Span {
        self.expr_spans[id.0 as usize]
    }

    /// The innermost expression whose def-relative span contains `offset` (also
    /// def-relative), i.e. the most specific expression at that point. Used by
    /// IDE position→entity lookups (go-to-definition, hover).
    pub fn expr_at(&self, offset: u32) -> Option<ExprId> {
        let mut best: Option<(ExprId, u32)> = None;
        for (i, span) in self.expr_spans.iter().enumerate() {
            if span.start <= offset && offset < span.end {
                let width = span.end - span.start;
                if best.is_none_or(|(_, w)| width < w) {
                    best = Some((ExprId(i as u32), width));
                }
            }
        }
        best.map(|(id, _)| id)
    }

    /// The def-relative span of a local's declaration.
    pub fn local_span(&self, id: LocalId) -> Span {
        self.local_spans[id.0 as usize]
    }

    /// All expressions in the body's arena (for whole-body walks like the
    /// direction check).
    pub fn exprs(&self) -> impl Iterator<Item = &Expr<'db>> {
        self.exprs.iter()
    }

    pub fn local(&self, id: LocalId) -> &LocalData<'db> {
        &self.locals[id.0 as usize]
    }

    pub fn locals(&self) -> &[LocalData<'db>] {
        &self.locals
    }

    pub fn param_count(&self) -> u32 {
        self.param_count
    }

    pub fn block(&self) -> &Block {
        &self.block
    }

    pub fn diagnostics(&self) -> &[BodyDiagnostic] {
        &self.diagnostics
    }
}

/// QUERY: a function/method's lowered body. Non-fn defs return an empty body.
#[salsa::tracked(returns(ref))]
pub fn body<'db>(db: &'db dyn salsa::Database, krate: SourceRoot, def: DefId<'db>) -> Body<'db> {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Body::default();
    };
    if !matches!(data.kind, DefKind::Fn | DefKind::Method) {
        return Body::default();
    }
    let module = data.module;
    let sig = sig_of(db, krate, def);

    let file = def.file(db);
    let source = file.text(db);
    let ast_ids = ast_id::ast_id_map(db, file);
    let Some((start, end)) = ast_ids.range_of(def.ast_id(db)) else {
        return Body::default();
    };
    let tree = parser::parse_text(source);
    let Some(node) = tree.root_node().descendant_for_byte_range(start, end) else {
        return Body::default();
    };
    let mut lowerer = BodyLowerer::new(map, module, start, &sig.generic_params, &sig.params);
    lowerer.record_param_spans(&node, source);

    // `= verilog { … }`: no HIR block — the raw text becomes a template,
    // splices resolved against the signature.
    if let Some(vb) = node.child_by_field_name("verilog_body") {
        let template = vb
            .child_by_field_name("content")
            .map(|c| lowerer.lower_verilog_template(&c, source))
            .unwrap_or_default();
        let mut body = lowerer.finish(Block::default());
        body.verilog = Some(template);
        return body;
    }

    let Some(block_node) = node.child_by_field_name("body") else {
        return Body::default();
    };
    let block = lowerer.lower_block(&block_node, source);
    lowerer.finish(block)
}

struct BodyLowerer<'a, 'db> {
    map: &'a CrateDefMap<'db>,
    module: ModuleId,
    /// The owning def's absolute start byte — subtracted to make spans
    /// def-relative (edit-stable across other defs).
    def_start: usize,
    generics: &'a [GenericParam],
    exprs: Vec<Expr<'db>>,
    expr_spans: Vec<Span>,
    locals: Vec<LocalData<'db>>,
    local_spans: Vec<Span>,
    param_count: u32,
    /// Lexical scopes (ribs): name → local. Inner scopes shadow outer.
    scopes: Vec<HashMap<String, LocalId>>,
    diagnostics: Vec<BodyDiagnostic>,
}

impl<'a, 'db> BodyLowerer<'a, 'db> {
    fn new(
        map: &'a CrateDefMap<'db>,
        module: ModuleId,
        def_start: usize,
        generics: &'a [GenericParam],
        params: &[super::sig::Param<'db>],
    ) -> Self {
        // Value-param locals come first; their ids match `sig_of`.
        let mut locals = Vec::new();
        let mut base = HashMap::new();
        for p in params {
            let id = LocalId(locals.len() as u32);
            base.insert(p.name.clone(), id);
            locals.push(LocalData {
                name: p.name.clone(),
                kind: LocalKind::Param,
                declared_ty: Some(p.ty.clone()),
            });
        }
        // A `dom clk` generic param is referenced in the body as a `Clock` value
        // (`clk.posedge()`), so it is in scope as a local. (Const/Type generics
        // appear only in type position.) These come *after* the value params so
        // those keep the ids `sig_of` assigned.
        for g in generics {
            if matches!(g.kind, crate::hir::types::TermKind::Domain(_)) {
                let id = LocalId(locals.len() as u32);
                base.insert(g.name.clone(), id);
                locals.push(LocalData {
                    name: g.name.clone(),
                    kind: LocalKind::Param,
                    declared_ty: Some(Type::Clock),
                });
            }
        }
        // Params/`dom` generics have no body declaration site → default span.
        let local_spans = vec![Span::default(); locals.len()];
        Self {
            map,
            module,
            def_start,
            generics,
            exprs: Vec::new(),
            expr_spans: Vec::new(),
            param_count: params.len() as u32,
            locals,
            local_spans,
            scopes: vec![base],
            diagnostics: Vec::new(),
        }
    }

    fn finish(self, block: Block) -> Body<'db> {
        Body {
            exprs: self.exprs,
            expr_spans: self.expr_spans,
            locals: self.locals,
            local_spans: self.local_spans,
            param_count: self.param_count,
            block,
            verilog: None,
            diagnostics: self.diagnostics,
        }
    }

    /// Split a verilog block's raw text at `${…}` splices and resolve each
    /// against the signature: a scalar value param, a dom generic, `result`,
    /// or a const expression over literals + Const-kind generics. Bare `$`
    /// (system tasks) passes through as text.
    fn lower_verilog_template(&mut self, content: &Node, source: &str) -> VerilogTemplate<'db> {
        let text = node_text(content, source);
        let base = content.start_byte();
        let mut segments = Vec::new();
        let mut rest = text.as_str();
        let mut offset = 0usize; // byte offset of `rest` within `text`
        while let Some(i) = rest.find("${") {
            if i > 0 {
                segments.push(VerilogSegment::Text(rest[..i].to_owned()));
            }
            let after = &rest[i + 2..];
            let Some(end) = after.find('}') else {
                self.diag_verilog(base, offset + i, offset + i + 2, "unterminated `${`");
                segments.push(VerilogSegment::Text(rest[i..].to_owned()));
                rest = "";
                break;
            };
            let inner = &after[..end];
            let span = (offset + i, offset + i + 2 + end + 1);
            let seg = self.resolve_splice(inner.trim(), base, span);
            segments.push(seg);
            offset += i + 2 + end + 1;
            rest = &rest[i + 2 + end + 1..];
        }
        if rest.is_empty() == false {
            segments.push(VerilogSegment::Text(rest.to_owned()));
        }
        VerilogTemplate { segments }
    }

    /// A `${…}` splice: single names resolve against the signature; anything
    /// else is the const micro-grammar (`lit | name | (e) | e+e | e-e | e*e`,
    /// names being Const-kind generics).
    fn resolve_splice(
        &mut self,
        inner: &str,
        base: usize,
        span: (usize, usize),
    ) -> VerilogSegment<'db> {
        let is_ident = inner
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if is_ident {
            if inner == "result" {
                return VerilogSegment::ResultPort;
            }
            // Dom generics first: they are also seeded as body locals (for
            // `posedge`), but here they mean the clock port.
            if let Some(i) = self
                .generics
                .iter()
                .position(|g| g.name == inner && matches!(g.kind, TermKind::Domain(_)))
            {
                return VerilogSegment::Dom(i as u32);
            }
            if let Some(local) = self.lookup_local(inner) {
                // Aggregate params flatten to per-field ports — a bare splice
                // has no single name to stand for.
                let aggregate = matches!(
                    self.locals[local.0 as usize].declared_ty,
                    Some(Type::Port { .. })
                        | Some(Type::Value {
                            kind: crate::hir::types::ValueKind::Struct { .. },
                            ..
                        })
                );
                if aggregate {
                    self.diag_verilog(
                        base,
                        span.0,
                        span.1,
                        "aggregate params flatten per-field and cannot be spliced",
                    );
                    return VerilogSegment::Text(inner.to_owned());
                }
                return VerilogSegment::Param(local);
            }
            // fall through: a Const-kind generic parses below
        }
        match parse_const_splice(inner, &|n| {
            self.generics
                .iter()
                .position(|g| g.name == n && g.kind == TermKind::Const)
                .map(|i| ConstArg::Param(i as u32))
        }) {
            Ok(c) => VerilogSegment::Const(c),
            Err(msg) => {
                self.diag_verilog(base, span.0, span.1, &msg);
                VerilogSegment::Text(inner.to_owned())
            }
        }
    }

    /// A diagnostic inside the raw verilog text, span def-relative.
    fn diag_verilog(&mut self, base: usize, start: usize, end: usize, msg: &str) {
        self.diagnostics.push(BodyDiagnostic {
            span: Span::new(
                (base + start).saturating_sub(self.def_start),
                (base + end).saturating_sub(self.def_start),
            ),
            kind: BodyDiagnosticKind::Unsupported {
                what: format!("verilog splice: {msg}"),
            },
        });
    }

    /// Point the param locals' spans at their name identifiers in the signature
    /// sections. Params have no *body* declaration site, so without this they
    /// keep the default span and go-to-definition can't target them.
    fn record_param_spans(&mut self, def_node: &Node, source: &str) {
        for field in ["parameters", "named_parameters"] {
            let Some(section) = def_node.child_by_field_name(field) else {
                continue;
            };
            let mut cursor = section.walk();
            for p in section.named_children(&mut cursor) {
                let Some(name_node) = p.child_by_field_name("name") else {
                    continue;
                };
                let name = name_node.utf8_text(source.as_bytes()).unwrap_or_default();
                // Only value params + `dom` generics are locals (in the base rib).
                if let Some(&id) = self.scopes[0].get(name) {
                    self.local_spans[id.0 as usize] = self.rel_span(&name_node);
                }
            }
        }
    }

    fn alloc(&mut self, kind: ExprKind<'db>) -> ExprId {
        let id = ExprId(self.exprs.len() as u32);
        self.exprs.push(Expr { kind });
        self.expr_spans.push(Span::default());
        id
    }

    /// Allocate an expression and record its source span (for exprs built
    /// outside [`Self::lower_expr`], which sets spans itself).
    fn alloc_spanned(&mut self, kind: ExprKind<'db>, node: &Node) -> ExprId {
        let id = self.alloc(kind);
        self.expr_spans[id.0 as usize] = self.rel_span(node);
        id
    }

    /// Surface a `TypeLowerer`'s unresolved-type records as body diagnostics.
    fn diag_unresolved_types(&mut self, unres: Vec<(String, usize, usize)>) {
        for (name, start, end) in unres {
            self.diagnostics.push(BodyDiagnostic {
                span: Span::new(
                    start.saturating_sub(self.def_start),
                    end.saturating_sub(self.def_start),
                ),
                kind: BodyDiagnosticKind::UnresolvedType { name },
            });
        }
    }

    /// A node's span, relative to the owning def's start.
    fn rel_span(&self, node: &Node) -> Span {
        Span::new(
            node.start_byte().saturating_sub(self.def_start),
            node.end_byte().saturating_sub(self.def_start),
        )
    }

    /// Record a diagnostic located at `node`.
    fn diag_at(&mut self, node: &Node, kind: BodyDiagnosticKind) {
        let span = self.rel_span(node);
        self.diagnostics.push(BodyDiagnostic { span, kind });
    }

    fn alloc_local(
        &mut self,
        name: &str,
        kind: LocalKind,
        declared_ty: Option<Type<'db>>,
        span: Span,
    ) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalData {
            name: name.to_owned(),
            kind,
            declared_ty,
        });
        self.local_spans.push(span);
        self.define(name, id);
        id
    }

    fn define(&mut self, name: &str, id: LocalId) {
        self.scopes.last_mut().unwrap().insert(name.to_owned(), id);
    }

    fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    // ----- blocks / statements -----

    fn lower_block(&mut self, node: &Node, source: &str) -> Block {
        self.scopes.push(HashMap::new());
        // Pre-scan: `var` bindings are visible throughout the block. A name
        // declared `var` twice in one block is a duplicate.
        let mut seen_vars: HashSet<String> = HashSet::new();
        let mut cursor = node.walk();
        for stmt in node
            .children(&mut cursor)
            .filter(|c| c.kind() == "statement")
        {
            if let Some(inner) = stmt.named_child(0)
                && inner.kind() == "var_statement"
            {
                for name in field_texts(&inner, "name", source) {
                    if !seen_vars.insert(name.clone()) {
                        self.diag_at(&inner, BodyDiagnosticKind::DuplicateVar { name });
                    }
                }
                self.prescan_vars(&inner, source);
            }
        }

        // Lower statements in order; a `var x` after a `let x` earlier in this
        // block is illegal (the `let` already bound the name).
        let mut lets: HashSet<String> = HashSet::new();
        let mut block = Block::default();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "statement" {
                if let Some(inner) = child.named_child(0) {
                    match inner.kind() {
                        "let_statement" => {
                            if let Some(pat) = inner.child_by_field_name("pattern") {
                                collect_pattern_names(&pat, source, &mut lets);
                            }
                        }
                        "var_statement" => {
                            for name in field_texts(&inner, "name", source) {
                                if lets.contains(&name) {
                                    self.diag_at(&inner, BodyDiagnosticKind::VarAfterLet { name });
                                }
                            }
                        }
                        _ => {}
                    }
                    self.lower_stmt(&inner, source, &mut block);
                }
            } else if child.is_named() && child.kind() != "comment" {
                // The block's tail expression (its value).
                block.tail = Some(self.lower_expr(&child, source));
            }
        }
        self.scopes.pop();
        block
    }

    fn prescan_vars(&mut self, node: &Node, source: &str) {
        // Names only: the declared type is lowered when `lower_stmt` reaches
        // the statement, so a width naming an earlier `let` resolves
        // (`let w = …; var count: uint(w);` — at prescan no lets exist yet).
        let declared_ty = None;
        // Span each var at its own name identifier, not the whole statement.
        let mut cursor = node.walk();
        for name_node in node.children_by_field_name("name", &mut cursor) {
            let name = name_node
                .utf8_text(source.as_bytes())
                .unwrap_or_default()
                .to_owned();
            let span = self.rel_span(&name_node);
            self.alloc_local(&name, LocalKind::Var, declared_ty.clone(), span);
        }
    }

    /// Bind a `let` pattern to a value: a bare name binds directly; a tuple
    /// pattern binds a synthetic local and recursively desugars each element
    /// to a `.N` projection let — there is no pattern IR (planning/tuples.md).
    fn bind_pattern(
        &mut self,
        pat: &Node,
        source: &str,
        declared_ty: Option<Type<'db>>,
        value: ExprId,
        stmts: &mut Vec<Stmt>,
    ) {
        let span = self.rel_span(pat);
        if pat.kind() == "tuple_pattern" {
            // Destructuring an existing local needs no synthetic copy —
            // project straight off it (an ascription still pins a synthetic
            // so the declared type has a carrier).
            if let ExprKind::Local(l) = self.exprs[value.0 as usize].kind
                && declared_ty.is_none()
            {
                self.destructure_into(pat, source, l, stmts);
                return;
            }
            let synth = format!("__pat{}", self.locals.len());
            let local = self.alloc_local(&synth, LocalKind::Let, declared_ty, span);
            stmts.push(Stmt::Let { local, value });
            self.destructure_into(pat, source, local, stmts);
            return;
        }
        let name = node_text(pat, source);
        let local = self.alloc_local(&name, LocalKind::Let, declared_ty, span);
        stmts.push(Stmt::Let { local, value });
    }

    /// Desugar a tuple pattern against an already-bound local: one projection
    /// let per element, recursing into nested patterns.
    fn destructure_into(
        &mut self,
        pat: &Node,
        source: &str,
        receiver: LocalId,
        stmts: &mut Vec<Stmt>,
    ) {
        let elems = pattern_children(pat);
        for (i, elem) in elems.iter().enumerate() {
            let recv = self.alloc(ExprKind::Local(receiver));
            let proj = self.alloc(ExprKind::Field {
                receiver: recv,
                field: i.to_string(),
            });
            self.bind_pattern(elem, source, None, proj, stmts);
        }
    }

    fn lower_stmt(&mut self, node: &Node, source: &str, block: &mut Block) {
        match node.kind() {
            "for_statement" => {
                let Some(pat) = node.child_by_field_name("pattern") else {
                    return;
                };
                // `.enumerate()` is a real method (typed in infer), but a
                // for-loop also RECOGNISES it so the index binder reuses the
                // genvar instead of materialising an index vector
                // (planning/tuples.md): `for (i, x) in v.enumerate()` makes
                // `i` the genvar and `x` the element of the receiver.
                let iter_raw = node
                    .child_by_field_name("iter")
                    .map(|i| self.lower_expr(&i, source))
                    .unwrap_or_else(|| self.alloc(ExprKind::Missing));
                let (iter, enumerated) = match &self.exprs[iter_raw.0 as usize].kind {
                    ExprKind::MethodCall {
                        receiver,
                        method,
                        args,
                    } if method == "enumerate" && args.is_empty() => (*receiver, true),
                    _ => (iter_raw, false),
                };
                // A `range(n)` loop's element IS the genvar — mark it so
                // indexed drives through it count as whole-place.
                let is_range = matches!(
                    &self.exprs[iter.0 as usize].kind,
                    ExprKind::Call { callee, .. }
                        if matches!(
                            self.exprs[callee.0 as usize].kind,
                            ExprKind::Def(d) if self
                                .map
                                .def_data(d)
                                .is_some_and(|dd| dd.name == "range")
                        )
                );
                self.scopes.push(HashMap::new());
                // Desugared element-pattern lets, prepended to the body.
                let mut pre: Vec<Stmt> = Vec::new();
                let (index, elem) = match pat.kind() {
                    "tuple_pattern" if enumerated => {
                        // `(i, elem-pattern)` — exactly two elements, the
                        // first a bare name (it becomes the genvar).
                        let kids = pattern_children(&pat);
                        let index_ok = kids.len() == 2 && kids[0].kind() == "identifier";
                        if !index_ok {
                            self.diag_at(node, BodyDiagnosticKind::ForEnumerateForm);
                        }
                        let span = self.rel_span(&pat);
                        let index = if index_ok {
                            let n = node_text(&kids[0], source);
                            let ispan = self.rel_span(&kids[0]);
                            Some(self.alloc_local(
                                &n,
                                LocalKind::ForBound,
                                Some(Type::Value {
                                    kind: ValueKind::Integer,
                                    domain: Domain::Const,
                                }),
                                ispan,
                            ))
                        } else {
                            None
                        };
                        let elem = match kids.get(1) {
                            Some(e) if e.kind() == "identifier" => {
                                let n = node_text(e, source);
                                let espan = self.rel_span(e);
                                self.alloc_local(&n, LocalKind::Let, None, espan)
                            }
                            Some(e) => {
                                let synth = format!("__pat{}", self.locals.len());
                                let local = self.alloc_local(&synth, LocalKind::Let, None, span);
                                self.destructure_into(e, source, local, &mut pre);
                                local
                            }
                            None => {
                                let synth = format!("__pat{}", self.locals.len());
                                self.alloc_local(&synth, LocalKind::Let, None, span)
                            }
                        };
                        (index, elem)
                    }
                    // `for (a, b) in pairs` — the element is a tuple;
                    // destructure it at the top of the body.
                    "tuple_pattern" => {
                        let span = self.rel_span(&pat);
                        let synth = format!("__pat{}", self.locals.len());
                        let local = self.alloc_local(&synth, LocalKind::Let, None, span);
                        self.destructure_into(&pat, source, local, &mut pre);
                        (None, local)
                    }
                    _ => {
                        // A bare name. `for x in v.enumerate()` drops the
                        // index on the floor — require the tuple binder.
                        if enumerated {
                            self.diag_at(node, BodyDiagnosticKind::ForEnumerateForm);
                        }
                        let elem_kind = if is_range {
                            LocalKind::ForBound
                        } else {
                            LocalKind::Let
                        };
                        let name = node_text(&pat, source);
                        let span = self.rel_span(&pat);
                        (None, self.alloc_local(&name, elem_kind, None, span))
                    }
                };
                let mut body = node
                    .child_by_field_name("body")
                    .map(|b| self.lower_block(&b, source))
                    .unwrap_or_default();
                body.stmts.splice(0..0, pre);
                self.scopes.pop();
                block.stmts.push(Stmt::For {
                    index,
                    elem,
                    iter,
                    body,
                });
            }
            "let_statement" => {
                let value = self.lower_field_expr(node, "value", source);
                let lookup = |n: &str| self.lookup_local(n);
                let mut unres = Vec::new();
                let declared_ty = node.child_by_field_name("type").map(|t| {
                    lower_type_expr(
                        self.map,
                        self.module,
                        self.generics,
                        Some(&lookup),
                        &t,
                        source,
                        Some(&mut unres),
                    )
                });
                self.diag_unresolved_types(unres);
                if let Some(pat) = node.child_by_field_name("pattern") {
                    self.bind_pattern(&pat, source, declared_ty, value, &mut block.stmts);
                }
            }
            "var_statement" => {
                // Locals were pre-scanned (names only); lower the declared
                // type here, where every preceding `let` is in scope, and
                // patch it onto the pre-scanned locals.
                let lookup = |n: &str| self.lookup_local(n);
                let mut unres = Vec::new();
                let declared_ty = node.child_by_field_name("type").map(|t| {
                    lower_type_expr(
                        self.map,
                        self.module,
                        self.generics,
                        Some(&lookup),
                        &t,
                        source,
                        Some(&mut unres),
                    )
                });
                self.diag_unresolved_types(unres);
                let names = field_texts(node, "name", source);
                for name in &names {
                    if let Some(local) = self.lookup_local(name) {
                        self.locals[local.0 as usize].declared_ty = declared_ty.clone();
                        block.stmts.push(Stmt::VarDecl { local });
                    }
                }
                if let Some(value) = node.child_by_field_name("value") {
                    let rhs = self.lower_expr(&value, source);
                    if let Some(first) = names.first().and_then(|n| self.lookup_local(n)) {
                        let lhs = self.alloc(ExprKind::Local(first));
                        block.stmts.push(Stmt::Equation { lhs, rhs });
                    }
                }
            }
            "assignment_statement" => {
                let lhs = self.lower_field_expr(node, "left", source);
                let rhs = self.lower_field_expr(node, "right", source);
                block.stmts.push(Stmt::Equation { lhs, rhs });
            }
            "return_statement" => {
                let value = self.lower_field_expr(node, "value", source);
                block.stmts.push(Stmt::Return { value });
            }
            "expression_statement" => {
                if let Some(e) = node.named_child(0) {
                    let id = self.lower_expr(&e, source);
                    block.stmts.push(Stmt::Expr(id));
                }
            }
            _ => {}
        }
    }

    // ----- expressions -----

    fn lower_field_expr(&mut self, node: &Node, field: &str, source: &str) -> ExprId {
        match node.child_by_field_name(field) {
            Some(e) => self.lower_expr(&e, source),
            None => self.alloc(ExprKind::Missing),
        }
    }

    fn lower_expr(&mut self, node: &Node, source: &str) -> ExprId {
        let id = match node.kind() {
            // Unwrap the `expression` / parenthesised wrappers.
            "expression" | "parenthesized_expression" => match node.named_child(0) {
                Some(inner) => self.lower_expr(&inner, source),
                None => self.alloc(ExprKind::Missing),
            },
            "number" => {
                let text = node_text(node, source);
                let (v, base) = match parse_number(&text) {
                    Some(vb) => vb,
                    None => {
                        // The grammar admits only well-formed digit runs, so
                        // the only failure is overflow — diagnose rather than
                        // silently compiling the literal to 0.
                        self.diag_at(node, BodyDiagnosticKind::NumberTooLarge { text });
                        (0, NumBase::Dec)
                    }
                };
                self.alloc(ExprKind::Number(v, base))
            }
            "vec_literal" => {
                // `[e; N]` repeat or `[a, b, c]` list.
                if let Some(elem) = node.child_by_field_name("elem") {
                    let elem = self.lower_expr(&elem, source);
                    let len = node
                        .child_by_field_name("len")
                        .map(|l| self.lower_len_const(&l, source))
                        .unwrap_or(ConstArg::Deferred);
                    return self.alloc(ExprKind::VecRepeat { elem, len });
                }
                let mut cursor = node.walk();
                let elems: Vec<ExprId> = node
                    .children(&mut cursor)
                    .filter(|c| c.kind() == "expression")
                    .map(|c| self.lower_expr(&c, source))
                    .collect();
                self.alloc(ExprKind::VecLit(elems))
            }
            "tuple_expression" => {
                let mut cursor = node.walk();
                let elems: Vec<ExprId> = node
                    .children(&mut cursor)
                    .filter(|c| c.kind() == "expression")
                    .map(|c| self.lower_expr(&c, source))
                    .collect();
                self.alloc(ExprKind::TupleLit(elems))
            }
            "typed_literal" => {
                let (value, base) = node
                    .child_by_field_name("value")
                    .and_then(|v| parse_number(&node_text(&v, source)))
                    .unwrap_or((0, NumBase::Dec));
                let lookup = |n: &str| self.lookup_local(n);
                let mut unres = Vec::new();
                let ty = node
                    .child_by_field_name("type")
                    .map(|t| {
                        lower_type_expr(
                            self.map,
                            self.module,
                            self.generics,
                            Some(&lookup),
                            &t,
                            source,
                            Some(&mut unres),
                        )
                    })
                    .unwrap_or(Type::Error);
                self.diag_unresolved_types(unres);
                self.alloc(ExprKind::TypedLiteral { value, base, ty })
            }
            "unary_expression" => {
                // `-x` desugars to the prelude `Neg` trait's method
                // (planning/numeric_literals.md L5) — EXCEPT applied to a
                // literal, where it constant-folds into a negative literal
                // value (`let x: sint(4) = -8;` must fit-check -8, not 8 —
                // the -128i8 case; the LEXER still has no negative literals).
                let operand = self.lower_field_expr(node, "operand", source);
                if let ExprKind::Number(v, base) = self.exprs[operand.0 as usize].kind {
                    return self.alloc(ExprKind::Number(-v, base));
                }
                self.alloc(ExprKind::MethodCall {
                    receiver: operand,
                    method: "neg".to_owned(),
                    args: Vec::new(),
                })
            }
            "path_expression" => {
                let kind = self.lower_path(node, source);
                self.alloc(kind)
            }
            "binary_expression" => self.lower_binary(node, source),
            "postfix_expression" => self.lower_postfix(node, source),
            "record_constructor_expression" => self.lower_record(node, source),
            "if_expression" => {
                let cond = self.lower_field_expr(node, "condition", source);
                let then_branch = self.lower_block_field(node, "then_branch", source);
                let else_branch = self.lower_block_field(node, "else_branch", source);
                self.alloc(ExprKind::If {
                    cond,
                    then_branch,
                    else_branch,
                })
            }
            "when_expression" => {
                let event = self.lower_field_expr(node, "event", source);
                let body = self.lower_block_field(node, "body", source);
                let init = node
                    .child_by_field_name("init")
                    .map(|i| self.lower_expr(&i, source));
                self.alloc(ExprKind::When { event, body, init })
            }
            "block" => {
                let b = self.lower_block(node, source);
                self.alloc(ExprKind::Block(b))
            }
            other => {
                self.diag_at(
                    node,
                    BodyDiagnosticKind::Unsupported {
                        what: other.to_owned(),
                    },
                );
                self.alloc(ExprKind::Missing)
            }
        };
        // Record this expression's source span (the body source map).
        self.expr_spans[id.0 as usize] = self.rel_span(node);
        id
    }

    fn lower_block_field(&mut self, node: &Node, field: &str, source: &str) -> Block {
        match node.child_by_field_name(field) {
            Some(b) => self.lower_block(&b, source),
            None => Block::default(),
        }
    }

    /// Resolve a name reference: a local first (lexical scope), then an item
    /// through the def map (a 1-segment name via `resolve_in_scope`, a multi-seg
    /// path via `resolve_path`).
    fn lower_path(&mut self, node: &Node, source: &str) -> ExprKind<'db> {
        let segments = path_segments(node, source);
        if segments.len() == 1 {
            let name = &segments[0];
            // `true` / `false` are boolean literals (the grammar parses them as
            // bare identifiers). A user binding of the same name still shadows.
            if (name == "true" || name == "false") && self.lookup_local(name).is_none() {
                return ExprKind::Bool(name == "true");
            }
            if let Some(local) = self.lookup_local(name) {
                return ExprKind::Local(local);
            }
            if let Some(def) = self
                .map
                .resolve_in_scope(self.module, name, Namespace::Item)
            {
                return ExprKind::Def(def);
            }
        } else {
            let refs: Vec<&str> = segments.iter().map(String::as_str).collect();
            if let Some(def) = self.map.resolve_path(&refs, self.module, Namespace::Item) {
                return ExprKind::Def(def);
            }
        }
        self.diag_at(
            node,
            BodyDiagnosticKind::UnresolvedName {
                name: segments.join("::"),
            },
        );
        ExprKind::Missing
    }

    /// The `[e; N]` repeat length: a literal, a Const-kind generic, or a
    /// const local — the same leaves the width fragment allows.
    fn lower_len_const(&mut self, node: &Node, source: &str) -> ConstArg<'db> {
        match node.kind() {
            "expression" | "parenthesized_expression" => {
                let mut cursor = node.walk();
                match node.children(&mut cursor).find(|c| c.is_named()) {
                    Some(inner) => self.lower_len_const(&inner, source),
                    None => ConstArg::Deferred,
                }
            }
            "number" => parse_number(&node_text(node, source))
                .map(|(v, _)| ConstArg::Lit(v))
                .unwrap_or(ConstArg::Deferred),
            "path_expression" | "identifier" => {
                let name = node_text(node, source);
                let name = name.rsplit("::").next().unwrap_or(&name).to_owned();
                if let Some(i) = self
                    .generics
                    .iter()
                    .position(|g| g.name == name && g.kind == TermKind::Const)
                {
                    return ConstArg::Param(i as u32);
                }
                match self.lookup_local(&name) {
                    Some(l) => ConstArg::Local(l),
                    None => ConstArg::Deferred,
                }
            }
            _ => ConstArg::Deferred,
        }
    }

    fn lower_binary(&mut self, node: &Node, source: &str) -> ExprId {
        let lhs = self.lower_field_expr(node, "left", source);
        let rhs = self.lower_field_expr(node, "right", source);
        let op = field_text(node, "operator", source);
        // Operators desugar to the prelude operator traits' methods
        // (`a + b` → `a.add(b)`; planning/traits.md T5) and dispatch through
        // the ordinary trait machinery.
        let method = match op.as_str() {
            "+" => "add",
            "-" => "sub",
            "*" => "mul",
            "==" => "eq",
            "<" => "lt",
            _ => {
                self.diag_at(node, BodyDiagnosticKind::UnresolvedName { name: op });
                return self.alloc(ExprKind::Missing);
            }
        };
        self.alloc(ExprKind::MethodCall {
            receiver: lhs,
            method: method.to_owned(),
            args: vec![ConnArg {
                out: false,
                expr: rhs,
            }],
        })
    }

    fn lower_postfix(&mut self, node: &Node, source: &str) -> ExprId {
        let Some(receiver) = node.child_by_field_name("receiver") else {
            return self.alloc(ExprKind::Missing);
        };
        let mut cur = self.lower_expr(&receiver, source);
        // Operations are the named children after the receiver, in order.
        let mut cursor = node.walk();
        let ops: Vec<Node> = node
            .children(&mut cursor)
            .filter(|c| c.is_named() && c.id() != receiver.id() && c.kind() != "comment")
            .collect();
        let mut i = 0;
        while i < ops.len() {
            let op = ops[i];
            match op.kind() {
                "index_access" => {
                    let index = self.lower_field_expr(&op, "index", source);
                    cur = self.alloc(ExprKind::Index { base: cur, index });
                    i += 1;
                }
                "field_access" => {
                    let field = field_text(&op, "field", source);
                    if i + 1 < ops.len() && ops[i + 1].kind() == "argument_list" {
                        let args = self.lower_arg_list(&ops[i + 1], source);
                        cur = self.alloc(ExprKind::MethodCall {
                            receiver: cur,
                            method: field,
                            args,
                        });
                        i += 2;
                    } else {
                        cur = self.alloc(ExprKind::Field {
                            receiver: cur,
                            field,
                        });
                        i += 1;
                    }
                }
                "argument_list" => {
                    let args = self.lower_arg_list(&op, source);
                    cur = self.alloc(ExprKind::Call {
                        callee: cur,
                        args,
                        named: Vec::new(),
                    });
                    i += 1;
                }
                // A named-argument section, optionally followed by a positional
                // `( … )` section — a module-instantiation / connection call.
                "named_argument_list" => {
                    let named = self.lower_named_args(&op, source);
                    let (args, advance) =
                        if i + 1 < ops.len() && ops[i + 1].kind() == "argument_list" {
                            (self.lower_arg_list(&ops[i + 1], source), 2)
                        } else {
                            (Vec::new(), 1)
                        };
                    cur = self.alloc(ExprKind::Call {
                        callee: cur,
                        args,
                        named,
                    });
                    i += advance;
                }
                _ => i += 1,
            }
        }
        cur
    }

    fn lower_arg_list(&mut self, node: &Node, source: &str) -> Vec<ConnArg> {
        let mut cursor = node.walk();
        let mut args = Vec::new();
        for arg in node.children(&mut cursor).filter(|c| c.is_named()) {
            match arg.kind() {
                "expression" => args.push(ConnArg {
                    out: false,
                    expr: self.lower_expr(&arg, source),
                }),
                // Positional out-arg `[out] => target`.
                "out_argument" => {
                    if let Some(target) = arg.child_by_field_name("target") {
                        let name = node_text(&target, source);
                        args.push(ConnArg {
                            out: true,
                            expr: self.lower_place(&name, &target),
                        });
                    }
                }
                _ => {}
            }
        }
        args
    }

    /// Lower a `named_argument_list` (`{ name = v, name => target, name }`).
    fn lower_named_args(&mut self, node: &Node, source: &str) -> Vec<NamedArg> {
        let mut cursor = node.walk();
        let mut out = Vec::new();
        for a in node
            .children(&mut cursor)
            .filter(|c| c.kind() == "named_or_shorthand_argument")
        {
            let name = field_text(&a, "name", source);
            // The optional `in`/`out` prefix is redundant with the connector
            // (`=` supplies, `=>` receives) — it must agree when written.
            let direction = a
                .child_by_field_name("direction")
                .map(|d| node_text(&d, source));
            if let Some(value) = a.child_by_field_name("value") {
                // `name = value`
                if direction.as_deref() == Some("out") {
                    self.diag_at(
                        &a,
                        BodyDiagnosticKind::DirectionPrefixMismatch {
                            direction: "out".to_owned(),
                        },
                    );
                }
                out.push(NamedArg {
                    name,
                    out: false,
                    expr: self.lower_expr(&value, source),
                });
            } else if let Some(target) = a.child_by_field_name("target") {
                // `name => target` — an out-connection.
                if direction.as_deref() == Some("in") {
                    self.diag_at(
                        &a,
                        BodyDiagnosticKind::DirectionPrefixMismatch {
                            direction: "in".to_owned(),
                        },
                    );
                }
                let tname = node_text(&target, source);
                out.push(NamedArg {
                    name,
                    out: true,
                    expr: self.lower_place(&tname, &target),
                });
            } else {
                // shorthand `name` — pass the local of the same name as the value.
                let expr = self.lower_name_value(&name, &a);
                out.push(NamedArg {
                    name,
                    out: false,
                    expr,
                });
            }
        }
        out
    }

    /// A name used as a value (shorthand arg): a local, else a def. `node` is the
    /// argument's CST node, for locating an unresolved-name diagnostic.
    fn lower_name_value(&mut self, name: &str, node: &Node) -> ExprId {
        if let Some(local) = self.lookup_local(name) {
            return self.alloc_spanned(ExprKind::Local(local), node);
        }
        if let Some(def) = self
            .map
            .resolve_in_scope(self.module, name, Namespace::Item)
        {
            return self.alloc_spanned(ExprKind::Def(def), node);
        }
        self.diag_at(
            node,
            BodyDiagnosticKind::UnresolvedName {
                name: name.to_owned(),
            },
        );
        self.alloc(ExprKind::Missing)
    }

    /// An out-connection *target* place. An existing local is reused; a fresh
    /// name introduces an implicit `var` (forward-only), mirroring the old
    /// `ImplicitVar`. `node` is the target's CST node (for its span).
    fn lower_place(&mut self, name: &str, node: &Node) -> ExprId {
        let span = self.rel_span(node);
        let local = self
            .lookup_local(name)
            .unwrap_or_else(|| self.alloc_local(name, LocalKind::Var, None, span));
        self.alloc_spanned(ExprKind::Local(local), node)
    }

    fn lower_record(&mut self, node: &Node, source: &str) -> ExprId {
        let ctor_name = field_text(node, "constructor", source);
        let ctor = self
            .map
            .resolve_in_scope(self.module, &ctor_name, Namespace::Item);
        if ctor.is_none() {
            self.diag_at(node, BodyDiagnosticKind::UnresolvedName { name: ctor_name });
        }
        let mut fields = Vec::new();
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for f in body
                .children(&mut cursor)
                .filter(|c| c.kind() == "record_field_value")
            {
                if let Some(target) = f.child_by_field_name("target") {
                    // `name => target` — an out-connection field: the
                    // constructed port's field drives the target place
                    // (NamedArg's `=>`, record-literal flavour).
                    let tname = node_text(&target, source);
                    fields.push(RecordField {
                        name: field_text(&f, "name", source),
                        out: true,
                        value: self.lower_place(&tname, &target),
                    });
                } else {
                    let value = self.lower_field_expr(&f, "value", source);
                    fields.push(RecordField {
                        name: field_text(&f, "name", source),
                        out: false,
                        value,
                    });
                }
            }
        }
        self.alloc(ExprKind::Record { ctor, fields })
    }
}

/// Recursive-descent parser for the `${…}` const fragment:
/// `expr := term (('+'|'-') term)* ; term := factor ('*' factor)* ;
/// factor := integer | name | '(' expr ')'` — names resolved by `lookup`
/// (Const-kind generics). Returns a rendered-later `ConstArg` tree.
fn parse_const_splice<'db>(
    src: &str,
    lookup: &dyn Fn(&str) -> Option<ConstArg<'db>>,
) -> Result<ConstArg<'db>, String> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn peek(&mut self) -> Option<u8> {
            self.ws();
            self.s.get(self.i).copied()
        }
    }
    fn factor<'db>(
        p: &mut P,
        lookup: &dyn Fn(&str) -> Option<ConstArg<'db>>,
    ) -> Result<ConstArg<'db>, String> {
        match p.peek() {
            Some(b'(') => {
                p.i += 1;
                let e = expr(p, lookup)?;
                if p.peek() != Some(b')') {
                    return Err("expected `)`".to_owned());
                }
                p.i += 1;
                Ok(e)
            }
            Some(c) if c.is_ascii_digit() => {
                let start = p.i;
                while p.i < p.s.len() && p.s[p.i].is_ascii_digit() {
                    p.i += 1;
                }
                std::str::from_utf8(&p.s[start..p.i])
                    .ok()
                    .and_then(|t| t.parse::<i128>().ok())
                    .map(ConstArg::Lit)
                    .ok_or_else(|| "bad integer".to_owned())
            }
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                let start = p.i;
                while p.i < p.s.len() && (p.s[p.i].is_ascii_alphanumeric() || p.s[p.i] == b'_') {
                    p.i += 1;
                }
                let name = std::str::from_utf8(&p.s[start..p.i]).unwrap_or("");
                lookup(name).ok_or_else(|| {
                    format!("`{name}` is not a param, dom generic, or const generic")
                })
            }
            _ => Err("expected an integer, name, or `(`".to_owned()),
        }
    }
    fn term<'db>(
        p: &mut P,
        lookup: &dyn Fn(&str) -> Option<ConstArg<'db>>,
    ) -> Result<ConstArg<'db>, String> {
        let mut a = factor(p, lookup)?;
        while p.peek() == Some(b'*') {
            p.i += 1;
            let b = factor(p, lookup)?;
            a = ConstArg::Op(ConstOp::Mul, Box::new(a), Box::new(b));
        }
        Ok(a)
    }
    fn expr<'db>(
        p: &mut P,
        lookup: &dyn Fn(&str) -> Option<ConstArg<'db>>,
    ) -> Result<ConstArg<'db>, String> {
        let mut a = term(p, lookup)?;
        loop {
            match p.peek() {
                Some(b'+') => {
                    p.i += 1;
                    let b = term(p, lookup)?;
                    a = ConstArg::Op(ConstOp::Add, Box::new(a), Box::new(b));
                }
                Some(b'-') => {
                    p.i += 1;
                    let b = term(p, lookup)?;
                    a = ConstArg::Op(ConstOp::Sub, Box::new(a), Box::new(b));
                }
                _ => break,
            }
        }
        Ok(a)
    }
    let mut p = P {
        s: src.as_bytes(),
        i: 0,
    };
    let e = expr(&mut p, lookup)?;
    if p.peek().is_some() {
        return Err("trailing characters in splice".to_owned());
    }
    Ok(e)
}

// ----- CST helpers -----

/// The identifier segments of a `path_expression`.
fn path_segments(node: &Node, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "identifier")
        .map(|c| node_text(&c, source))
        .collect()
}

/// All children under `field` (for `commaSep1(field("name", …))`).
/// Every name bound by a pattern, recursively.
fn collect_pattern_names(pat: &Node, source: &str, out: &mut HashSet<String>) {
    if pat.kind() == "identifier" {
        out.insert(node_text(pat, source));
        return;
    }
    for child in pattern_children(pat) {
        collect_pattern_names(&child, source, out);
    }
}

/// A tuple pattern's element patterns (names and nested tuples), in order.
fn pattern_children<'t>(pat: &Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = pat.walk();
    pat.children(&mut cursor)
        .filter(|c| matches!(c.kind(), "identifier" | "tuple_pattern"))
        .collect()
}

fn field_texts(node: &Node, field: &str, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..node.child_count() {
        if node.field_name_for_child(i as u32) == Some(field)
            && let Some(c) = node.child(i)
        {
            out.push(node_text(&c, source));
        }
    }
    out
}

fn field_text(node: &Node, field: &str, source: &str) -> String {
    node.child_by_field_name(field)
        .map(|n| node_text(&n, source))
        .unwrap_or_default()
}

fn node_text(node: &Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;
    use crate::base::vfs::Vfs;

    fn load(db: &mut RootDatabase, vfs: &mut Vfs, text: &str) -> SourceRoot {
        vfs.set_file_text(db, "t.mrn", text);
        vfs.source_root(db, "t.mrn")
    }

    fn body_of<'db>(db: &'db RootDatabase, krate: SourceRoot, name: &str) -> &'db Body<'db> {
        let map = crate_def_map(db, krate);
        let def = map
            .resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("def");
        body(db, krate, def)
    }

    #[test]
    fn a_duplicate_var_is_a_diagnostic() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f () -> uint(8) { var x; var x; x = 0; return x; }",
        );
        let b = body_of(&db, krate, "f");
        assert!(
            b.diagnostics().iter().any(
                |d| matches!(&d.kind, BodyDiagnosticKind::DuplicateVar { name } if name == "x")
            ),
            "{:?}",
            b.diagnostics()
        );
    }

    #[test]
    fn a_var_after_a_let_of_the_same_name_is_a_diagnostic() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8)) -> uint(8) { let acc = a; var acc; acc = a; return acc; }",
        );
        let b = body_of(&db, krate, "f");
        assert!(
            b.diagnostics().iter().any(
                |d| matches!(&d.kind, BodyDiagnosticKind::VarAfterLet { name } if name == "acc")
            ),
            "{:?}",
            b.diagnostics()
        );
    }

    #[test]
    fn lets_vars_equations_and_name_resolution() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f (a: uint(8)) -> uint(8) { let b = a; var c; c = b; return c; }",
        );
        let b = body_of(&db, krate, "f");

        assert_eq!(b.param_count(), 1);
        // The block: let, var-decl, equation, return.
        let stmts = &b.block().stmts;
        assert_eq!(stmts.len(), 4);
        assert!(matches!(stmts[0], Stmt::Let { .. }));
        assert!(matches!(stmts[1], Stmt::VarDecl { .. }));
        assert!(matches!(stmts[2], Stmt::Equation { .. }));
        assert!(matches!(stmts[3], Stmt::Return { .. }));

        // `let b = a` — the value resolves to the param local `a` (local 0).
        let Stmt::Let { value, .. } = stmts[0] else {
            unreachable!()
        };
        assert!(matches!(b.expr(value).kind, ExprKind::Local(LocalId(0))));
        assert_eq!(b.local(LocalId(0)).kind, LocalKind::Param);
        // A `var` and a `let` were introduced.
        assert!(
            b.locals()
                .iter()
                .any(|l| l.kind == LocalKind::Var && l.name == "c")
        );
        assert!(
            b.locals()
                .iter()
                .any(|l| l.kind == LocalKind::Let && l.name == "b")
        );
        assert!(b.diagnostics().is_empty());
    }

    #[test]
    fn binary_desugars_to_the_operator_trait_method() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn g (a: uint(8)) -> uint(8) { return a + a; }",
        );
        let b = body_of(&db, krate, "g");
        let Stmt::Return { value } = b.block().stmts[0] else {
            unreachable!()
        };
        let ExprKind::MethodCall {
            receiver,
            method,
            args,
        } = &b.expr(value).kind
        else {
            panic!("expected the operator-trait method call");
        };
        assert_eq!(method, "add");
        assert!(matches!(b.expr(*receiver).kind, ExprKind::Local(_)));
        assert_eq!(args.len(), 1);
        assert!(!args[0].out && matches!(b.expr(args[0].expr).kind, ExprKind::Local(_)));
    }

    #[test]
    fn a_call_resolves_its_callee_def() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn callee () -> uint(8) { return 0; }\nfn caller () -> uint(8) { return callee(); }",
        );
        let b = body_of(&db, krate, "caller");
        let Stmt::Return { value } = b.block().stmts[0] else {
            unreachable!()
        };
        let ExprKind::Call { callee, args, .. } = &b.expr(value).kind else {
            panic!("expected a call");
        };
        assert!(matches!(b.expr(*callee).kind, ExprKind::Def(_)));
        assert!(args.is_empty());
    }

    #[test]
    fn named_args_and_out_connections_lower() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        // A call with a named section: `a = x` (value) and `b => y` (out-conn).
        let krate = load(
            &mut db,
            &mut vfs,
            "fn snk { in a: uint(8), out b: uint(8) } () { b = a; }\nfn top (x: uint(8), out y: uint(8)) { snk{a = x, b => y}(); }",
        );
        let b = body_of(&db, krate, "top");
        let Stmt::Expr(call) = b.block().stmts[0] else {
            panic!("expected an expression statement")
        };
        let ExprKind::Call { named, args, .. } = &b.expr(call).kind else {
            panic!("expected a call");
        };
        assert!(args.is_empty(), "no positional args");
        assert_eq!(named.len(), 2);
        // `a = x` is a value connection; `b => y` is an out-connection.
        assert_eq!(named[0].name, "a");
        assert!(!named[0].out);
        assert_eq!(named[1].name, "b");
        assert!(named[1].out);
        // No Unsupported diagnostic — the connection forms lower now.
        assert!(
            !b.diagnostics()
                .iter()
                .any(|d| matches!(&d.kind, BodyDiagnosticKind::Unsupported { .. })),
            "{:?}",
            b.diagnostics()
        );
    }

    #[test]
    fn an_unresolved_name_is_a_diagnostic_and_lowers_to_missing() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(&mut db, &mut vfs, "fn h () -> uint(8) { return zzz; }");
        let b = body_of(&db, krate, "h");
        let Stmt::Return { value } = b.block().stmts[0] else {
            unreachable!()
        };
        assert!(matches!(b.expr(value).kind, ExprKind::Missing));
        assert!(b.diagnostics().iter().any(
            |d| matches!(&d.kind, BodyDiagnosticKind::UnresolvedName { name } if name == "zzz")
        ));
    }

    #[test]
    fn method_calls_are_deferred() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn m (a: uint(8)) -> uint(8) { return a.posedge(); }",
        );
        let b = body_of(&db, krate, "m");
        let Stmt::Return { value } = b.block().stmts[0] else {
            unreachable!()
        };
        let ExprKind::MethodCall {
            receiver,
            method,
            args,
        } = &b.expr(value).kind
        else {
            panic!("expected a method call");
        };
        assert_eq!(method, "posedge");
        assert!(matches!(b.expr(*receiver).kind, ExprKind::Local(_)));
        assert!(args.is_empty());
        // Method dispatch is deferred to infer — no diagnostic at body time.
        assert!(b.diagnostics().is_empty());
    }

    #[test]
    fn record_constructor_resolves_its_ctor() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Packet = packet { valid: bool }\nfn mk () -> uint(8) { let p = packet { valid = 1 }; return 0; }",
        );
        let b = body_of(&db, krate, "mk");
        let Stmt::Let { value, .. } = b.block().stmts[0] else {
            unreachable!()
        };
        let ExprKind::Record { ctor, fields } = &b.expr(value).kind else {
            panic!("expected a record constructor");
        };
        assert!(ctor.is_some(), "the `packet` ctor resolves");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "valid");
    }

    #[test]
    fn if_and_when_lower_structurally() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn c (a: uint(8)) -> uint(8) { return if a { 1 } else { 0 }; }",
        );
        let b = body_of(&db, krate, "c");
        let Stmt::Return { value } = b.block().stmts[0] else {
            unreachable!()
        };
        let ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } = &b.expr(value).kind
        else {
            panic!("expected an if-expression");
        };
        assert!(matches!(b.expr(*cond).kind, ExprKind::Local(_)));
        assert!(then_branch.tail.is_some() && else_branch.tail.is_some());
    }

    #[test]
    fn body_lowering_only_for_functions() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(&mut db, &mut vfs, "struct S = s { a: uint(8) }");
        let map = crate_def_map(&db, krate);
        let s = map
            .resolve_in_scope(map.root(), "S", Namespace::Item)
            .unwrap();
        let b = body(&db, krate, s);
        assert!(b.block().stmts.is_empty() && b.locals().is_empty());
    }
}

/// Parse a numeric literal in any base, `_` separators stripped.
pub(crate) fn parse_number(text: &str) -> Option<(i128, NumBase)> {
    let (digits, radix, base) = match text.as_bytes() {
        [b'0', b'x' | b'X', rest @ ..] => (rest, 16, NumBase::Hex),
        [b'0', b'b' | b'B', rest @ ..] => (rest, 2, NumBase::Bin),
        _ => (text.as_bytes(), 10, NumBase::Dec),
    };
    let cleaned: String = digits
        .iter()
        .map(|b| *b as char)
        .filter(|c| *c != '_')
        .collect();
    i128::from_str_radix(&cleaned, radix)
        .ok()
        .map(|v| (v, base))
}
