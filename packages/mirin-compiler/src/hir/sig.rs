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
    ConstArg, ConstOp, Direction, Domain, DomainSort, Folder, GenericArgs, GenericParam,
    LIFTED_DOM, LocalId, Predicate, Term, TermKind, TraitRef, Type, ValueKind, super_fold_type,
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
    /// The referrable result place(s): one `return` place for a normal return,
    /// the named part(s) for a `-> (name: T, …)` signature, empty for a unit fn
    /// (planning/return_variable.md).
    pub result_places: Vec<ResultPlace<'db>>,
    pub fields: Vec<Field<'db>>,
    /// Written bounds (`param T: Add + Bits`, `where T: Bits`) plus, on a
    /// trait method decl, the implicit `Self: Trait`. Instantiated into
    /// obligations at call sites; assumed inside the body (the param env).
    pub predicates: Vec<Predicate<'db>>,
    /// An impl's associated-const VALUE (`const width: integer = 2 * T::width;`),
    /// lowered in the impl's generic-prefix space. Only on `AssocConst` defs.
    pub const_value: Option<ConstArg<'db>>,
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
    /// (`domain_checking.md`: explicit-mode annotation requirement).
    MissingDomainAnnotation,
    /// A type name that resolved to nothing (or to a non-type item).
    UnresolvedType {
        name: String,
    },
    NotATrait {
        name: String,
    },
    UnknownWhereParam {
        name: String,
    },
    /// An aggregate's `@D` annotation conflicts with an element's own explicit
    /// domain — e.g. `Vec(2, uint(8) @b) @a` or `(uint(8) @a, uint(8) @b) @c`.
    /// A domain lives on the leaf; an aggregate `@D` may only FILL unspecified
    /// element slots, never override a conflicting one
    /// (planning/domain_checking.md).
    ConflictingDomain,
    /// An `impl` on a generic owner written without its type arguments
    /// (`impl {dom clk} Bus` on `struct Bus(A: Type)`). A generic owner must be
    /// applied — `impl {dom clk, A: Type} Bus(A)` — so the owner is a real type,
    /// not a bare constructor.
    GenericOwnerNotApplied {
        name: String,
        arity: usize,
    },
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
            SigDiagnosticKind::NotATrait { name } => {
                format!("`{name}` is not a trait")
            }
            SigDiagnosticKind::UnknownWhereParam { name } => {
                format!("`{name}` is not a type parameter of this signature")
            }
            SigDiagnosticKind::ConflictingDomain => {
                "domain annotation conflicts with an element's own domain: a domain lives on \
                 the leaf, and `@` on an aggregate may only fill unspecified element slots"
                    .to_owned()
            }
            SigDiagnosticKind::GenericOwnerNotApplied { name, arity } => {
                format!(
                    "generic type `{name}` must be applied here: write `{name}(…)` with its \
                     {arity} type argument(s), declared in the impl binder"
                )
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

/// A referrable result place — the `return` variable, or a named result/named
/// tuple part (planning/return_variable.md). `name` is the source binding
/// (`return` when unnamed); `sv_base` is the SystemVerilog port base its leaves
/// emit under (`result`, or `result__0`/`result__1`/… for tuple parts). Empty
/// for a unit fn (no return type).
#[derive(Clone, PartialEq, Eq, salsa::Update)]
pub struct ResultPlace<'db> {
    pub name: String,
    pub ty: Type<'db>,
    pub sv_base: String,
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
        // A trait impl's HEADER: binder generics, self type (in return_type),
        // binder-bound predicates. The solver's impl-candidate shape.
        DefKind::Impl => lower_impl_header(db, krate, map, module, &node, source),
        // An impl's associated const: its VALUE, lowered in the impl's
        // generic-prefix space (a trait's const DECL carries no value).
        DefKind::AssocConst => lower_assoc_const(db, krate, map, module, &node, source),
        // Ctor/BuiltinType/Mod have no signature lowered here.
        _ => Signature::default(),
    }
}

