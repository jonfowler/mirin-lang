//! The `item_tree` — the per-file **syntactic firewall** (`planning/query_engine.md`
//! §3.1). A lean summary of the items a file declares: kind, name, visibility,
//! and stable [`FileAstId`], with modules recursing into their children and
//! impls carrying their method index.
//!
//! Deliberately holds **no types, fields, param signatures, or bodies** — only
//! what *name resolution* (Q2 `crate_def_map`) needs. Because it is a pure
//! function of the parse that drops everything edit-volatile, editing a function
//! body (or a signature) produces a structurally-equal `ItemTree`, so salsa
//! backdates it and name resolution / every other item survive untouched. This
//! is the modern rust-analyzer shape (signatures live in a separate layer, Q3).

use tree_sitter::Node;

use crate::base::db::SourceFile;
use crate::base::parser;
use crate::syntax::ast_id::{AstIdMap, FileAstId, ast_id_map};

/// The items of one file, in source order. Top level only at the root; modules
/// nest their own items.
#[derive(Clone, PartialEq, Eq, Debug, Default, salsa::Update)]
pub struct ItemTree {
    pub top_level: Vec<Item>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum Item {
    Fn(FnItem),
    Struct(NamedItem),
    Port(NamedItem),
    Trait(TraitItem),
    Impl(ImplItem),
    Mod(ModItem),
    Use(UseItem),
}

/// A function — at the top level or as an impl method.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct FnItem {
    pub name: String,
    pub visibility: Visibility,
    pub ast_id: FileAstId,
}

/// A struct or port: its type name, its mandatory constructor name (`struct
/// Bus = bus`), visibility, and id. Fields and parameters are deferred to the
/// signature layer (Q3) and are not part of the firewall.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct NamedItem {
    pub name: String,
    /// The term-level constructor name after `=` (the grammar requires it).
    pub constructor: String,
    pub visibility: Visibility,
    pub ast_id: FileAstId,
}

/// A `trait Name { … }` declaration: method signatures and associated-const
/// declarations (no bodies).
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct TraitItem {
    pub name: String,
    pub visibility: Visibility,
    pub ast_id: FileAstId,
    pub methods: Vec<FnItem>,
    pub consts: Vec<AssocConstItem>,
}

/// An associated const: declared in a trait (`const width: integer;`) or
/// valued in an impl (`const width: integer = 1;`).
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct AssocConstItem {
    pub name: String,
    pub ast_id: FileAstId,
}

/// An `impl Owner { … }` or `impl Trait for SelfType { … }` block. `owner` is
/// the implementing type's name as written — the `name` field for an inherent
/// impl, the head of the `for` self type for a trait impl (resolved in Q2);
/// `methods` is the impl-method index name resolution needs.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct ImplItem {
    pub owner: String,
    /// The trait's name, for a trait impl.
    pub trait_: Option<String>,
    /// Whether a trait impl's self type carries explicit args
    /// (`impl Scale for uint(8)` vs `impl Scale for Sample`). Coherence's
    /// cheap first cut: two arg-less impls for one head always overlap.
    pub self_has_args: bool,
    pub ast_id: FileAstId,
    pub methods: Vec<FnItem>,
    pub consts: Vec<AssocConstItem>,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct ModItem {
    pub name: String,
    pub visibility: Visibility,
    pub ast_id: FileAstId,
    pub kind: ModKind,
}

#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum ModKind {
    /// `mod foo { … }` — items written in place.
    Inline(Vec<Item>),
    /// `mod foo;` — body lives in `foo.mrn`, stitched in by Q2's loader.
    File,
}

/// A `use` import: its visibility, id, and the lowered import tree that name
/// resolution (`crate_def_map`, Q2c) consumes. The tree is pure syntax, so it
/// belongs in the firewall.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub struct UseItem {
    pub visibility: Visibility,
    pub ast_id: FileAstId,
    pub tree: UseTree,
}

