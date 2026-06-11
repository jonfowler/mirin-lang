//! `sig_of(def)` — the **signature** query (`planning/q3_typed_hir.md` §2).
//!
//! A def's signature: its generic parameters, its value parameters (with
//! owner-relative [`LocalId`]s), its return type, and — for structs/ports — its
//! field types. A pure function of the def's *syntactic signature* + the crate's
//! name resolution: it lowers signature **types** transiently from the CST
//! (re-parsing, like `item_tree`), resolving type paths through
//! [`crate_def_map`]. Bodies are never touched, so editing a body leaves
//! `sig_of` value-equal — the signature/body firewall (`query_engine.md` §3.1).
//!
//! Keyed on `(SourceRoot, DefId)`: the `SourceRoot` gives the def map (type
//! paths resolve crate-relative); the `DefId` gives the file + `FileAstId` to
//! find the signature node.
//!
//! **Q3b scope:** `fn`/`method` signatures (generic params, value params, return
//! type) and struct/port signatures (generic params + field types, with port
//! field direction). Synthesised prelude-fn signatures land with `infer` (Q3d);
//! arithmetic widths and const/domain generic *args* passed by name are refined
//! in Q3d/Q4.

use std::cell::RefCell;

use tree_sitter::Node;

use crate::base::db::SourceRoot;
use crate::base::diagnostics::Span;
use crate::base::parser;
use crate::hir::types::{
    ConstArg, Direction, Domain, DomainSort, Folder, GenericArgs, GenericParam, LIFTED_DOM,
    LocalId, Term, TermKind, Type, ValueKind, super_fold_type,
};
use crate::nameres::def_map::{CrateDefMap, ModuleId, crate_def_map};
use crate::nameres::ids::{DefId, DefKind, Namespace};
use crate::syntax::ast_id;

/// A def's lowered signature. Which fields are populated depends on the def kind:
/// fns fill `params`/`return_type`; structs/ports fill `fields`; all may have
/// `generic_params`.
#[derive(Clone, PartialEq, Eq, Default, salsa::Update)]
pub struct Signature<'db> {
    pub generic_params: Vec<GenericParam>,
    pub params: Vec<Param<'db>>,
    pub return_type: Option<Type<'db>>,
    pub fields: Vec<Field<'db>>,
    /// Signature-level diagnostics (def-relative spans, like body diagnostics).
    pub diagnostics: Vec<SigDiagnostic>,
}

/// A signature-lowering diagnostic. The [`Span`] is def-relative.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct SigDiagnostic {
    pub span: Span,
    pub kind: SigDiagnosticKind,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum SigDiagnosticKind {
    /// In explicit mode (the signature introduces a `dom` or uses `@`), every
    /// parameter and the return type must carry a domain annotation
    /// (`domain_checking_redux.md`: explicit-mode annotation requirement).
    MissingDomainAnnotation,
    /// A type name that resolved to nothing (or to a non-type item).
    UnresolvedType { name: String },
}

impl SigDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            SigDiagnosticKind::MissingDomainAnnotation => {
                "missing `@domain` annotation: this signature declares domains explicitly, \
                 so every parameter and the return type must be annotated (or `@const`)"
                    .to_owned()
            }
            SigDiagnosticKind::UnresolvedType { name } => {
                format!("cannot find type `{name}`")
            }
        }
    }
}

/// A value parameter: its name, owner-relative local, type, and (for directed
/// params) direction. `self` is marked and carries no resolved structural type
/// yet (the receiver type is filled by method handling in Q3d).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct Param<'db> {
    pub name: String,
    pub local: LocalId,
    pub ty: Type<'db>,
    pub direction: Option<Direction>,
    pub is_self: bool,
    /// `true` if declared in the `{ … }` named section — call sites match named
    /// args to these, positional args to the rest (in declared order).
    pub from_named_section: bool,
    /// The raw source of a `= default` value, if any (`high`, `0`) — a call that
    /// omits this param wires the default at the instance.
    pub default: Option<String>,
}

/// A struct/port field: its name, type, and (ports only) direction.
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct Field<'db> {
    pub name: String,
    pub ty: Type<'db>,
    pub direction: Option<Direction>,
}