/// Lower an `impl {binders} Trait for SelfType` HEADER. Mirrors the generic
/// prefix `lower_fn_sig` builds for the impl's methods — the binding a header
/// match produces indexes the same positions as each method's leading
/// generics. `return_type` carries the self type.
fn lower_impl_header<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    map: &CrateDefMap<'db>,
    module: ModuleId,
    node: &Node,
    source: &str,
) -> Signature<'db> {
    // The self-type node: the `for` self type for a trait impl, otherwise the
    // impl subject itself (an inherent impl's `name` IS its self type, applied
    // if the owner is generic — `impl {A: Type} Bus(A)`). A generic owner is
    // applied explicitly, so its params come from the binder (no auto-binding).
    let self_type_node = node
        .child_by_field_name("self_type")
        .or_else(|| node.child_by_field_name("name"));
    let mut generic_params = Vec::new();
    let is_trait_name = |n: &str| {
        map.resolve_in_scope(module, n, Namespace::Item)
            .and_then(|d| map.def_data(d))
            .is_some_and(|d| d.kind == DefKind::Trait)
    };
    let mut generic_param_nodes: Vec<(usize, Node)> = Vec::new();
    for (field, child_kind) in [("named_parameters", "named_parameter")] {
        for p in section_params(node, field, child_kind) {
            if let ParamClass::Generic(kind) = classify(&p, source, &is_trait_name) {
                generic_params.push(GenericParam {
                    name: param_name(&p, source),
                    kind,
                    from_named_section: true,
                });
                generic_param_nodes.push((generic_params.len() - 1, p));
            }
        }
    }
    let lowerer = TypeLowerer {
        map,
        module,
        generics: &generic_params,
        locals: None,
        unresolved: RefCell::new(Vec::new()),
        self_ty: RefCell::new(None),
        assoc_self: RefCell::new(None),
        bounds: RefCell::new(Vec::new()),
    };
    let self_ty = self_type_node
        .map(|st| lowerer.lower_type(&st, source))
        .unwrap_or(Type::Error);
    // Binder bounds (`impl {param T: Bits} Bits for Pair(T)`) become the
    // impl's predicates — the solver's NESTED obligations on selection.
    let mut predicates = Vec::new();
    let mut pred_diags = Vec::new();
    let def_start = node.start_byte();
    for (i, p) in &generic_param_nodes {
        if generic_params[*i].kind != TermKind::Type {
            continue;
        }
        let mut push = |bname: String, at: &Node| match map
            .resolve_in_scope(module, &bname, Namespace::Item)
            .and_then(|d| map.def_data(d).map(|data| (d, data.kind)))
        {
            Some((t, DefKind::Trait)) => predicates.push(Predicate::Trait(TraitRef {
                trait_def: t,
                self_ty: Type::Value {
                    kind: ValueKind::Param(*i as u32),
                    domain: Domain::Unspecified,
                },
            })),
            _ => pred_diags.push(SigDiagnostic {
                span: Span {
                    start: (at.start_byte() - def_start) as u32,
                    end: (at.end_byte() - def_start) as u32,
                },
                kind: SigDiagnosticKind::NotATrait { name: bname },
            }),
        };
        if let Some(ty) = p.child_by_field_name("type") {
            let tname = field_text(&ty, "name", source);
            if tname != "Type" {
                push(tname, &ty);
            }
        }
        let mut c = p.walk();
        for b in p.children_by_field_name("bound", &mut c) {
            push(field_text(&b, "name", source), &b);
        }
    }
    let mut diagnostics = drain_unresolved(&lowerer, node);
    diagnostics.extend(pred_diags);
    // A generic owner must be APPLIED: `impl {dom clk, A: Type} Bus(A)`, never
    // bare `impl {dom clk} Bus`. Resolve the owner and compare its positional
    // (non-binder) arity against the args written on the self type.
    if let Some(st) = self_type_node
        && let Some(owner_def) = st
            .child_by_field_name("name")
            .map(|n| node_text(&n, source))
            .and_then(|name| map.resolve_in_scope(module, &name, Namespace::Item))
            .filter(|d| {
                matches!(
                    map.def_data(*d).map(|x| x.kind),
                    Some(DefKind::Struct | DefKind::Port)
                )
            })
    {
        let arity = sig_of(db, krate, owner_def)
            .generic_params
            .iter()
            .filter(|g| !g.from_named_section)
            .count();
        let written = type_index(&st).is_some_and(|idx| {
            idx.children(&mut idx.walk())
                .any(|c| c.kind() == "type_argument")
        });
        if arity > 0 && !written {
            let name = field_text(&st, "name", source);
            diagnostics.push(SigDiagnostic {
                span: Span {
                    start: (st.start_byte() - def_start) as u32,
                    end: (st.end_byte() - def_start) as u32,
                },
                kind: SigDiagnosticKind::GenericOwnerNotApplied { name, arity },
            });
        }
    }
    Signature {
        generic_params,
        params: Vec::new(),
        return_type: Some(self_ty),
        result_places: Vec::new(),
        fields: Vec::new(),
        predicates,
        const_value: None,
        diagnostics,
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
    let is_trait_name = |n: &str| {
        map.resolve_in_scope(module, n, Namespace::Item)
            .and_then(|d| map.def_data(d))
            .is_some_and(|d| d.kind == DefKind::Trait)
    };
    let mut generic_params = Vec::new();
    for (field, child_kind, named) in sections {
        for p in section_params(node, field, child_kind) {
            if let ParamClass::Generic(kind) = classify(&p, source, &is_trait_name) {
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
        self_ty: RefCell::new(None),
        assoc_self: RefCell::new(None),
        bounds: RefCell::new(Vec::new()),
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
        result_places: Vec::new(),
        fields,
        predicates: Vec::new(),
        const_value: None,
        diagnostics,
    }
}

/// Lower an associated const's def: for an IMPL const, the value expression
/// in the impl's generic-prefix space (`const width: integer = 2 * T::width;`);
/// a trait DECL const carries no value. The value is restricted to the const
/// fragment (literals, generic params, `+ - *`, assoc projections) — anything
/// else lowers `Deferred`.
fn lower_assoc_const<'db>(
    db: &'db dyn salsa::Database,
    krate: SourceRoot,
    map: &CrateDefMap<'db>,
    module: ModuleId,
    node: &Node,
    source: &str,
) -> Signature<'db> {
    let Some(impl_node) = enclosing_impl(node) else {
        // A trait's const DECL: generics = [Self].
        return Signature {
            generic_params: vec![GenericParam {
                name: "Self".to_owned(),
                kind: TermKind::Type,
                from_named_section: true,
            }],
            ..Signature::default()
        };
    };
    // The impl's generic prefix — same positions as the impl header's.
    let header = lower_impl_header(db, krate, map, module, &impl_node, source);
    let generic_params = header.generic_params.clone();
    let lowerer = TypeLowerer {
        map,
        module,
        generics: &generic_params,
        locals: None,
        unresolved: RefCell::new(Vec::new()),
        self_ty: RefCell::new(header.return_type.clone()),
        assoc_self: RefCell::new(None),
        bounds: RefCell::new(
            header
                .predicates
                .iter()
                .filter_map(|Predicate::Trait(tr)| match &tr.self_ty {
                    Type::Value {
                        kind: ValueKind::Param(i),
                        ..
                    } => Some((*i, tr.trait_def)),
                    _ => None,
                })
                .collect(),
        ),
    };
    let const_value = node
        .child_by_field_name("value")
        .map(|v| lower_const_value(&lowerer, &v, source));
    Signature {
        generic_params,
        const_value,
        ..Signature::default()
    }
}