/// A `use` tree, mirroring `surface::UseTree` but with owned string segments
/// (the firewall holds no `NodeId`s). `crate::`/`super::`/`self` anchors are
/// ordinary segments the resolver recognises.
#[derive(Clone, PartialEq, Eq, Debug, salsa::Update)]
pub enum UseTree {
    /// `a::b::c` optionally `as d`. `segments` is the whole path (≥1).
    Path {
        segments: Vec<String>,
        alias: Option<String>,
    },
    /// `prefix::{ children }` — `prefix` may be empty (`{ … }` at the root).
    Group {
        prefix: Vec<String>,
        children: Vec<UseTree>,
    },
    /// `prefix::*` — `prefix` may be empty.
    Glob { prefix: Vec<String> },
}

/// Visibility as written. Mirrors `mirin-compiler`'s `surface::Visibility`.
#[derive(Clone, PartialEq, Eq, Debug, Default, salsa::Update)]
pub enum Visibility {
    /// No modifier — private to the defining module and its descendants.
    #[default]
    Inherited,
    Public,
    Crate,
    Super,
    /// `pub(in a::b)` — the restriction path segments.
    Restricted(Vec<String>),
}

/// QUERY: the item summary for a file. Parses transiently and reads the shared
/// [`ast_id_map`]; returns the owned, comparable [`ItemTree`].
#[salsa::tracked(returns(ref))]
pub fn item_tree(db: &dyn salsa::Database, file: SourceFile) -> ItemTree {
    let source = file.text(db);
    let tree = parser::parse_text(source);
    let ast_ids = ast_id_map(db, file);
    ItemTree {
        top_level: lower_items(tree.root_node(), source, ast_ids),
    }
}

/// Lower the item children of `node` (the file root, or a `module_body`).
fn lower_items(node: Node, source: &str, ast_ids: &AstIdMap) -> Vec<Item> {
    let mut cursor = node.walk();
    let mut items = Vec::new();
    for child in node.children(&mut cursor) {
        let item = match child.kind() {
            "function_definition" => Item::Fn(fn_item(&child, source, ast_ids)),
            "struct_definition" => Item::Struct(named_item(&child, source, ast_ids)),
            "port_definition" => Item::Port(named_item(&child, source, ast_ids)),
            "trait_definition" => Item::Trait(trait_item(&child, source, ast_ids)),
            "impl_block" => Item::Impl(impl_item(&child, source, ast_ids)),
            "module_definition" => Item::Mod(mod_item(&child, source, ast_ids)),
            "use_declaration" => Item::Use(UseItem {
                visibility: visibility(&child, source),
                ast_id: ast_id(&child, ast_ids),
                tree: child
                    .child_by_field_name("tree")
                    .map(|t| lower_use_tree(&t, source))
                    .unwrap_or(UseTree::Path {
                        segments: Vec::new(),
                        alias: None,
                    }),
            }),
            _ => continue,
        };
        items.push(item);
    }
    items
}

fn fn_item(node: &Node, source: &str, ast_ids: &AstIdMap) -> FnItem {
    FnItem {
        name: name_of(node, source),
        visibility: visibility(node, source),
        ast_id: ast_id(node, ast_ids),
    }
}

fn named_item(node: &Node, source: &str, ast_ids: &AstIdMap) -> NamedItem {
    NamedItem {
        name: name_of(node, source),
        constructor: field_text(node, "constructor", source),
        visibility: visibility(node, source),
        ast_id: ast_id(node, ast_ids),
    }
}

fn impl_item(node: &Node, source: &str, ast_ids: &AstIdMap) -> ImplItem {
    let mut methods = Vec::new();
    let mut consts = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for c in body.children(&mut cursor) {
            match c.kind() {
                "function_definition" => methods.push(fn_item(&c, source, ast_ids)),
                "impl_const" => consts.push(AssocConstItem {
                    name: name_of(&c, source),
                    ast_id: ast_id(&c, ast_ids),
                }),
                _ => {}
            }
        }
    }
    // The impl's `name` is now a (restricted) TYPE expression: for an inherent
    // impl it IS the self type (`Bus(A)`); for a trait impl it is the trait and
    // `for` introduces the self type. The owner is the self type's head ident;
    // `self_has_args` records whether that self type is applied (`Bus(A)`).
    let has_args = |n: &Node| n.children(&mut n.walk()).any(|c| c.kind() == "type_index");
    let head = |n: &Node| field_text(n, "name", source);
    let name_node = node.child_by_field_name("name");
    let (owner, trait_, self_has_args) = match node.child_by_field_name("self_type") {
        Some(st) => (head(&st), name_node.as_ref().map(head), has_args(&st)),
        None => (
            name_node.as_ref().map(head).unwrap_or_default(),
            None,
            name_node.as_ref().is_some_and(has_args),
        ),
    };
    ImplItem {
        owner,
        trait_,
        self_has_args,
        ast_id: ast_id(node, ast_ids),
        methods,
        consts,
    }
}