/// QUERY: a def's signature.
#[salsa::tracked(returns(ref))]
pub fn sig_of<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    def: DefId<'db>,
) -> Signature<'db> {
    let map = crate_def_map(db, krate);
    let Some(data) = map.def_data(def) else {
        return Signature::default();
    };
    let module = data.module;
    let kind = data.kind;

    // Find the signature's CST node by the def's stable byte range.
    let file = def.file(db);
    let source = file.text(db);
    let ast_ids = ast_id::ast_id_map(db, file);
    let Some((start, end)) = ast_ids.range_of(def.ast_id(db)) else {
        return Signature::default(); // synthetic (prelude) def — no CST node
    };
    let tree = parser::parse_text(source);
    let Some(node) = tree.root_node().descendant_for_byte_range(start, end) else {
        return Signature::default();
    };

    match kind {
        DefKind::Fn | DefKind::Method => lower_fn_sig(map, module, &node, source),
        DefKind::Struct => lower_adt_sig(map, module, &node, source, false),
        DefKind::Port => lower_adt_sig(map, module, &node, source, true),
        // Ctor/BuiltinType/Impl/Mod have no signature lowered here.
        _ => Signature::default(),
    }
}

/// Lower a struct/port: its generic params (from both param sections — only the
/// positional section for structs) and its field types. Ports carry per-field
/// direction; structs do not. Structs/ports have no value params (values come
/// from the fields), so only the generic classification of the sections is used.
fn lower_adt_sig<'db>(
    map: &CrateDefMap<'db>,
    module: ModuleId,
    node: &Node,
    source: &str,
    is_port: bool,
) -> Signature<'db> {
    let sections: &[(&str, &str, bool)] = if is_port {
        &[
            ("named_parameters", "named_parameter", true),
            ("parameters", "parameter", false),
        ]
    } else {
        &[("parameters", "parameter", false)]
    };
    let mut generic_params = Vec::new();
    for (field, child_kind, named) in sections {
        for p in section_params(node, field, child_kind) {
            if let ParamClass::Generic(kind) = classify(&p, source) {
                generic_params.push(GenericParam {
                    name: param_name(&p, source),
                    kind,
                    from_named_section: *named,
                });
            }
        }
    }

    let lowerer = TypeLowerer {
        map,
        module,
        generics: &generic_params,
        locals: None,
        unresolved: RefCell::new(Vec::new()),
    };

    let field_kind = if is_port {
        "port_field"
    } else {
        "record_field_type"
    };
    let mut fields = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for f in body
            .children(&mut cursor)
            .filter(|c| c.kind() == field_kind)
        {
            let ty = f
                .child_by_field_name("type")
                .map(|t| lowerer.lower_type(&t, source))
                .unwrap_or(Type::Error);
            fields.push(Field {
                name: field_text(&f, "name", source),
                ty,
                direction: if is_port {
                    direction_of(&f, source)
                } else {
                    None
                },
            });
        }
    }

    let diagnostics = drain_unresolved(&lowerer, node);
    Signature {
        generic_params,
        params: Vec::new(),
        return_type: None,
        fields,
        diagnostics,
    }
}

/// Convert a lowerer's unresolved-type records to def-relative diagnostics.
fn drain_unresolved(lowerer: &TypeLowerer<'_, '_>, def_node: &Node) -> Vec<SigDiagnostic> {
    let def_start = def_node.start_byte();
    lowerer
        .unresolved
        .borrow_mut()
        .drain(..)
        .map(|(name, start, end)| SigDiagnostic {
            span: Span {
                start: start.saturating_sub(def_start) as u32,
                end: end.saturating_sub(def_start) as u32,
            },
            kind: SigDiagnosticKind::UnresolvedType { name },
        })
        .collect()
}