/// Lower an ordinary EXPRESSION node into the const fragment (impl const
/// values parse as expressions, not const_expressions).
fn lower_const_value<'db>(
    lowerer: &TypeLowerer<'_, 'db>,
    node: &Node,
    source: &str,
) -> ConstArg<'db> {
    match node.kind() {
        "expression" | "parenthesized_expression" => {
            let mut cursor = node.walk();
            match node.children(&mut cursor).find(|c| c.is_named()) {
                Some(inner) => lower_const_value(lowerer, &inner, source),
                None => ConstArg::Deferred,
            }
        }
        "number" => node_text(node, source)
            .parse::<i128>()
            .map(ConstArg::Lit)
            .unwrap_or(ConstArg::Deferred),
        "binary_expression" => {
            let op = match field_text(node, "operator", source).as_str() {
                "+" => ConstOp::Add,
                "-" => ConstOp::Sub,
                "*" => ConstOp::Mul,
                _ => return ConstArg::Deferred,
            };
            let (Some(l), Some(r)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                return ConstArg::Deferred;
            };
            ConstArg::Op(
                op,
                Box::new(lower_const_value(lowerer, &l, source)),
                Box::new(lower_const_value(lowerer, &r, source)),
            )
        }
        "path_expression" => {
            let mut cursor = node.walk();
            let segs: Vec<String> = node
                .children_by_field_name("segment", &mut cursor)
                .map(|n| node_text(&n, source))
                .collect();
            match segs.as_slice() {
                [one] => lowerer.lower_const_name(one),
                [base, item] => lowerer.lower_const_path(base, item),
                _ => ConstArg::Deferred,
            }
        }
        "identifier" => lowerer.lower_const_name(&node_text(node, source)),
        _ => ConstArg::Deferred,
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
    // A method's generics lead with the impl block's declared generics
    // (`impl {dom clk: Clock, A: Type} Bus(A) { … }` — Rust's `impl<T>` shape;
    // a generic owner is APPLIED, its params declared in the binder), then the
    // fn's own.
    // A trait METHOD DECL (`fn add(self, other: Self) -> Self;` inside a
    // `trait`) gets an implicit leading Type-kind generic named `Self` —
    // rustc's Self-as-param-0. `Self` in type positions and the bare `self`
    // receiver both resolve to it.
    let in_trait = enclosing(node, "trait_definition").is_some();
    if in_trait {
        generic_params.push(GenericParam {
            name: "Self".to_owned(),
            kind: TermKind::Type,
            from_named_section: true,
        });
    }
    // The impl's self-type node: the `for` self type for a trait impl,
    // otherwise the inherent subject itself (applied if the owner is generic —
    // `impl {A: Type} Bus(A)`). A generic owner is applied explicitly, so its
    // params come from the binder (no auto-binding).
    let impl_self_type_node = enclosing_impl(node).and_then(|impl_node| {
        impl_node
            .child_by_field_name("self_type")
            .or_else(|| impl_node.child_by_field_name("name"))
    });
    // CST node per written generic param — the source of its bounds.
    let mut generic_param_nodes: Vec<(usize, Node)> = Vec::new();
    let is_trait_name = |n: &str| {
        map.resolve_in_scope(module, n, Namespace::Item)
            .and_then(|d| map.def_data(d))
            .is_some_and(|d| d.kind == DefKind::Trait)
    };
    if let Some(impl_node) = enclosing_impl(node) {
        for (field, child_kind, named) in [
            ("named_parameters", "named_parameter", true),
            ("parameters", "parameter", false),
        ] {
            for p in section_params(&impl_node, field, child_kind) {
                if let ParamClass::Generic(kind) = classify(&p, source, &is_trait_name) {
                    generic_params.push(GenericParam {
                        name: param_name(&p, source),
                        kind,
                        from_named_section: named,
                    });
                    generic_param_nodes.push((generic_params.len() - 1, p));
                }
            }
        }
    }
    for (field, child_kind, named) in [
        ("named_parameters", "named_parameter", true),
        ("parameters", "parameter", false),
    ] {
        for p in section_params(node, field, child_kind) {
            match classify(&p, source, &is_trait_name) {
                ParamClass::Generic(kind) => {
                    generic_params.push(GenericParam {
                        name: param_name(&p, source),
                        kind,
                        from_named_section: named,
                    });
                    generic_param_nodes.push((generic_params.len() - 1, p));
                }
                ParamClass::Value => value_param_nodes.push((p, named)),
            }
        }
    }

    // ----- predicates: written bounds + the trait decl's implicit Self bound -----
    let mut predicates: Vec<Predicate<'db>> = Vec::new();
    let mut pred_diags: Vec<SigDiagnostic> = Vec::new();
    let def_start = node.start_byte();
    let push_bound = |i: u32,
                      bname: &str,
                      at: &Node,
                      preds: &mut Vec<Predicate<'db>>,
                      diags: &mut Vec<SigDiagnostic>| {
        let resolved = map
            .resolve_in_scope(module, bname, Namespace::Item)
            .and_then(|d| map.def_data(d).map(|data| (d, data.kind)));
        match resolved {
            Some((t, DefKind::Trait)) => preds.push(Predicate::Trait(TraitRef {
                trait_def: t,
                self_ty: Type::Value {
                    kind: ValueKind::Param(i),
                    domain: Domain::Unspecified,
                },
            })),
            _ => diags.push(SigDiagnostic {
                span: Span {
                    start: (at.start_byte() - def_start) as u32,
                    end: (at.end_byte() - def_start) as u32,
                },
                kind: SigDiagnosticKind::NotATrait {
                    name: bname.to_owned(),
                },
            }),
        }
    };
    if in_trait
        && let Some(t) = enclosing(node, "trait_definition")
            .and_then(|t| t.child_by_field_name("name"))
            .map(|n| node_text(&n, source))
            .and_then(|n| map.resolve_in_scope(module, &n, Namespace::Item))
    {
        // Inside `trait Scale`, `Self: Scale` is assumed.
        predicates.push(Predicate::Trait(TraitRef {
            trait_def: t,
            self_ty: Type::Value {
                kind: ValueKind::Param(0),
                domain: Domain::Unspecified,
            },
        }));
    }
    for (i, p) in &generic_param_nodes {
        if generic_params[*i].kind != TermKind::Type {
            continue;
        }
        // The ascription itself, when it names a trait (`param T: Add`).
        if let Some(ty) = p.child_by_field_name("type") {
            let tname = field_text(&ty, "name", source);
            if tname != "Type" {
                push_bound(*i as u32, &tname, &ty, &mut predicates, &mut pred_diags);
            }
        }
        // `+ Bound` tails.
        let mut c = p.walk();
        for b in p.children_by_field_name("bound", &mut c) {
            let bname = field_text(&b, "name", source);
            push_bound(*i as u32, &bname, &b, &mut predicates, &mut pred_diags);
        }
    }
    if let Some(w) = node.child_by_field_name("where") {
        let mut c = w.walk();
        for pred in w
            .named_children(&mut c)
            .filter(|n| n.kind() == "where_predicate")
        {
            let pname = field_text(&pred, "name", source);
            let target = generic_params
                .iter()
                .position(|g| g.name == pname && g.kind == TermKind::Type);
            match target {
                Some(i) => {
                    let mut bc = pred.walk();
                    for b in pred.children_by_field_name("bound", &mut bc) {
                        let bname = field_text(&b, "name", source);
                        push_bound(i as u32, &bname, &b, &mut predicates, &mut pred_diags);
                    }
                }
                None => pred_diags.push(SigDiagnostic {
                    span: Span {
                        start: (pred.start_byte() - def_start) as u32,
                        end: (pred.end_byte() - def_start) as u32,
                    },
                    kind: SigDiagnosticKind::UnknownWhereParam { name: pname },
                }),
            }
        }
    }

    let lowerer = TypeLowerer {
        map,
        module,
        generics: &generic_params,
        locals: None,
        unresolved: RefCell::new(Vec::new()),
        self_ty: RefCell::new(None),
        assoc_self: RefCell::new(None),
        bounds: RefCell::new(Vec::new()),
    };
    // What `Self` (and the bare `self` receiver) mean in an impl method: the
    // impl's self type, lowered against the binder (`impl {A: Type} Bus(A)`
    // gives `Bus(A)`; a builtin self like `impl Bits for uint(8)` gives
    // `uint(8)`).
    let impl_self_base: Option<Type> =
        impl_self_type_node.map(|st| lowerer.lower_type(&st, source));
    lowerer.self_ty.replace(impl_self_base.clone());
    // Associated-const context: bare const names (`uint(width)`) resolve
    // against the enclosing trait (decl scope: Self = Param(0)) or the
    // enclosing trait impl (its self type); `T::width` projects through the
    // signature's own bounds.
    lowerer.bounds.replace(
        predicates
            .iter()
            .filter_map(|Predicate::Trait(tr)| match &tr.self_ty {
                Type::Value {
                    kind: ValueKind::Param(i),
                    ..
                } => Some((*i, tr.trait_def)),
                _ => None,
            })
            .collect(),
    );
    let assoc_self = if in_trait {
        enclosing(node, "trait_definition")
            .and_then(|t| t.child_by_field_name("name"))
            .map(|n| node_text(&n, source))
            .and_then(|n| map.resolve_in_scope(module, &n, Namespace::Item))
            .map(|t| {
                (
                    t,
                    Type::Value {
                        kind: ValueKind::Param(0),
                        domain: Domain::Unspecified,
                    },
                )
            })
    } else {
        enclosing_impl(node)
            .filter(|i| i.child_by_field_name("self_type").is_some())
            .and_then(|i| i.child_by_field_name("name"))
            // The trait name is the head ident of the impl's subject type expr.
            .and_then(|n| n.child_by_field_name("name"))
            .map(|n| node_text(&n, source))
            .and_then(|n| map.resolve_in_scope(module, &n, Namespace::Item))
            .zip(impl_self_base)
    };
    lowerer.assoc_self.replace(assoc_self);

    // Pass 2: lower each value parameter's type, assigning owner-relative ids.
    let mut params = Vec::new();
    for (i, (p, named)) in value_param_nodes.iter().enumerate() {
        let name = param_name(p, source);
        let is_self = name == "self";
        let ty = if is_self && in_trait {
            // A trait decl's receiver is `Self` — the implicit Param(0).
            Type::Value {
                kind: ValueKind::Param(0),
                domain: lowerer.lower_domain(p, source),
            }
        } else if is_self {
            // The receiver: the impl's self type, carrying self's own
            // `@domain` annotation (`self @clk`) so the receiver's domain
            // connects to the method's generics at the call site.
            let base = lowerer.self_ty.borrow().clone().unwrap_or(Type::Error);
            let domain = lowerer.lower_domain(p, source);
            match base {
                Type::Value { kind, .. } => Type::Value { kind, domain },
                Type::Port { def, args, .. } => Type::Port { def, args, domain },
                other => other,
            }
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

    let return_node = node.child_by_field_name("return_type");
    // Named result(s) (`-> (output: DF)`, `-> (sum: T, carry: U)`): capture the
    // names; the underlying type is the element (1) or a tuple (≥2). For a
    // normal return the name list is empty and the `return` place is synthesised
    // below (planning/return_variable.md).
    let result_names: Vec<String> = return_node
        .filter(|n| n.kind() == "named_return")
        .map(|n| named_result_names(&n, source))
        .unwrap_or_default();
    let return_type = return_node.map(|t| lower_return_type(&lowerer, &t, source));
    // Drain now: `lowerer` borrows `generic_params`, which the lifting branch
    // below moves.
    let unresolved_diags = drain_unresolved(&lowerer, node);

    // Domain mode (`domain_checking.md`): a signature that introduces a
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
            let Some(ty) = p.child_by_field_name("type") else {
                continue;
            };
            if !type_has_domain(ty, source) {
                diagnostics.push(SigDiagnostic {
                    span: rel(p),
                    kind: SigDiagnosticKind::MissingDomainAnnotation,
                });
            }
            if let Some(c) = domain_conflict(&ty, source, None) {
                diagnostics.push(SigDiagnostic {
                    span: rel(&c),
                    kind: SigDiagnosticKind::ConflictingDomain,
                });
            }
        }
        if let Some(rt) = node.child_by_field_name("return_type") {
            // A named return checks each named result's type; a normal return
            // checks the type node itself.
            let checked: Vec<Node> = if rt.kind() == "named_return" {
                named_result_type_nodes(&rt)
            } else {
                vec![rt]
            };
            for ty in checked {
                if !type_has_domain(ty, source) {
                    diagnostics.push(SigDiagnostic {
                        span: rel(&ty),
                        kind: SigDiagnosticKind::MissingDomainAnnotation,
                    });
                }
                if let Some(c) = domain_conflict(&ty, source, None) {
                    diagnostics.push(SigDiagnostic {
                        span: rel(&c),
                        kind: SigDiagnosticKind::ConflictingDomain,
                    });
                }
            }
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
    diagnostics.extend(pred_diags);
    let result_places = build_result_places(&return_type, &result_names);
    Signature {
        generic_params,
        params,
        return_type,
        result_places,
        fields: Vec::new(),
        predicates,
        const_value: None,
        diagnostics,
    }
}

/// The result place(s) for a fn's (already lifted) return type: one `return`
/// place for a normal return, the named part(s) for a named return. A tuple of
/// named parts splits into `result__0`/`result__1`/… (planning/return_variable.md).
fn build_result_places<'db>(
    return_type: &Option<Type<'db>>,
    names: &[String],
) -> Vec<ResultPlace<'db>> {
    let Some(rt) = return_type else {
        return Vec::new();
    };
    match names {
        // A normal return: the whole result is the `return` place.
        [] => vec![ResultPlace {
            name: "return".to_owned(),
            ty: rt.clone(),
            sv_base: "result".to_owned(),
        }],
        // A single named result names the whole result.
        [name] => vec![ResultPlace {
            name: name.clone(),
            ty: rt.clone(),
            sv_base: "result".to_owned(),
        }],
        // Two or more name the parts of a tuple result.
        _ => match rt {
            Type::Tuple(elems) => names
                .iter()
                .zip(elems)
                .enumerate()
                .map(|(i, (name, ety))| ResultPlace {
                    name: name.clone(),
                    ty: ety.clone(),
                    sv_base: format!("result__{i}"),
                })
                .collect(),
            _ => Vec::new(),
        },
    }
}

