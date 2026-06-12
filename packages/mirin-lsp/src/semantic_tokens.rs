//! Semantic tokens, computed from the *same* `highlights.scm` the grammar ships
//! (`planning/lsp.md` M1). The query is embedded at compile time so the server
//! and the editor's TextMate fallback never drift, and run via a cached
//! [`Query`]; captures map to a fixed legend by name.
//!
//! Overlap policy: when several patterns capture the same span, the one earlier
//! in the query wins — the trailing `(identifier) @variable` is an explicit
//! fallback, so specific patterns must take priority over it.

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tower_lsp_server::ls_types::{
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
};
use tree_sitter::{Query, QueryCursor, Tree};

use crate::encoding::{Encoding, byte_to_position, line_content_end_byte};

/// The highlight query, shared with the compiler/grammar via `include_str!`.
pub const HIGHLIGHTS: &str = include_str!("../../tree-sitter-mirin/queries/highlights.scm");

/// Emitted token types, in legend order — the index here is the wire value.
const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE, // 0
    SemanticTokenType::TYPE,      // 1
    SemanticTokenType::FUNCTION,  // 2
    SemanticTokenType::PARAMETER, // 3
    SemanticTokenType::VARIABLE,  // 4
    SemanticTokenType::PROPERTY,  // 5
    SemanticTokenType::KEYWORD,   // 6
    SemanticTokenType::MODIFIER,  // 7
    SemanticTokenType::COMMENT,   // 8
    SemanticTokenType::NUMBER,    // 9
    SemanticTokenType::OPERATOR,  // 10
    // `@constant` (true/false/high/low) — LSP has no "constant" token type;
    // enumMember is the conventional stand-in and themes colour it as one.
    SemanticTokenType::ENUM_MEMBER, // 11
];

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: vec![],
    }
}

/// Compile the embedded highlight query against the Mirin grammar.
pub fn query() -> Query {
    Query::new(&mirin_compiler::language(), HIGHLIGHTS).expect("highlights.scm is a valid query")
}

/// Map a `highlights.scm` capture name to a legend index.
fn token_index(capture: &str) -> Option<u32> {
    Some(match capture {
        "namespace" => 0,
        "type" => 1,
        "function" | "constructor" => 2,
        "variable.parameter" => 3,
        "variable" => 4,
        "property" => 5,
        "keyword" => 6,
        "keyword.modifier" => 7,
        "comment" => 8,
        "number" => 9,
        "operator" | "punctuation.special" | "punctuation.delimiter" => 10,
        "constant" => 11,
        _ => return None,
    })
}