/// Lower a `function_definition` node's signature.
fn lower_fn_sig<'db>(
    map: &CrateDefMap<'db>,
    module: ModuleId,
    node: &Node,
    source: &str,
) -> Signature<'db> {
    // Pass 1: classify every parameter (named then positional) into generic
    // params vs value params, so type lowering can resolve `Param(i)` refs.
    let mut generic_params = Vec::new();
    let mut value_param_nodes: Vec<(Node, bool)> = Vec::new();
    for (field, child_kind, named) in [
        ("named_parameters", "named_parameter", true),
        ("parameters", "parameter", false),
    ] {
        for p in section_params(node, field, child_kind) {
            match classify(&p, source) {
                ParamClass::Generic(kind) => generic_params.push(GenericParam {
                    name: param_name(&p, source),
                    kind,
                    from_named_section: named,
                }),
                ParamClass::Value => value_param_nodes.push((p, named)),
            }
        }
    }

    let lowerer = TypeLowerer {
        map,
        module,
        generics: &generic_params,
        locals: None,
        unresolved: RefCell::new(Vec::new()),
    };

    // Pass 2: lower each value parameter's type, assigning owner-relative ids.
    let mut params = Vec::new();
    for (i, (p, named)) in value_param_nodes.iter().enumerate() {
        let name = param_name(p, source);
        let is_self = name == "self";
        let ty = if is_self {
            // The receiver's structural type is filled by method handling (Q3d).
            Type::Error
        } else {
            p.child_by_field_name("type")
                .map(|t| lowerer.lower_type(&t, source))
                .unwrap_or(Type::Error)
        };
        params.push(Param {
            name,
            local: LocalId(i as u32),
            ty,
            direction: direction_of(p, source),
            is_self,
            from_named_section: *named,
            default: p
                .child_by_field_name("default")
                .map(|n| node_text(&n, source)),
        });
    }

    let return_type = node
        .child_by_field_name("return_type")
        .map(|t| lowerer.lower_type(&t, source));
    // Drain now: `lowerer` borrows `generic_params`, which the lifting branch
    // below moves.
    let unresolved_diags = drain_unresolved(&lowerer, node);

    // Domain mode (`domain_checking_redux.md`): a signature that introduces a
    // `dom` generic or writes any `@` is EXPLICIT — every value param and the
    // return type must carry a domain annotation. Anything else is PURE and is
    // lifted: one implicit `__Dom` generic (appended LAST, so user `Param(i)`
    // indices are untouched), stamped over every unannotated domain slot.
    let explicit = generic_params
        .iter()
        .any(|g| matches!(g.kind, TermKind::Domain(_)))
        || has_domain_annotation(node);
    let mut generic_params = generic_params;
    let mut params = params;
    let mut return_type = return_type;
    let mut diagnostics = Vec::new();
    if explicit {
        let def_start = node.start_byte();
        let rel = |n: &Node| Span {
            start: (n.start_byte() - def_start) as u32,
            end: (n.end_byte() - def_start) as u32,
        };
        for (p, _) in &value_param_nodes {
            let name = param_name(p, source);
            if name == "self" {
                continue; // the receiver's domain is its own annotation
            }
            let annotated = p.child_by_field_name("type").is_some_and(type_has_domain);
            if !annotated {
                diagnostics.push(SigDiagnostic {
                    span: rel(p),
                    kind: SigDiagnosticKind::MissingDomainAnnotation,
                });
            }
        }
        if let Some(rt) = node.child_by_field_name("return_type")
            && !type_has_domain(rt)
        {
            diagnostics.push(SigDiagnostic {
                span: rel(&rt),
                kind: SigDiagnosticKind::MissingDomainAnnotation,
            });
        }
    } else {
        let dom_index = generic_params.len() as u32;
        generic_params.push(GenericParam {
            name: LIFTED_DOM.to_owned(),
            kind: TermKind::Domain(DomainSort::Domain),
            from_named_section: true,
        });
        let mut lift = LiftDomains { dom_index };
        for p in &mut params {
            p.ty = lift.fold_type(&p.ty);
        }
        return_type = return_type.map(|t| lift.fold_type(&t));
    }

    diagnostics.extend(unresolved_diags);
    Signature {
        generic_params,
        params,
        return_type,
        fields: Vec::new(),
        diagnostics,
    }
}

/// Is this written type domain-annotated? Either an `@domain` suffix, or a
/// named-section type application (`DF{clk}(…)`) — a port/struct applied to
/// its domain arguments is fully domain-specified.
fn type_has_domain(t: Node) -> bool {
    t.child_by_field_name("domain").is_some()
        || t.children(&mut t.walk())
            .any(|c| c.kind() == "type_named_args")
}