/// `-> (a: T, b: U)` → `["a", "b"]`, in order.
fn named_result_names(node: &Node, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "named_result")
        .filter_map(|c| c.child_by_field_name("name").map(|n| node_text(&n, source)))
        .collect()
}

/// `-> (a: T, b: U)` → the `T`, `U` type nodes, in order.
fn named_result_type_nodes<'t>(node: &Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "named_result")
        .filter_map(|c| c.child_by_field_name("type"))
        .collect()
}

/// Lower a fn's return-type node to its [`Type`]: a `named_return` becomes its
/// single element's type, or a tuple of the parts' types (≥2); any other node
/// lowers as an ordinary type.
fn lower_return_type<'db>(lowerer: &TypeLowerer<'_, 'db>, node: &Node, source: &str) -> Type<'db> {
    if node.kind() == "named_return" {
        let mut types: Vec<Type<'db>> = named_result_type_nodes(node)
            .iter()
            .map(|t| lowerer.lower_type(t, source))
            .collect();
        if types.len() == 1 {
            types.pop().unwrap()
        } else {
            Type::Tuple(types)
        }
    } else {
        lowerer.lower_type(node, source)
    }
}

/// Is this written type domain-annotated? Either an `@domain` suffix, or a
/// named-section type application (`DF{clk}(…)`) — a port/struct applied to
/// its domain arguments is fully domain-specified.
/// Propagate an aggregate annotation `@D` into a type's *unspecified* domain
/// slots — the `Ty @ D` constraint for a head-known type (fill, don't
/// override; planning/domain_checking.md). A no-op when `D` is Unspecified
/// (a pure signature; the lift handles those slots instead).
fn stamp_domain<'db>(ty: Type<'db>, dom: Domain) -> Type<'db> {
    if dom == Domain::Unspecified {
        return ty;
    }
    struct StampDom {
        dom: Domain,
    }
    impl<'db> Folder<'db> for StampDom {
        fn fold_domain(&mut self, d: Domain) -> Domain {
            match d {
                Domain::Unspecified => self.dom,
                other => other,
            }
        }
    }
    StampDom { dom }.fold_type(&ty)
}

