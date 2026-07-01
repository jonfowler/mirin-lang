//! Stable syntactic identity — the `AstIdMap`.
//!
//! The keystone of incremental reuse: every item
//! gets a `FileAstId` derived from its **identity** (kind + name + enclosing
//! parent), *not* its byte offset or sibling position. So an edit inside one
//! body, a reformat, or inserting an *unrelated* item leaves every other item's
//! id unchanged — which is what lets downstream memo keys survive edits.
//!
//! This mirrors rust-analyzer's modern `AstIdMap` (a packed hash-of-identity id
//! with a collision disambiguator), specialised to Mirin's small item set and
//! to tree-sitter (we key entries by byte range instead of a rowan
//! `SyntaxNodePtr`; the map is rebuilt each parse and re-derives the same ids).

use tree_sitter::{Node, Tree};

/// Stable identity of an item within one file. Layout (32 bits):
/// `[ kind : 8 ][ index : 8 ][ name-hash : 16 ]`. The name-hash folds the
/// item's `(parent, name)` identity; `index` disambiguates same-`(kind, hash)`
/// siblings. Stable across edits that don't change this item's kind, name, or
/// the count of identical siblings before it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, salsa::Update)]
pub struct FileAstId(u32);

impl FileAstId {
    pub fn kind(self) -> AstIdKind {
        AstIdKind::from_raw((self.0 >> 24) as u8)
    }

    /// A synthetic id for a def with no source location (the prelude's builtins).
    /// Uses a reserved kind byte (`0xFF`) so it can never collide with an id
    /// minted from a real CST node (whose kinds are `0..=7`).
    pub fn synthetic(index: u16) -> FileAstId {
        FileAstId((0xFF << 24) | index as u32)
    }
}

/// The categories of item that receive a stable id. (Bodies, fields, params,
/// and expressions deliberately do not — they live below the firewall.)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, salsa::Update)]
#[repr(u8)]
pub enum AstIdKind {
    Fn = 0,
    Struct = 1,
    Port = 2,
    Impl = 3,
    Mod = 4,
    Use = 5,
    Trait = 6,
    /// An associated const (`trait_const` / `impl_const`).
    Const = 7,
}

impl AstIdKind {
    fn from_node_kind(kind: &str) -> Option<Self> {
        Some(match kind {
            "function_definition" => AstIdKind::Fn,
            // A trait's method decl is fn-shaped; it shares the Fn kind.
            "trait_method" => AstIdKind::Fn,
            "struct_definition" => AstIdKind::Struct,
            "port_definition" => AstIdKind::Port,
            "impl_block" => AstIdKind::Impl,
            "trait_definition" => AstIdKind::Trait,
            "trait_const" | "impl_const" => AstIdKind::Const,
            "module_definition" => AstIdKind::Mod,
            "use_declaration" => AstIdKind::Use,
            _ => return None,
        })
    }

    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => AstIdKind::Fn,
            1 => AstIdKind::Struct,
            2 => AstIdKind::Port,
            3 => AstIdKind::Impl,
            4 => AstIdKind::Mod,
            5 => AstIdKind::Use,
            6 => AstIdKind::Trait,
            7 => AstIdKind::Const,
            other => panic!("invalid AstIdKind discriminant {other}"),
        }
    }

    /// Does this item nest further items (so the walk recurses into it)? Only
    /// modules, impls, and traits do; fn/struct/port bodies hold no items.
    fn is_container(self) -> bool {
        matches!(self, AstIdKind::Mod | AstIdKind::Impl | AstIdKind::Trait)
    }
}

/// One id assignment: a stable id plus the byte range it occupied in *this*
/// parse (the handle back to the CST node, resolved via
/// `Tree::descendant_for_byte_range` when a later query needs the node).
#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::Update)]
pub struct AstIdEntry {
    pub id: FileAstId,
    pub start: u32,
    pub end: u32,
}

/// All stable ids for one file. Lookups are linear scans — a file has tens of
/// items, so this is not worth indexing.
#[derive(Clone, PartialEq, Eq, Debug, Default, salsa::Update)]
pub struct AstIdMap {
    entries: Vec<AstIdEntry>,
}

