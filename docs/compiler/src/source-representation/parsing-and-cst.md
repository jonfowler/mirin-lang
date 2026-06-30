# Parsing and the CST

The first thing the compiler does with source text is parse it into a **concrete
syntax tree** — a CST that mirrors the source exactly, down to the whitespace.
Mirin parses with [tree-sitter](https://tree-sitter.github.io/), and the parse is
infallible: every input produces a tree, valid or not. This chapter covers the
grammar and the parser, what the CST is and why it keeps everything, the error
recovery that lets a half-written file still parse, and why — alone among the
compiler's representations — the CST is never cached.

## The grammar and the parser

Mirin's grammar lives in `packages/tree-sitter-mirin`, written in JavaScript and
compiled to C. The crate's `build.rs` compiles that C and links it in, so the
grammar ships inside the compiler binary. The Rust side is thin: `src/base/parser.rs`
wraps the linked grammar as a tree-sitter `Language` and exposes one function,
`parse_text(text) -> Tree`.

The CST it produces is a faithful tree of the source. It keeps exact byte ranges
and trivia, which is what lets layout-sensitive tooling — the formatter, the
syntax highlighter, the language server — work off the same parse the compiler
uses. You can see the tree for any file with `--emit cst`:

```console
$ cargo run -p mirin-compiler -- --emit cst inc.mrn
```

For the one-line program `fn inc(a: uint(8)) -> uint(8) { a + 1 }`, that prints
(reindented here for reading):

```
(source_file
  (function_definition
    name: (identifier)
    parameters: (parameter_section
      (parameter name: (identifier)
        type: (type_expression name: (identifier)
          (type_index (type_argument (number))))))
    return_type: (return_type_expression name: (identifier)
      (type_index (type_argument (number))))
    body: (block
      tail: (expression
        (binary_expression
          left: (expression (path_expression segment: (identifier)))
          right: (expression (number)))))))
```

Every node carries the source range it spans, so the compiler can point a
diagnostic at the exact characters that caused it.

## An infallible, error-recovering parse

`parse_text` always returns a tree. Tree-sitter never fails: given invalid input,
it recovers and marks the trouble with `ERROR` and `MISSING` nodes rather than
giving up. This is the property that makes tree-sitter a good editor frontend — a
file mid-keystroke is rarely valid, and the compiler still needs a tree to work
from.

Those error nodes are how the compiler reports syntax problems. The
`syntax_errors(file)` query walks the tree, collects each `ERROR` and `MISSING`
node as a diagnostic, and also flags a few lexically-valid-but-rejected cases
such as a reserved word used as a binding name. Recovery is coarse — one
diagnostic per trouble spot — but it is enough to keep the later phases running on
a partly-broken file, which is what an editor needs.

## Why the CST is transient

Every other representation in the compiler is a memoised query value. The CST is
not, for the reason [the query engine](../architecture/query-engine.md) gave: a tree-sitter
`Tree` is owned by the C library and has no structural equality the engine can
use for backdating, so it cannot be a tracked value. Parsing is therefore a cheap
transient *inside* the queries that need a tree. Each of those queries re-parses
and immediately lifts an owned, comparable summary from the tree, then lets the
tree drop.

Two such summaries come straight off the CST, and they are the next two chapters:
a [stable identity and item skeleton](item-tree.md) for every item in the file,
and — built on those — [the crate's resolved names](name-resolution.md). The CST
is the raw material; everything downstream works from the summaries, not the tree.