/// An aggregate's `@D` may only FILL unspecified element slots, never
/// override a conflicting one (planning/domain_checking.md). Returns the
/// first element type node whose explicit domain conflicts with one imposed
/// by an enclosing aggregate. `@const` is compatible with any clock.
fn domain_conflict<'t>(t: &Node<'t>, source: &str, inherited: Option<&str>) -> Option<Node<'t>> {
    let own = t
        .child_by_field_name("domain")
        .map(|d| node_text(&d, source));
    if let (Some(d), Some(e)) = (inherited, own.as_deref())
        && d != e
        && d != "const"
        && e != "const"
    {
        return Some(*t);
    }
    let next = own.as_deref().or(inherited);
    let elems: Vec<Node> = if t.kind() == "tuple_type" {
        let mut c = t.walk();
        t.children(&mut c)
            .filter(|c| matches!(c.kind(), "type_expression" | "tuple_type"))
            .collect()
    } else if matches!(t.kind(), "type_expression" | "return_type_expression")
        && field_text(t, "name", source) == "Vec"
    {
        // `Vec(N, A)` — the element is the second type argument onward.
        vec_type_args(t).into_iter().skip(1).collect()
    } else {
        Vec::new()
    };
    elems.iter().find_map(|e| domain_conflict(e, source, next))
}