impl AstIdMap {
    /// Build the map by walking the parse tree's item structure.
    pub fn from_tree(tree: &Tree, source: &str) -> Self {
        let mut builder = Builder::default();
        builder.walk(tree.root_node(), None, source);
        AstIdMap {
            entries: builder.entries,
        }
    }

    /// The id assigned to a node, found by its byte range.
    pub fn id_for_node(&self, node: &Node) -> Option<FileAstId> {
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        self.entries
            .iter()
            .find(|e| e.start == start && e.end == end)
            .map(|e| e.id)
    }

    /// The byte range an id was last seen at, for resolving back to a CST node.
    pub fn range_of(&self, id: FileAstId) -> Option<(usize, usize)> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| (e.start as usize, e.end as usize))
    }

    pub fn entries(&self) -> &[AstIdEntry] {
        &self.entries
    }
}

#[derive(Default)]
struct Builder {
    entries: Vec<AstIdEntry>,
    /// `(kind, name-hash) -> next collision index`.
    collisions: std::collections::HashMap<(AstIdKind, u16), u8>,
}

impl Builder {
    fn walk(&mut self, node: Node, parent: Option<FileAstId>, source: &str) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match AstIdKind::from_node_kind(child.kind()) {
                Some(kind) => {
                    let id = self.mint(kind, &child, parent, source);
                    self.entries.push(AstIdEntry {
                        id,
                        start: child.start_byte() as u32,
                        end: child.end_byte() as u32,
                    });
                    if kind.is_container() {
                        self.walk(child, Some(id), source);
                    }
                }
                // Not an item: descend through structural wrappers (the file
                // root, `module_body`, `impl_body`) to reach nested items.
                None => self.walk(child, parent, source),
            }
        }
    }

    fn mint(
        &mut self,
        kind: AstIdKind,
        node: &Node,
        parent: Option<FileAstId>,
        source: &str,
    ) -> FileAstId {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("");
        let hash = ident_hash(parent, name);
        let counter = self.collisions.entry((kind, hash)).or_insert(0);
        let index = *counter;
        *counter = counter.saturating_add(1);
        FileAstId((hash as u32) | ((index as u32) << 16) | ((kind as u32) << 24))
    }
}

/// 16-bit FNV-1a over the parent id and name — the item's identity fingerprint.
/// Parent inclusion means a method's id depends on its `impl`, so identical
/// method names on different owners don't collide.
fn ident_hash(parent: Option<FileAstId>, name: &str) -> u16 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    };
    mix(&parent.map_or(0u32, |p| p.0).to_le_bytes());
    mix(name.as_bytes());
    // Fold to 16 bits.
    (hash ^ (hash >> 16) ^ (hash >> 32) ^ (hash >> 48)) as u16
}