fn trait_item(node: &Node, source: &str, ast_ids: &AstIdMap) -> TraitItem {
    let mut methods = Vec::new();
    let mut consts = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for c in body.children(&mut cursor) {
            match c.kind() {
                "trait_method" => methods.push(fn_item(&c, source, ast_ids)),
                "trait_const" => consts.push(AssocConstItem {
                    name: name_of(&c, source),
                    ast_id: ast_id(&c, ast_ids),
                }),
                _ => {}
            }
        }
    }
    TraitItem {
        name: name_of(node, source),
        visibility: visibility(node, source),
        ast_id: ast_id(node, ast_ids),
        methods,
        consts,
    }
}

fn mod_item(node: &Node, source: &str, ast_ids: &AstIdMap) -> ModItem {
    let kind = match node.child_by_field_name("body") {
        Some(body) => ModKind::Inline(lower_items(body, source, ast_ids)),
        None => ModKind::File,
    };
    ModItem {
        name: name_of(node, source),
        visibility: visibility(node, source),
        ast_id: ast_id(node, ast_ids),
        kind,
    }
}

fn name_of(node: &Node, source: &str) -> String {
    field_text(node, "name", source)
}

/// The text of a named child field, or `""` if absent.
fn field_text(node: &Node, field: &str, source: &str) -> String {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("")
        .to_string()
}

fn ast_id(node: &Node, ast_ids: &AstIdMap) -> FileAstId {
    ast_ids
        .id_for_node(node)
        .expect("every item node is assigned a FileAstId by ast_id_map")
}

/// Parse the optional `visibility` modifier on an item.
fn visibility(node: &Node, source: &str) -> Visibility {
    let Some(vis) = node.child_by_field_name("visibility") else {
        return Visibility::Inherited;
    };
    let text = vis.utf8_text(source.as_bytes()).unwrap_or("pub");
    if text.contains("(crate") {
        Visibility::Crate
    } else if text.contains("(super") {
        Visibility::Super
    } else if text.contains("(in") {
        // `pub(in a::b)` — collect the use_path's identifier segments.
        let mut segments = Vec::new();
        let mut cursor = vis.walk();
        for path in vis.children(&mut cursor).filter(|c| c.kind() == "use_path") {
            let mut pc = path.walk();
            for ident in path.children(&mut pc).filter(|c| c.kind() == "identifier") {
                if let Ok(text) = ident.utf8_text(source.as_bytes()) {
                    segments.push(text.to_string());
                }
            }
        }
        Visibility::Restricted(segments)
    } else {
        Visibility::Public
    }
}

/// Lower a `use_tree` CST node. Mirrors `surface::ir::lower_use_tree`.
fn lower_use_tree(node: &Node, source: &str) -> UseTree {
    let mut cursor = node.walk();
    let prefix = node
        .children(&mut cursor)
        .find(|c| c.kind() == "use_path")
        .map(|p| use_path_segments(&p, source))
        .unwrap_or_default();
    if let Some(group) = node.child_by_field_name("group") {
        let mut gc = group.walk();
        let children = group
            .children(&mut gc)
            .filter(|c| c.kind() == "use_tree")
            .map(|c| lower_use_tree(&c, source))
            .collect();
        UseTree::Group { prefix, children }
    } else if node.child_by_field_name("glob").is_some() {
        UseTree::Glob { prefix }
    } else {
        let alias = node
            .child_by_field_name("alias")
            .and_then(|a| a.utf8_text(source.as_bytes()).ok())
            .map(str::to_owned);
        UseTree::Path {
            segments: prefix,
            alias,
        }
    }
}