/// Is this written type domain-annotated? A domain lives on a *leaf*, so an
/// aggregate is annotated when its elements are (planning/domain_checking.md):
/// a tuple iff every element is; a `Vec` iff its element is.
fn type_has_domain(t: Node, source: &str) -> bool {
    if t.child_by_field_name("domain").is_some()
        || t.children(&mut t.walk())
            .any(|c| c.kind() == "type_named_args")
    {
        return true;
    }
    if t.kind() == "tuple_type" {
        let mut cursor = t.walk();
        let elems: Vec<Node> = t
            .children(&mut cursor)
            .filter(|c| matches!(c.kind(), "type_expression" | "tuple_type"))
            .collect();
        return !elems.is_empty() && elems.into_iter().all(|e| type_has_domain(e, source));
    }
    if matches!(t.kind(), "type_expression" | "return_type_expression")
        && field_text(&t, "name", source) == "Vec"
    {
        // `Vec(N, A)` — the element is the second type argument.
        return vec_type_args(&t)
            .get(1)
            .is_some_and(|e| type_has_domain(*e, source));
    }
    false
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
    /// What `Self` means here: the impl's self type (set while lowering an
    /// impl method's signature). In a trait DECL `Self` is instead the
    /// implicit Param(0) generic, reached through `generic_index`.
    self_ty: RefCell<Option<Type<'db>>>,
    /// Bare associated-const names resolve against this trait at this self
    /// type: `(trait def, Self)`. Set inside trait decls (`Self` = Param(0))
    /// and trait impls (the impl's self type).
    assoc_self: RefCell<Option<(DefId<'db>, Type<'db>)>>,
    /// The signature's trait bounds, for `T::width` projections: which trait
    /// a Type-kind `Param(i)` may project consts through.
    bounds: RefCell<Vec<(u32, DefId<'db>)>>,
}

impl<'db> TypeLowerer<'_, 'db> {
    fn lower_type(&self, node: &Node, source: &str) -> Type<'db> {
        // `(A, B)` — elements are full types (own domains); a trailing
        // `@clk` is the tuple's own domain, the default for elements
        // without one (planning/tuples.md).
        if node.kind() == "tuple_type" {
            let domain = self.lower_domain(node, source);
            let mut cursor = node.walk();
            // A trailing `@D` is the constraint "every clock slot is D",
            // propagated into each element's unspecified slots
            // (planning/domain_checking.md) — a tuple has no domain of its
            // own.
            let elems = node
                .children(&mut cursor)
                .filter(|c| matches!(c.kind(), "type_expression" | "tuple_type"))
                .map(|c| stamp_domain(self.lower_type(&c, source), domain))
                .collect();
            return Type::Tuple(elems);
        }
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
            "sint" => {
                return Type::Value {
                    kind: ValueKind::SInt {
                        width: self.lower_width(node, source),
                    },
                    domain,
                };
            }
            "bits" => {
                return Type::Value {
                    kind: ValueKind::Bits {
                        width: self.lower_width(node, source),
                    },
                    domain,
                };
            }
            "Vec" => {
                // `Vec(N, A)`: first positional arg is the const length,
                // second the element type. A domain lives on a leaf, never on
                // an aggregate — so an explicit `@D` here is the *constraint*
                // "every clock slot in A is D" (planning/domain_checking.md):
                // propagate it into the element's unspecified slots now, so a
                // later write meets a concrete element domain instead of a
                // lenient `Unspecified` (which laundered the crossing).
                let args = vec_type_args(node);
                let len = args
                    .first()
                    .map(|n| self.lower_const_expr(n, source))
                    .unwrap_or(ConstArg::Deferred);
                let elem = args
                    .get(1)
                    .map(|n| self.lower_type(n, source))
                    .unwrap_or(Type::Error);
                return Type::Vec {
                    len,
                    elem: Box::new(stamp_domain(elem, domain)),
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

        // 1b. `Self` in an impl: the impl's self type, restamped with the
        // use site's `@domain` if one is written.
        if name == "Self"
            && let Some(t) = self.self_ty.borrow().clone()
        {
            return match (&t, &domain) {
                (_, Domain::Unspecified) => t,
                (Type::Value { kind, .. }, d) => Type::Value {
                    kind: kind.clone(),
                    domain: d.clone(),
                },
                (Type::Port { def, args, .. }, d) => Type::Port {
                    def: *def,
                    args: args.clone(),
                    domain: d.clone(),
                },
                _ => t,
            };
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
                self.unresolved
                    .borrow_mut()
                    .push((name.clone(), at.start_byte(), at.end_byte()));
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
    fn lower_width(&self, node: &Node, source: &str) -> ConstArg<'db> {
        let Some(arg) = first_type_argument(node) else {
            return ConstArg::Deferred;
        };
        self.lower_const_expr(&arg, source)
    }

    /// Lower a const-expression tree in width/const position
    /// (`planning/const_eval.md`): literals, names (a Const-kind generic →
    /// `Param`, a body local → `Local`), `+`/`-`/`*` arithmetic, and field
    /// projection. Anything outside the fragment is `Deferred`.
    fn lower_const_expr(&self, node: &Node, source: &str) -> ConstArg<'db> {
        match node.kind() {
            "number" => node_text(node, source)
                .parse::<i128>()
                .map(ConstArg::Lit)
                .unwrap_or(ConstArg::Deferred),
            "const_expression" | "const_paren" => {
                let mut cursor = node.walk();
                match node.children(&mut cursor).find(|c| c.is_named()) {
                    Some(inner) => self.lower_const_expr(&inner, source),
                    None => ConstArg::Deferred,
                }
            }
            "const_binary" => {
                let op = match field_text(node, "operator", source).as_str() {
                    "+" => ConstOp::Add,
                    "-" => ConstOp::Sub,
                    "*" => ConstOp::Mul,
                    _ => return ConstArg::Deferred,
                };
                let lhs = match node.child_by_field_name("left") {
                    Some(l) => self.lower_const_expr(&l, source),
                    None => return ConstArg::Deferred,
                };
                let rhs = match node.child_by_field_name("right") {
                    Some(r) => self.lower_const_expr(&r, source),
                    None => return ConstArg::Deferred,
                };
                ConstArg::Op(op, Box::new(lhs), Box::new(rhs))
            }
            "const_field" => {
                let base = match node.child_by_field_name("base") {
                    Some(b) => self.lower_const_name(&node_text(&b, source)),
                    None => return ConstArg::Deferred,
                };
                let mut cursor = node.walk();
                node.children_by_field_name("field", &mut cursor)
                    .fold(base, |acc, f| {
                        ConstArg::Field(Box::new(acc), node_text(&f, source))
                    })
            }
            "const_path" => self.lower_const_path(
                &field_text(node, "base", source),
                &field_text(node, "item", source),
            ),
            "identifier" => self.lower_const_name(&node_text(node, source)),
            // A bare name parses as a type_expression — resolve its name.
            "type_expression" => self.lower_const_name(&field_text(node, "name", source)),
            _ => ConstArg::Deferred,
        }
    }

    fn lower_const_name(&self, name: &str) -> ConstArg<'db> {
        if let Some(i) = self.generic_index(name, TermKind::Const) {
            return ConstArg::Param(i);
        }
        if let Some(l) = self.locals.and_then(|f| f(name)) {
            return ConstArg::Local(l);
        }
        // A bare associated-const name in trait/impl scope (`uint(width)`).
        if let Some((trait_def, self_ty)) = self.assoc_self.borrow().clone()
            && let Some(item) = self.map.trait_const(trait_def, name)
        {
            return ConstArg::Assoc {
                item,
                self_ty: Box::new(self_ty),
            };
        }
        ConstArg::Deferred
    }

    /// `T::width` — project an associated const through a bounded type param.
    fn lower_const_path(&self, base: &str, item: &str) -> ConstArg<'db> {
        let Some(i) = self.generic_index(base, TermKind::Type) else {
            return ConstArg::Deferred;
        };
        for (j, trait_def) in self.bounds.borrow().iter() {
            if *j == i
                && let Some(c) = self.map.trait_const(*trait_def, item)
            {
                return ConstArg::Assoc {
                    item: c,
                    self_ty: Box::new(Type::Value {
                        kind: ValueKind::Param(i),
                        domain: Domain::Unspecified,
                    }),
                };
            }
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
        self_ty: RefCell::new(None),
        assoc_self: RefCell::new(None),
        bounds: RefCell::new(Vec::new()),
        unresolved: RefCell::new(Vec::new()),
    };
    let ty = lowerer.lower_type(node, source);
    if let Some(sink) = unresolved_sink {
        sink.append(&mut lowerer.unresolved.borrow_mut());
    }
    ty
}

