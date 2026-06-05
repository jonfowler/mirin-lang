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

use tree_sitter::Node;

use crate::base::db::SourceRoot;
use crate::base::parser;
use crate::hir::types::{
    ConstArg, Direction, Domain, GenericArg, GenericArgs, GenericParam, GenericParamKind, LocalId,
    Type, ValueKind,
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

    Signature {
        generic_params,
        params: Vec::new(),
        return_type: None,
        fields,
    }
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
    let mut value_param_nodes: Vec<Node> = Vec::new();
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
                ParamClass::Value => value_param_nodes.push(p),
            }
        }
    }

    let lowerer = TypeLowerer {
        map,
        module,
        generics: &generic_params,
    };

    // Pass 2: lower each value parameter's type, assigning owner-relative ids.
    let mut params = Vec::new();
    for (i, p) in value_param_nodes.iter().enumerate() {
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
        });
    }

    let return_type = node
        .child_by_field_name("return_type")
        .map(|t| lowerer.lower_type(&t, source));

    Signature {
        generic_params,
        params,
        return_type,
        fields: Vec::new(),
    }
}

/// Lowers a `type_expression` / `return_type_expression` to a [`Type`], resolving
/// names against the prelude builtins, the enclosing def's generic params, and
/// the crate's name resolution.
struct TypeLowerer<'a, 'db> {
    map: &'a CrateDefMap<'db>,
    module: ModuleId,
    generics: &'a [GenericParam],
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
            "usize" => {
                return Type::Value {
                    kind: ValueKind::Usize,
                    domain,
                };
            }
            "Clock" => return Type::Clock,
            _ => {}
        }

        // 2. A Type-kind generic parameter referenced by name (`data: A`).
        if let Some(i) = self.generic_index(&name, GenericParamKind::Type) {
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
            _ => Type::Error,
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
                match self.generic_index(&name, GenericParamKind::Domain) {
                    Some(i) => Domain::Param(i),
                    None => Domain::Unspecified,
                }
            }
        }
    }

    /// The width inside `uint(W)`: a literal, a Const-kind generic ref, or
    /// deferred (anything else — arithmetic widths land in `const_eval`, Q4).
    fn lower_width(&self, node: &Node, source: &str) -> ConstArg {
        let Some(arg) = first_type_argument(node) else {
            return ConstArg::Deferred;
        };
        if arg.kind() == "number" {
            return node_text(&arg, source)
                .parse::<u64>()
                .map(ConstArg::Lit)
                .unwrap_or(ConstArg::Deferred);
        }
        // A bare identifier naming a Const-kind generic param.
        let name = field_text(&arg, "name", source);
        match self.generic_index(&name, GenericParamKind::Const) {
            Some(i) => ConstArg::Param(i),
            None => ConstArg::Deferred,
        }
    }

    /// Generic args at a struct/port reference (`Bus(uint(8))`). For Q3b a
    /// numeric arg is a `Const`, anything else a `Type`; const/domain args passed
    /// by name (and named-section `{clk}` args) are refined in Q3d.
    fn lower_args(&self, node: &Node, source: &str) -> GenericArgs<'db> {
        let mut args = Vec::new();
        if let Some(index) = type_index(node) {
            let mut cursor = index.walk();
            for ta in index
                .children(&mut cursor)
                .filter(|c| c.kind() == "type_argument")
            {
                let mut tc = ta.walk();
                let Some(inner) = ta.children(&mut tc).find(|n| n.is_named()) else {
                    continue;
                };
                if inner.kind() == "number" {
                    let c = node_text(&inner, source)
                        .parse::<u64>()
                        .map(ConstArg::Lit)
                        .unwrap_or(ConstArg::Deferred);
                    args.push(GenericArg::Const(c));
                } else {
                    args.push(GenericArg::Type(self.lower_type(&inner, source)));
                }
            }
        }
        GenericArgs(args)
    }

    fn generic_index(&self, name: &str, kind: GenericParamKind) -> Option<u32> {
        self.generics
            .iter()
            .position(|g| g.name == name && g.kind == kind)
            .map(|i| i as u32)
    }
}

// ----- CST helpers -----

enum ParamClass {
    Generic(GenericParamKind),
    Value,
}

/// Classify a parameter node as a generic param (by `dom`/`param` keyword or a
/// `: Type` annotation) or a value param.
fn classify(node: &Node, source: &str) -> ParamClass {
    if param_name(node, source) == "self" {
        return ParamClass::Value;
    }
    match field_text(node, "kind", source).as_str() {
        "dom" => return ParamClass::Generic(GenericParamKind::Domain),
        "param" => return ParamClass::Generic(GenericParamKind::Const),
        _ => {}
    }
    // `A: Type` — a Type-kind generic.
    if let Some(ty) = node.child_by_field_name("type")
        && field_text(&ty, "name", source) == "Type"
    {
        return ParamClass::Generic(GenericParamKind::Type);
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

        assert!(sig.generic_params.is_empty());
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
                domain: Domain::Unspecified
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
            "fn f { dom clk: Clock, param N: usize, A: Type } (x: uint(N) @clk) -> uint(N) @clk { return x; }",
        );
        let def = fn_def(&db, krate, "f");
        let sig = sig_of(&db, krate, def);

        // Three generics, in named-section declaration order.
        assert_eq!(sig.generic_params.len(), 3);
        assert_eq!(sig.generic_params[0].name, "clk");
        assert_eq!(sig.generic_params[0].kind, GenericParamKind::Domain);
        assert_eq!(sig.generic_params[1].kind, GenericParamKind::Const); // N
        assert_eq!(sig.generic_params[2].kind, GenericParamKind::Type); // A
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
                    GenericArg::Type(Type::Value {
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
        assert_eq!(sig.generic_params[0].kind, GenericParamKind::Type);
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
        assert_eq!(sig.generic_params[0].kind, GenericParamKind::Domain);
        assert!(sig.generic_params[0].from_named_section);
        assert_eq!(sig.generic_params[1].kind, GenericParamKind::Type);
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