/// Does any type written in this signature carry an `@domain` annotation?
/// (Checked syntactically: a `domain` field anywhere under the parameter
/// sections or the return type.)
fn has_domain_annotation(node: &Node) -> bool {
    fn subtree_has_domain(n: &Node) -> bool {
        if n.child_by_field_name("domain").is_some() {
            return true;
        }
        let mut cursor = n.walk();
        let children: Vec<Node> = n.children(&mut cursor).collect();
        children.iter().any(subtree_has_domain)
    }
    ["named_parameters", "parameters", "return_type"]
        .iter()
        .filter_map(|f| node.child_by_field_name(f))
        .any(|n| subtree_has_domain(&n))
}

/// Lifting: stamp the implicit `__Dom` over every unannotated domain slot.
struct LiftDomains {
    dom_index: u32,
}

impl<'db> Folder<'db> for LiftDomains {
    fn fold_type(&mut self, t: &Type<'db>) -> Type<'db> {
        super_fold_type(self, t)
    }

    fn fold_domain(&mut self, d: Domain) -> Domain {
        match d {
            Domain::Unspecified => Domain::Param(self.dom_index),
            other => other,
        }
    }
}

/// Lowers a `type_expression` / `return_type_expression` to a [`Type`], resolving
/// names against the prelude builtins, the enclosing def's generic params, and
/// the crate's name resolution.
struct TypeLowerer<'a, 'db> {
    map: &'a CrateDefMap<'db>,
    module: ModuleId,
    generics: &'a [GenericParam],
    /// Body-local resolver for widths in `let`/`var` ascriptions; `None` when
    /// lowering signatures (no locals in scope).
    locals: Option<&'a dyn Fn(&str) -> Option<LocalId>>,
    /// Type names that resolved to nothing — `(name, abs_start, abs_end)`,
    /// drained by the caller into its own diagnostic stream (sig or body).
    unresolved: RefCell<Vec<(String, usize, usize)>>,
}

impl<'db> TypeLowerer<'_, 'db> {
    fn lower_type(&self, node: &Node, source: &str) -> Type<'db> {
        let name = field_text(node, "name", source);
        let domain = self.lower_domain(node, source);

        // 1. Builtins, recognised by name (the old parser treats them as keywords).
        match name.as_str() {
            "uint" => {
                return Type::Value {
                    kind: ValueKind::UInt {
                        width: self.lower_width(node, source),
                    },
                    domain,
                };
            }
            "bool" => {
                return Type::Value {
                    kind: ValueKind::Bool,
                    domain,
                };
            }
            "Reset" => {
                return Type::Value {
                    kind: ValueKind::Reset,
                    domain,
                };
            }
            "Event" => {
                return Type::Value {
                    kind: ValueKind::Event,
                    domain,
                };
            }
            "integer" => {
                return Type::Value {
                    kind: ValueKind::Integer,
                    domain,
                };
            }
            "Clock" => return Type::Clock,
            _ => {}
        }

        // 2. A Type-kind generic parameter referenced by name (`data: A`).
        if let Some(i) = self.generic_index(&name, TermKind::Type) {
            return Type::Value {
                kind: ValueKind::Param(i),
                domain,
            };
        }