/// The `impl_block` enclosing a method's fn node, if any.
fn enclosing_impl<'t>(node: &Node<'t>) -> Option<Node<'t>> {
    enclosing(node, "impl_block")
}

/// The nearest ancestor of `kind`, if any.
fn enclosing<'t>(node: &Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == kind {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

// ----- CST helpers -----

enum ParamClass {
    Generic(TermKind),
    Value,
}

/// Classify a parameter node as a generic param (by `dom`/`param` keyword or a
/// `: Type` annotation) or a value param.
fn classify(node: &Node, source: &str, is_trait: &dyn Fn(&str) -> bool) -> ParamClass {
    if param_name(node, source) == "self" {
        return ParamClass::Value;
    }
    if field_text(node, "kind", source) == "dom" {
        return ParamClass::Generic(TermKind::Domain(DomainSort::Clock));
    }
    // A `: Type` annotation makes it a Type-kind generic — this wins over a
    // `param` keyword (`param A: Type` is type-generic, not const-generic).
    // A trait name in the ascription is a *bounded* Type-kind generic
    // (`param T: Add` — planning/traits.md [D1]); the bound itself is
    // collected with the predicates.
    if let Some(ty) = node.child_by_field_name("type") {
        let n = field_text(&ty, "name", source);
        if n == "Type" || is_trait(&n) {
            return ParamClass::Generic(TermKind::Type);
        }
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

/// All positional type-argument nodes of a type reference, in order.
fn vec_type_args<'t>(node: &Node<'t>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for index in node
        .children(&mut cursor)
        .filter(|c| c.kind() == "type_index")
    {
        let mut c2 = index.walk();
        for arg in index
            .children(&mut c2)
            .filter(|c| c.kind() == "type_argument")
        {
            let mut c3 = arg.walk();
            if let Some(inner) = arg.children(&mut c3).find(|c| c.is_named()) {
                out.push(inner);
            }
        }
    }
    out
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
        vfs.set_file_text(db, "t.mrn", text);
        vfs.source_root(db, "t.mrn")
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
    fn result_places_name_the_return() {
        let mut db = RootDatabase::default();
        let mut vfs = Vfs::new();
        let krate = load(
            &mut db,
            &mut vfs,
            "fn r() -> uint(8) { return 0; }\n\
             fn s() -> (out: uint(8)) { out = 0; }\n\
             fn t() -> (sum: uint(8), carry: bool) { sum = 0; carry = false; }\n\
             fn u() { }",
        );
        // Unnamed: one whole-result place named `return`, SV base `result`.
        let r = sig_of(&db, krate, fn_def(&db, krate, "r"));
        assert_eq!(r.result_places.len(), 1);
        assert_eq!(r.result_places[0].name, "return");
        assert_eq!(r.result_places[0].sv_base, "result");
        // Single named: the whole result, SV base still `result`.
        let s = sig_of(&db, krate, fn_def(&db, krate, "s"));
        assert_eq!(s.result_places.len(), 1);
        assert_eq!(s.result_places[0].name, "out");
        assert_eq!(s.result_places[0].sv_base, "result");
        // Named tuple parts split into result__0 / result__1; return type is a tuple.
        let t = sig_of(&db, krate, fn_def(&db, krate, "t"));
        let bases: Vec<(&str, &str)> = t
            .result_places
            .iter()
            .map(|p| (p.name.as_str(), p.sv_base.as_str()))
            .collect();
        assert_eq!(bases, vec![("sum", "result__0"), ("carry", "result__1")]);
        assert!(matches!(t.return_type, Some(Type::Tuple(_))));
        // A unit fn has no result place.
        let u = sig_of(&db, krate, fn_def(&db, krate, "u"));
        assert!(u.result_places.is_empty());
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
            "t.mrn",
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