/// QUERY: the stable id map for a file. Parses transiently (the tree-sitter
/// `Tree` cannot be a tracked value — see [`crate::base::db`]) and returns the owned,
/// comparable map. Re-runs when the file text changes, but its value is
/// unchanged by edits that don't touch item identity, so dependents survive.
#[salsa::tracked(returns(ref))]
pub fn ast_id_map(db: &dyn salsa::Database, file: crate::base::db::SourceFile) -> AstIdMap {
    let source = file.text(db);
    let tree = crate::base::parser::parse_text(source);
    AstIdMap::from_tree(&tree, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::db::{RootDatabase, SourceFile};

    fn file(db: &mut RootDatabase, text: &str) -> SourceFile {
        SourceFile::new(db, "t.mrn".into(), text.to_string())
    }

    /// Find the `function_definition` named `want` and return its stable id.
    fn fn_id(map: &AstIdMap, tree: &Tree, source: &str, want: &str) -> FileAstId {
        fn search<'a>(node: Node<'a>, source: &str, want: &str) -> Option<Node<'a>> {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "function_definition"
                    && child
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                        == Some(want)
                {
                    return Some(child);
                }
                if let Some(found) = search(child, source, want) {
                    return Some(found);
                }
            }
            None
        }
        let node = search(tree.root_node(), source, want).expect("function present");
        map.id_for_node(&node).expect("node has an id")
    }

    const TWO_FNS: &str = "\
fn a (x: uint(8)) -> uint(8) { return x; }
fn b (y: uint(8)) -> uint(8) { return y; }
";

    #[test]
    fn assigns_one_id_per_top_level_item() {
        let mut db = RootDatabase::default();
        let f = file(&mut db, TWO_FNS);
        let map = ast_id_map(&db, f);
        assert_eq!(map.entries().len(), 2);
        assert!(map.entries().iter().all(|e| e.id.kind() == AstIdKind::Fn));
    }

    #[test]
    fn id_is_stable_across_an_unrelated_body_edit() {
        // Editing the body of `a` must not change the id of `b`.
        let src_a = TWO_FNS;
        let src_b = "\
fn a (x: uint(8)) -> uint(8) { return x + x; }
fn b (y: uint(8)) -> uint(8) { return y; }
";
        let tree_a = crate::base::parser::parse_text(src_a);
        let tree_b = crate::base::parser::parse_text(src_b);
        let map_a = AstIdMap::from_tree(&tree_a, src_a);
        let map_b = AstIdMap::from_tree(&tree_b, src_b);
        assert_eq!(
            fn_id(&map_a, &tree_a, src_a, "b"),
            fn_id(&map_b, &tree_b, src_b, "b"),
            "b's id must survive an edit to a's body"
        );
    }

    #[test]
    fn id_is_stable_across_inserting_an_earlier_item() {
        // The hash-of-identity scheme's payoff over a positional index: adding
        // an item *before* `b` must not renumber `b`.
        let src_a = TWO_FNS;
        let src_b = "\
fn a (x: uint(8)) -> uint(8) { return x; }
fn c (z: uint(8)) -> uint(8) { return z; }
fn b (y: uint(8)) -> uint(8) { return y; }
";
        let tree_a = crate::base::parser::parse_text(src_a);
        let tree_b = crate::base::parser::parse_text(src_b);
        let map_a = AstIdMap::from_tree(&tree_a, src_a);
        let map_b = AstIdMap::from_tree(&tree_b, src_b);
        assert_eq!(
            fn_id(&map_a, &tree_a, src_a, "b"),
            fn_id(&map_b, &tree_b, src_b, "b"),
            "inserting `c` before `b` must not change b's id"
        );
    }

    #[test]
    fn nested_items_get_ids_with_parent_context() {
        let src = "\
mod m {
  fn inner (x: uint(8)) -> uint(8) { return x; }
}
impl Widget {
  fn method (self) -> uint(8) { return 0; }
}
";
        let mut db = RootDatabase::default();
        let f = file(&mut db, src);
        let map = ast_id_map(&db, f);
        // mod + its inner fn + impl + its method = 4 ids.
        assert_eq!(map.entries().len(), 4);
        let kinds: Vec<_> = map.entries().iter().map(|e| e.id.kind()).collect();
        assert!(kinds.contains(&AstIdKind::Mod));
        assert!(kinds.contains(&AstIdKind::Impl));
        assert_eq!(
            kinds.iter().filter(|k| **k == AstIdKind::Fn).count(),
            2,
            "the nested fn and the impl method both get Fn ids"
        );
    }

    #[test]
    fn query_memoizes_and_invalidates() {
        let mut db = RootDatabase::default();
        let mut vfs = crate::base::vfs::Vfs::new();
        let f = vfs.set_file_text(&mut db, "t.mrn", TWO_FNS);
        let len_before = ast_id_map(&db, f).entries().len();
        assert_eq!(len_before, 2);
        // Add an item; the map reflects it after the input changes.
        vfs.set_file_text(&mut db, "t.mrn", "fn a () -> uint(8) { return 0; }");
        assert_eq!(ast_id_map(&db, f).entries().len(), 1);
    }
}