pub fn compute(rope: &Rope, tree: &Tree, query: &Query, enc: Encoding) -> SemanticTokens {
    let source = rope.to_string();
    let names = query.capture_names();

    // (start_byte, end_byte, pattern_index, token_type)
    let mut raw: Vec<(usize, usize, usize, u32)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut caps = cursor.captures(query, tree.root_node(), source.as_bytes());
    while let Some((m, capture_ix)) = caps.next() {
        let cap = m.captures[*capture_ix];
        if let Some(tt) = token_index(names[cap.index as usize]) {
            raw.push((
                cap.node.start_byte(),
                cap.node.end_byte(),
                m.pattern_index,
                tt,
            ));
        }
    }

    // Earlier pattern wins; drop spans overlapping one already kept.
    raw.sort_by_key(|&(s, _, p, _)| (s, p));
    let mut chosen: Vec<(usize, usize, u32)> = Vec::new();
    let mut last_end = 0;
    for (s, e, _p, tt) in raw {
        if e > s && s >= last_end {
            chosen.push((s, e, tt));
            last_end = e;
        }
    }

    // Delta-encode, splitting any multi-line span into per-line tokens (LSP
    // tokens cannot span lines).
    let mut data = Vec::new();
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    for (s, e, tt) in chosen {
        let first = rope.byte_to_line(s);
        let last = rope.byte_to_line(e);
        for line in first..=last {
            let seg_start = s.max(rope.line_to_byte(line));
            let seg_end = e.min(line_content_end_byte(rope, line));
            if seg_end <= seg_start {
                continue;
            }
            let sp = byte_to_position(rope, seg_start, enc);
            let ep = byte_to_position(rope, seg_end, enc);
            let delta_line = sp.line - prev_line;
            let delta_start = if delta_line == 0 {
                sp.character - prev_start
            } else {
                sp.character
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: ep.character - sp.character,
                token_type: tt,
                token_modifiers_bitset: 0,
            });
            (prev_line, prev_start) = (sp.line, sp.character);
        }
    }

    SemanticTokens {
        result_id: None,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Decode the delta stream back to absolute (line, start, len, type) for
    /// assertions.
    fn decode(toks: &SemanticTokens) -> Vec<(u32, u32, u32, u32)> {
        let (mut line, mut start) = (0, 0);
        let mut out = Vec::new();
        for t in &toks.data {
            if t.delta_line == 0 {
                start += t.delta_start;
            } else {
                line += t.delta_line;
                start = t.delta_start;
            }
            out.push((line, start, t.length, t.token_type));
        }
        out
    }

    // A real, parseable Mirin fn (from examples/working/add_constant.mrn).
    const ADD_CONSTANT: &str = "fn addConstant\n  { dom clk: Clock }\n  \
        ( value: uint(8) @clk )\n  -> uint(8) @clk\n  {\n    \
        let bumped = value + 3;\n    bumped\n  }\n";

    #[test]
    fn keyword_and_number_are_tokenized() {
        let doc = Document::open(ADD_CONSTANT);
        let toks = compute(&doc.rope, &doc.tree, &query(), Encoding::Utf8);
        let decoded = decode(&toks);
        // `let` → keyword (type 6), length 3 (the only len-3 keyword here).
        assert!(
            decoded.iter().any(|&(_, _, len, ty)| ty == 6 && len == 3),
            "expected a `let` keyword token, got {decoded:?}"
        );
        // numeric literals → number (type 9).
        assert!(
            decoded.iter().any(|&(_, _, _, ty)| ty == 9),
            "expected a number token, got {decoded:?}"
        );
    }

    #[test]
    fn constructor_is_highlighted_at_definition_and_use() {
        // `packet` is the constructor; it must be tagged at the `struct` def
        // AND at the `packet { .. }` use site (record_constructor_expression).
        let src = "struct Packet = packet {\n  valid: bool,\n}\n\n\
            fn f\n  { dom clk: Clock }\n  ( inp: Packet @clk )\n  -> Packet @clk\n  {\n    \
            let held = packet { valid: false };\n    return held;\n  }\n";
        let doc = Document::open(src);
        let toks = compute(&doc.rope, &doc.tree, &query(), Encoding::Utf8);
        // CONSTRUCTOR maps to FUNCTION (legend index 2); real `fn` names resolve
        // to TYPE via an earlier pattern, so a type-2 count of 2 means both
        // constructor sites (def + use) are tagged.
        let decoded = decode(&toks);
        let ctors = decoded.iter().filter(|&&(_, _, _, ty)| ty == 2).count();
        assert_eq!(
            ctors, 2,
            "constructor not tagged at both sites: {decoded:?}"
        );
    }

    /// The VS Code TextMate fallback, read from the repo checkout. Tests use it
    /// to keep the cold-start grammar in sync with the real one.
    fn tm_grammar() -> String {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../editors/vscode/syntaxes/mirin.tmLanguage.json"
        );
        std::fs::read_to_string(path).expect("read mirin.tmLanguage.json")
    }

    /// Whole-word containment: `needle` occurs in `haystack` with non-word
    /// characters (or the string edge) on both sides.
    fn contains_word(haystack: &str, needle: &str) -> bool {
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        haystack.match_indices(needle).any(|(i, _)| {
            let before_ok = haystack[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word(c));
            let after_ok = haystack[i + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| !is_word(c));
            before_ok && after_ok
        })
    }

    /// Drift guard: every keyword in the grammar (alphabetic anonymous node
    /// kind) must be captured by highlights.scm AND matched by the TextMate
    /// fallback. Catches the `pub`/`init` failure mode where a keyword lands in
    /// the grammar but never gets highlighting.
    #[test]
    fn every_grammar_keyword_is_highlighted() {
        let lang = mirin_compiler::language();
        let tm = tm_grammar();
        let mut missing = Vec::new();
        for id in 0..lang.node_kind_count() as u16 {
            // Visible anonymous alphabetic kinds are exactly the keywords;
            // hidden kinds (`_repeat1` helpers, `end`) are parser internals.
            if lang.node_kind_is_named(id) || !lang.node_kind_is_visible(id) {
                continue;
            }
            let Some(kind) = lang.node_kind_for_id(id) else {
                continue;
            };
            if !kind.starts_with(|c: char| c.is_ascii_alphabetic()) {
                continue;
            }
            if !HIGHLIGHTS.contains(&format!("\"{kind}\"")) {
                missing.push(format!("highlights.scm: `{kind}`"));
            }
            if !contains_word(&tm, kind) {
                missing.push(format!("mirin.tmLanguage.json: `{kind}`"));
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "keywords without highlighting: {missing:?}"
        );
    }

    /// Drift guard: every builtin type the language seeds into the prelude must
    /// be highlighted by both grammars (the `sint` failure mode).
    #[test]
    fn every_builtin_type_is_highlighted() {
        let tm = tm_grammar();
        let mut missing = Vec::new();
        for name in mirin_compiler::builtin_type_names() {
            if !HIGHLIGHTS.contains(&format!("\"{name}\"")) {
                missing.push(format!("highlights.scm: `{name}`"));
            }
            if !contains_word(&tm, name) {
                missing.push(format!("mirin.tmLanguage.json: `{name}`"));
            }
        }
        assert!(
            missing.is_empty(),
            "builtin types without highlighting: {missing:?}"
        );
    }

    #[test]
    fn recent_keywords_and_builtins_get_expected_tokens() {
        // `pub` (modifier), `init` (keyword), `sint` (type), `high` (constant).
        let src = "pub fn smoke {dom clk: Clock, rstn: Reset @clk = high} (a: sint(8) @clk) \
                   -> sint(8) @clk {\n    var acc: sint(8) @clk;\n    \
                   acc = init 0 when clk.posedge() { acc + a };\n    acc\n}\n";
        let doc = Document::open(src);
        assert!(
            !doc.tree.root_node().has_error(),
            "smoke snippet must parse cleanly"
        );
        let toks = compute(&doc.rope, &doc.tree, &query(), Encoding::Utf8);
        let decoded = decode(&toks);
        let has = |len: u32, ty: u32| decoded.iter().any(|&(_, _, l, t)| l == len && t == ty);
        assert!(has(3, 7), "`pub` should be a modifier token: {decoded:?}");
        assert!(has(4, 6), "`init` should be a keyword token: {decoded:?}");
        assert!(has(4, 1), "`sint` should be a type token: {decoded:?}");
        assert!(has(4, 11), "`high` should be a constant token: {decoded:?}");
    }

    #[test]
    fn tokens_are_sorted_and_non_overlapping() {
        let doc = Document::open(ADD_CONSTANT);
        let toks = compute(&doc.rope, &doc.tree, &query(), Encoding::Utf8);
        let decoded = decode(&toks);
        let mut prev = (0, 0);
        for &(line, start, len, _) in &decoded {
            assert!(
                (line, start) >= prev,
                "tokens out of order at {:?} (prev {:?})",
                (line, start),
                prev
            );
            assert!(len > 0, "zero-length token");
            prev = (line, start + len);
        }
        assert!(!decoded.is_empty());
    }
}