        // 3. A user struct / port resolved through the crate's name table.
        match self
            .map
            .resolve_in_scope(self.module, &name, Namespace::Item)
            .and_then(|d| self.map.def_data(d).map(|data| (d, data.kind)))
        {
            Some((def, DefKind::Struct)) => Type::Value {
                kind: ValueKind::Struct {
                    def,
                    args: self.lower_args(node, source),
                },
                domain,
            },
            Some((def, DefKind::Port)) => Type::Port {
                def,
                args: self.lower_args(node, source),
                domain,
            },
            _ => {
                let at = node.child_by_field_name("name").unwrap_or(*node);
                self.unresolved.borrow_mut().push((
                    name.clone(),
                    at.start_byte(),
                    at.end_byte(),
                ));
                Type::Error
            }
        }
    }

    /// The `@domain` annotation: a `dom`-kind generic param → `Param(i)`; absent
    /// or unrecognised → `Unspecified`. (Concrete `Clock(local)` domains arise in
    /// bodies, Q3c, not in signatures.)
    fn lower_domain(&self, node: &Node, source: &str) -> Domain {
        match node.child_by_field_name("domain") {
            None => Domain::Unspecified,
            Some(d) => {
                let name = node_text(&d, source);
                match self.generic_index(&name, TermKind::Domain(DomainSort::Clock)) {
                    Some(i) => Domain::Param(i),
                    None => Domain::Unspecified,
                }
            }
        }
    }

    /// The width inside `uint(W)`: a literal, a Const-kind generic ref, a body
    /// local (in body-lowered ascriptions), or deferred (anything else —
    /// arithmetic widths land in `const_eval`, Q4).
    fn lower_width(&self, node: &Node, source: &str) -> ConstArg {
        let Some(arg) = first_type_argument(node) else {
            return ConstArg::Deferred;
        };
        if arg.kind() == "number" {
            return node_text(&arg, source)
                .parse::<i128>()
                .map(ConstArg::Lit)
                .unwrap_or(ConstArg::Deferred);
        }
        // A bare identifier naming a Const-kind generic param, or (in a body
        // type ascription) a local in scope.
        let name = field_text(&arg, "name", source);
        if let Some(i) = self.generic_index(&name, TermKind::Const) {
            return ConstArg::Param(i);
        }
        if let Some(l) = self.locals.and_then(|f| f(&name)) {
            return ConstArg::Local(l);
        }
        ConstArg::Deferred
    }

    /// Generic args at a struct/port reference (`Bus(uint(8))`,
    /// `DF{clk}(uint(8))`). Named-section args lower first, then positional,
    /// matching the declared param order, so args align with
    /// `generic_params` by index when fully supplied.
    fn lower_args(&self, node: &Node, source: &str) -> GenericArgs<'db> {
        let mut args = Vec::new();
        // Named-section args (`DF{clk}`) come first: a def's generic_params
        // list the named section before the positional one, and args align
        // with params by index.
        let mut cursor = node.walk();
        let named: Vec<Node> = node
            .children(&mut cursor)
            .filter(|c| c.kind() == "type_named_args")
            .collect();
        for sec in named {
            self.lower_arg_section(&sec, source, &mut args);
        }
        if let Some(index) = type_index(node) {
            self.lower_arg_section(&index, source, &mut args);
        }
        GenericArgs(args)
    }

    fn lower_arg_section(&self, section: &Node, source: &str, args: &mut Vec<Term<'db>>) {
        let mut cursor = section.walk();
        for ta in section
            .children(&mut cursor)
            .filter(|c| c.kind() == "type_argument")
        {
            let mut tc = ta.walk();
            let Some(inner) = ta.children(&mut tc).find(|n| n.is_named()) else {
                continue;
            };
            args.push(self.lower_generic_arg(&inner, source));
        }
    }

    /// Lower one generic argument, kind-directed by what the name means in
    /// the *enclosing* def's environment: a number is a const; a bare name
    /// that names a `dom` generic is a domain argument (`DF{clk}` →
    /// `Domain::Param(i)`), a `param` generic a const argument; anything else
    /// lowers as a type. (The target def's own param kinds can't be consulted
    /// here — `sig_of(target)` from inside `sig_of(self)` would cycle on
    /// mutually-referencing types.)
    fn lower_generic_arg(&self, inner: &Node, source: &str) -> Term<'db> {
        if inner.kind() == "number" {
            let c = node_text(inner, source)
                .parse::<i128>()
                .map(ConstArg::Lit)
                .unwrap_or(ConstArg::Deferred);
            return Term::Const(c);
        }
        let mut cursor = inner.walk();
        let plain_name = inner.kind() == "type_expression"
            && inner.child_by_field_name("domain").is_none()
            && !inner
                .children(&mut cursor)
                .any(|c| matches!(c.kind(), "type_index" | "type_named_args"));
        if plain_name {
            let name = field_text(inner, "name", source);
            if let Some(i) = self
                .generics
                .iter()
                .position(|g| g.name == name && matches!(g.kind, TermKind::Domain(_)))
            {
                return Term::Domain(Domain::Param(i as u32));
            }
            if let Some(i) = self
                .generics
                .iter()
                .position(|g| g.name == name && matches!(g.kind, TermKind::Const))
            {
                return Term::Const(ConstArg::Param(i as u32));
            }
        }
        Term::Type(self.lower_type(inner, source))
    }

    fn generic_index(&self, name: &str, kind: TermKind) -> Option<u32> {
        self.generics
            .iter()
            .position(|g| g.name == name && g.kind == kind)
            .map(|i| i as u32)
    }
}