/// The identifier segments of a `use_path` node.
fn use_path_segments(node: &Node, source: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "identifier")
        .filter_map(|c| c.utf8_text(source.as_bytes()).ok().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::RootDatabase;

    /// Pure lowering for tests — builds its own `AstIdMap`, no db.
    fn lower(source: &str) -> ItemTree {
        let tree = parser::parse_text(source);
        let ast_ids = AstIdMap::from_tree(&tree, source);
        ItemTree {
            top_level: lower_items(tree.root_node(), source, &ast_ids),
        }
    }

    const SAMPLE: &str = "\
use a::b;
pub fn top (x: uint(8)) -> uint(8) { return x; }
struct S = S { a: uint(8) }
mod inner {
  pub(crate) fn nested () -> uint(8) { return 0; }
}
impl Widget {
  fn m1 (self) -> uint(8) { return 0; }
  fn m2 (self) -> uint(8) { return 1; }
}
";

    #[test]
    fn lowers_all_item_kinds_with_names_and_visibility() {
        let it = lower(SAMPLE);
        assert_eq!(it.top_level.len(), 5);
        assert!(matches!(&it.top_level[0], Item::Use(_)));
        match &it.top_level[1] {
            Item::Fn(f) => {
                assert_eq!(f.name, "top");
                assert_eq!(f.visibility, Visibility::Public);
            }
            other => panic!("expected fn, got {other:?}"),
        }
        assert!(matches!(&it.top_level[2], Item::Struct(s) if s.name == "S"));
    }

    #[test]
    fn module_recurses_and_keeps_inner_visibility() {
        let it = lower(SAMPLE);
        let Item::Mod(m) = &it.top_level[3] else {
            panic!("expected mod")
        };
        assert_eq!(m.name, "inner");
        let ModKind::Inline(children) = &m.kind else {
            panic!("expected inline mod")
        };
        assert_eq!(children.len(), 1);
        assert!(
            matches!(&children[0], Item::Fn(f) if f.name == "nested" && f.visibility == Visibility::Crate)
        );
    }

    #[test]
    fn impl_collects_its_method_index() {
        let it = lower(SAMPLE);
        let Item::Impl(i) = &it.top_level[4] else {
            panic!("expected impl")
        };
        assert_eq!(i.owner, "Widget");
        let names: Vec<_> = i.methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["m1", "m2"]);
        // Methods get distinct ids (parent = the impl).
        assert_ne!(i.methods[0].ast_id, i.methods[1].ast_id);
    }

    #[test]
    fn file_module_has_no_inline_body() {
        let it = lower("mod external;");
        let Item::Mod(m) = &it.top_level[0] else {
            panic!("expected mod")
        };
        assert_eq!(m.kind, ModKind::File);
    }

    #[test]
    fn firewall_body_edit_does_not_change_the_item_tree() {
        // THE firewall: a length-changing edit to a body leaves the item_tree
        // structurally equal (ids are offset-independent), so salsa backdates
        // and name resolution survives.
        let before = lower("fn a () -> uint(8) { return 0; }\nfn b () -> uint(8) { return 1; }\n");
        let after =
            lower("fn a () -> uint(8) { return 0 + 0 + 0; }\nfn b () -> uint(8) { return 1; }\n");
        assert_eq!(
            before, after,
            "editing a body must not change the item_tree"
        );
    }

    #[test]
    fn query_item_tree_is_stable_across_a_body_edit() {
        // Same firewall, exercised through the salsa query + a real input edit.
        let mut db = RootDatabase::default();
        let mut vfs = crate::base::vfs::Vfs::new();
        let f = vfs.set_file_text(&mut db, "t.mrn", "fn a () -> uint(8) { return 0; }");
        let before = item_tree(&db, f).clone();
        vfs.set_file_text(&mut db, "t.mrn", "fn a () -> uint(8) { return 0 + 1 + 2; }");
        let after = item_tree(&db, f).clone();
        assert_eq!(
            before, after,
            "the item_tree query must backdate across a body edit"
        );
    }

    #[test]
    fn query_reflects_a_new_item() {
        let mut db = RootDatabase::default();
        let mut vfs = crate::base::vfs::Vfs::new();
        let f = vfs.set_file_text(&mut db, "t.mrn", "fn a () -> uint(8) { return 0; }");
        assert_eq!(item_tree(&db, f).top_level.len(), 1);
        vfs.set_file_text(
            &mut db,
            "t.mrn",
            "fn a () -> uint(8) { return 0; }\nfn b () -> uint(8) { return 1; }",
        );
        assert_eq!(item_tree(&db, f).top_level.len(), 2);
    }
}