/// Lower a single `type_expression` node against a module + the enclosing def's
/// generic params. Shared with body lowering (Q3c), which lowers `let`/`var`
/// `x: T` annotations the same way `sig_of` lowers param/field types — plus a
/// `locals` resolver so a width can reference a body local (`uint(n)`).
pub(crate) fn lower_type_expr<'db>(
    map: &CrateDefMap<'db>,
    module: ModuleId,
    generics: &[GenericParam],
    locals: Option<&dyn Fn(&str) -> Option<LocalId>>,
    node: &Node,
    source: &str,
    unresolved_sink: Option<&mut Vec<(String, usize, usize)>>,
) -> Type<'db> {
    let lowerer = TypeLowerer {
        map,
        module,
        generics,
        locals,
        unresolved: RefCell::new(Vec::new()),
    };
    let ty = lowerer.lower_type(node, source);
    if let Some(sink) = unresolved_sink {
        sink.append(&mut lowerer.unresolved.borrow_mut());
    }
    ty
}

// ----- CST helpers -----

enum ParamClass {
    Generic(TermKind),
    Value,
}

/// Classify a parameter node as a generic param (by `dom`/`param` keyword or a
/// `: Type` annotation) or a value param.
fn classify(node: &Node, source: &str) -> ParamClass {
    if param_name(node, source) == "self" {
        return ParamClass::Value;
    }
    if field_text(node, "kind", source) == "dom" {
        return ParamClass::Generic(TermKind::Domain(DomainSort::Clock));
    }
    // A `: Type` annotation makes it a Type-kind generic — this wins over a
    // `param` keyword (`param A: Type` is type-generic, not const-generic).
    if let Some(ty) = node.child_by_field_name("type")
        && field_text(&ty, "name", source) == "Type"
    {
        return ParamClass::Generic(TermKind::Type);
    }
    // `param N: integer` (or bare `param`) — a Const-kind generic.
    if field_text(node, "kind", source) == "param" {
        return ParamClass::Generic(TermKind::Const);
    }
    ParamClass::Value
}

fn section_params<'a>(item: &Node<'a>, field: &str, child_kind: &str) -> Vec<Node<'a>> {
    let Some(section) = item.child_by_field_name(field) else {
        return Vec::new();
    };
    let mut cursor = section.walk();
    section
        .children(&mut cursor)
        .filter(|n| n.kind() == child_kind)
        .collect()
}

fn param_name(node: &Node, source: &str) -> String {
    field_text(node, "name", source)
}

fn direction_of(node: &Node, source: &str) -> Option<Direction> {
    match field_text(node, "direction", source).as_str() {
        "in" => Some(Direction::In),
        "out" => Some(Direction::Out),
        _ => None,
    }
}

/// The `type_index` (`( … )`) child of a type expression, if any.
fn type_index<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|c| c.kind() == "type_index")
}

fn first_type_argument<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let index = type_index(node)?;
    let mut cursor = index.walk();
    index
        .children(&mut cursor)
        .find(|c| c.kind() == "type_argument")
        .and_then(|arg| {
            let mut c = arg.walk();
            arg.children(&mut c).find(|n| n.is_named())
        })
        .or_else(|| {
            let mut c = index.walk();
            index.children(&mut c).find(|n| n.is_named())
        })
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

    /// Load one file as the crate root and return `(SourceRoot, def of `name`)`.
    fn fn_def<'db>(db: &'db RootDatabase, krate: SourceRoot, name: &str) -> DefId<'db> {
        let map = crate_def_map(db, krate);
        map.resolve_in_scope(map.root(), name, Namespace::Item)
            .expect("fn def")
    }

    fn load(db: &mut RootDatabase, vfs: &mut Vfs, text: &str) -> SourceRoot {
        vfs.set_file_text(db, "t.plr", text);
        vfs.source_root(db, "t.plr")
    }

    #[test]
    fn scalar_fn_signature() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn add (a: uint(8), b: uint(8)) -> uint(8) { return a; }",
        );
        let def = fn_def(&db, krate, "add");
        let sig = sig_of(&db, krate, def);

        // A pure signature is LIFTED: one implicit `__Dom` generic appended,
        // stamped over every unannotated domain slot.
        assert_eq!(sig.generic_params.len(), 1);
        assert!(sig.generic_params[0].is_lifted_dom());
        assert_eq!(sig.params.len(), 2);
        assert_eq!(sig.params[0].name, "a");
        assert_eq!(sig.params[0].local, LocalId(0));
        assert_eq!(sig.params[1].local, LocalId(1));
        assert!(matches!(
            sig.params[0].ty,
            Type::Value {
                kind: ValueKind::UInt {
                    width: ConstArg::Lit(8)
                },
                domain: Domain::Param(0)
            }
        ));
        assert!(matches!(
            sig.return_type,
            Some(Type::Value {
                kind: ValueKind::UInt {
                    width: ConstArg::Lit(8)
                },
                ..
            })
        ));
    }

    #[test]
    fn generic_fn_classifies_params_and_resolves_refs() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn f { dom clk: Clock, param N: integer, A: Type } (x: uint(N) @clk) -> uint(N) @clk { return x; }",
        );
        let def = fn_def(&db, krate, "f");
        let sig = sig_of(&db, krate, def);

        // Three generics, in named-section declaration order.
        assert_eq!(sig.generic_params.len(), 3);
        assert_eq!(sig.generic_params[0].name, "clk");
        assert_eq!(
            sig.generic_params[0].kind,
            TermKind::Domain(DomainSort::Clock)
        );
        assert_eq!(sig.generic_params[1].kind, TermKind::Const); // N
        assert_eq!(sig.generic_params[2].kind, TermKind::Type); // A
        assert!(sig.generic_params[0].from_named_section);

        // The value param `x: uint(N) @clk` references N (const #1) and clk (dom #0).
        assert_eq!(sig.params.len(), 1);
        assert!(matches!(
            sig.params[0].ty,
            Type::Value {
                kind: ValueKind::UInt {
                    width: ConstArg::Param(1)
                },
                domain: Domain::Param(0)
            }
        ));
        assert!(matches!(
            sig.return_type,
            Some(Type::Value {
                kind: ValueKind::UInt {
                    width: ConstArg::Param(1)
                },
                domain: Domain::Param(0)
            })
        ));
    }

    #[test]
    fn param_typed_by_a_user_struct_resolves_to_its_def() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Bus = bus { a: uint(8) }\nfn g (b: Bus) -> uint(8) { return 0; }",
        );
        let map = crate_def_map(&db, krate);
        let bus = map
            .resolve_in_scope(map.root(), "Bus", Namespace::Item)
            .unwrap();
        let def = map
            .resolve_in_scope(map.root(), "g", Namespace::Item)
            .unwrap();
        let sig = sig_of(&db, krate, def);

        match &sig.params[0].ty {
            Type::Value {
                kind: ValueKind::Struct { def, args },
                ..
            } => {
                assert!(*def == bus, "param type resolves to the Bus def");
                assert!(args.0.is_empty());
            }
            _ => panic!("expected a struct-typed param"),
        }
    }

    #[test]
    fn parametric_struct_reference_carries_type_args() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Bus(A: Type) = bus { a: uint(8) }\nfn h (b: Bus(uint(8))) -> uint(8) { return 0; }",
        );
        let def = fn_def(&db, krate, "h");
        let sig = sig_of(&db, krate, def);
        match &sig.params[0].ty {
            Type::Value {
                kind: ValueKind::Struct { args, .. },
                ..
            } => {
                assert_eq!(args.0.len(), 1);
                assert!(matches!(
                    &args.0[0],
                    Term::Type(Type::Value {
                        kind: ValueKind::UInt { .. },
                        ..
                    })
                ));
            }
            _ => panic!("expected a parametric struct param"),
        }
    }

    #[test]
    fn signature_is_stable_across_a_body_edit() {
        // A 'static projection so we can compare across the mutating edit.
        fn summary(sig: &Signature) -> (Vec<String>, usize, bool) {
            (
                sig.params.iter().map(|p| p.name.clone()).collect(),
                sig.generic_params.len(),
                sig.return_type.is_some(),
            )
        }
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn add (a: uint(8)) -> uint(8) { return a; }",
        );
        let before = {
            let def = fn_def(&db, krate, "add");
            summary(sig_of(&db, krate, def))
        };
        vfs.set_file_text(
            &mut db,
            "t.plr",
            "fn add (a: uint(8)) -> uint(8) { return a + a + a; }",
        );
        let after = {
            let def = fn_def(&db, krate, "add");
            summary(sig_of(&db, krate, def))
        };
        assert_eq!(before, after, "a body edit must not change the signature");
    }

    // ----- Q3b-2: struct / port fields -----

    #[test]
    fn struct_field_types() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Packet = packet { valid: bool, payload: uint(8) }",
        );
        let def = fn_def(&db, krate, "Packet");
        let sig = sig_of(&db, krate, def);
        assert!(sig.params.is_empty() && sig.return_type.is_none());
        assert_eq!(sig.fields.len(), 2);
        assert_eq!(sig.fields[0].name, "valid");
        assert!(matches!(
            sig.fields[0].ty,
            Type::Value {
                kind: ValueKind::Bool,
                ..
            }
        ));
        assert!(matches!(
            sig.fields[1].ty,
            Type::Value {
                kind: ValueKind::UInt {
                    width: ConstArg::Lit(8)
                },
                ..
            }
        ));
        // Struct fields carry no direction.
        assert!(sig.fields[0].direction.is_none());
    }

    #[test]
    fn parametric_struct_field_references_its_type_param() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "struct Bus(A: Type) = bus { valid: bool, data: A }",
        );
        let def = fn_def(&db, krate, "Bus");
        let sig = sig_of(&db, krate, def);
        assert_eq!(sig.generic_params.len(), 1);
        assert_eq!(sig.generic_params[0].kind, TermKind::Type);
        // `data: A` references the 0-th generic param in type position.
        assert!(matches!(
            sig.fields[1].ty,
            Type::Value {
                kind: ValueKind::Param(0),
                ..
            }
        ));
    }

    #[test]
    fn port_fields_carry_direction() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "port Stream8 = stream8 { out valid: bool, out data: uint(8), in ready: bool }",
        );
        let def = fn_def(&db, krate, "Stream8");
        let sig = sig_of(&db, krate, def);
        assert_eq!(sig.fields.len(), 3);
        assert_eq!(sig.fields[0].direction, Some(Direction::Out));
        assert_eq!(sig.fields[1].direction, Some(Direction::Out));
        assert_eq!(sig.fields[2].direction, Some(Direction::In));
    }

    #[test]
    fn port_generic_domain_and_type_params() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "port DF { dom clk: Clock } ( A: Type ) = df { in ready: bool @clk, out data: A @clk }",
        );
        let def = fn_def(&db, krate, "DF");
        let sig = sig_of(&db, krate, def);
        // clk (named, Domain) then A (positional, Type).
        assert_eq!(sig.generic_params.len(), 2);
        assert_eq!(
            sig.generic_params[0].kind,
            TermKind::Domain(DomainSort::Clock)
        );
        assert!(sig.generic_params[0].from_named_section);
        assert_eq!(sig.generic_params[1].kind, TermKind::Type);
        assert!(!sig.generic_params[1].from_named_section);
        // `in ready: bool @clk` — domain references the dom generic clk (#0).
        assert!(matches!(
            sig.fields[0].ty,
            Type::Value {
                kind: ValueKind::Bool,
                domain: Domain::Param(0)
            }
        ));
        assert_eq!(sig.fields[0].direction, Some(Direction::In));
        // `out data: A @clk` — A is type-param #1, domain is clk #0.
        assert!(matches!(
            sig.fields[1].ty,
            Type::Value {
                kind: ValueKind::Param(1),
                domain: Domain::Param(0)
            }
        ));
    }
}
