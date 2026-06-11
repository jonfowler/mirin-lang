const PREC = {
  assign: 1,
  additive: 2,
  multiplicative: 3,
  postfix: 4,
};

module.exports = grammar({
  name: "polar",

  extras: ($) => [/\s/, $.comment],

  // The raw body of `= verilog { … }` — one token from src/scanner.c
  // (brace-counting, string/comment aware). See planning/inline_verilog.md.
  externals: ($) => [$.verilog_content],

  word: ($) => $.identifier,

  conflicts: ($) => [
    [$.use_path],
    // `x { … }` in expression position is ambiguous: a record constructor
    // (`x { a: 1 }`) or a path `x` followed by a block. GLR resolves it by the
    // brace contents — a valid record literal wins. (`if`/`when` conditions
    // dodge this entirely by restricting their expression form.)
    [$.path_expression, $.record_constructor_expression],
  ],

  rules: {
    source_file: ($) => repeat($._item),

    comment: () => token(seq("//", /.*/)),

    _item: ($) =>
      choice(
        $.function_definition,
        $.struct_definition,
        $.port_definition,
        $.impl_block,
        $.module_definition,
        $.use_declaration,
      ),

    // `use` imports. Paths are 2018-style relative; `crate`/`super`/`self`
    // anchors are ordinary identifier segments recognised by the resolver.
    //   use a::b::c;   use a::b as d;   use a::{b, c::{d, e}};   use a::*;
    use_declaration: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "use",
        field("tree", $.use_tree),
        ";",
      ),

    use_tree: ($) =>
      choice(
        seq($.use_path, optional(seq("as", field("alias", $.identifier)))),
        seq(optional(seq($.use_path, "::")), field("group", $.use_group)),
        seq(optional(seq($.use_path, "::")), field("glob", "*")),
      ),

    use_path: ($) => seq($.identifier, repeat(seq("::", $.identifier))),

    use_group: ($) => seq("{", commaSep($.use_tree), optional(","), "}"),

    // Item visibility. Default (absent) is private. The parenthesised forms
    // narrow a public item's reach.
    //   pub   pub(crate)   pub(super)   pub(in a::b)
    visibility_modifier: ($) =>
      seq(
        "pub",
        optional(
          seq("(", choice("crate", "super", seq("in", $.use_path)), ")"),
        ),
      ),

    // Module declaration. `mod foo { items… }` nests an inline body (modules
    // nest arbitrarily); `mod foo;` loads the body from `foo.plr` at load time.
    module_definition: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "mod",
        field("name", $.identifier),
        choice(field("body", $.module_body), ";"),
      ),

    module_body: ($) => seq("{", repeat($._item), "}"),

    // The constructor name (`= ctor`) is mandatory. A struct/port introduces
    // *two* names — the type and its constructor — and they share one namespace,
    // so they must differ (`struct S = S { … }` is a duplicate-name error,
    // diagnosed by name resolution). Requiring `= ctor` keeps the type/term
    // split explicit at the syntax level. See `planning/syntax.md`.
    struct_definition: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "struct",
        field("name", $.identifier),
        optional(field("parameters", $.parameter_section)),
        "=",
        field("constructor", $.identifier),
        field("body", $.record_type_body),
      ),

    port_definition: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "port",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        optional(field("parameters", $.parameter_section)),
        "=",
        field("constructor", $.identifier),
        field("body", $.port_body),
      ),

    // Binder-first: `impl {dom clk: Clock} Stream8 { … }` — the braces after
    // `impl` DECLARE generics; the owner is applied implicitly (via `self @clk`
    // etc.), never with application braces of its own.
    impl_block: ($) =>
      seq(
        "impl",
        optional(field("named_parameters", $.named_parameter_section)),
        field("name", $.identifier),
        field("body", $.impl_body),
      ),

    impl_body: ($) => seq("{", repeat($.function_definition), "}"),

    function_definition: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "fn",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        field("parameters", $.parameter_section),
        optional(seq("->", field("return_type", $.return_type_expression))),
        choice(
          field("body", $.block),
          seq("=", "verilog", field("verilog_body", $.verilog_block)),
        ),
      ),

    // An inline-verilog fn body: the signature is the contract, the raw
    // text is spliced into the emitted module (`planning/inline_verilog.md`).
    verilog_block: ($) => seq("{", field("content", $.verilog_content), "}"),

    // Return-position type. Excludes `type_named_args` because a trailing
    // `{` in return position opens the fn body, not a named-arg-application.
    // Use parentheses to write a parametric return type (`-> DF(uint(8))`)
    // and call sites supply the named bindings positionally elsewhere.
    return_type_expression: ($) =>
      prec.right(
        seq(
          field("name", $.identifier),
          optional($.type_index),
          optional(seq("@", field("domain", $.identifier))),
        ),
      ),

    named_parameter_section: ($) => seq("{", commaSep($.named_parameter), optional(","), "}"),

    parameter_section: ($) => seq("(", commaSep($.parameter), optional(","), ")"),

    named_parameter: ($) =>
      prec(
        1,
        seq(
          optional(field("direction", choice("in", "out"))),
          optional(field("kind", choice("param", "dom"))),
          field("name", $.identifier),
          optional(seq(":", field("type", $.type_expression))),
          optional(seq("=", field("default", $.expression))),
        ),
      ),

    parameter: ($) =>
      choice(
        seq(
          field("name", "self"),
          optional(seq("@", field("domain", $.identifier))),
        ),
        seq(
          optional(field("direction", choice("in", "out"))),
          optional(field("kind", choice("param", "dom"))),
          field("name", $.identifier),
          ":",
          field("type", $.type_expression),
          optional(seq("=", field("default", $.expression))),
        ),
      ),

    record_type_body: ($) =>
      seq("{", commaSep($.record_field_type), optional(","), "}"),

    port_body: ($) => seq("{", commaSep($.port_field), optional(","), "}"),

    record_field_type: ($) =>
      seq(field("name", $.identifier), ":", field("type", $.type_expression)),

    port_field: ($) =>
      seq(
        field("direction", choice("in", "out")),
        field("name", $.identifier),
        ":",
        field("type", $.type_expression),
      ),

    // `{ stmt; ...; tail }` — used both as a function/if/when body and in
    // expression position. The tail (if present) is the block's value;
    // statements without a tail evaluate to unit-style (no value), which the
    // type-checker rejects in value contexts.
    block: ($) =>
      seq("{", repeat($.statement), optional(field("tail", $.expression)), "}"),

    statement: ($) =>
      choice(
        $.let_statement,
        $.return_statement,
        $.var_statement,
        $.assignment_statement,
        $.expression_statement,
      ),

    let_statement: ($) =>
      seq(
        "let",
        field("name", $.identifier),
        optional(seq(":", field("type", $.type_expression))),
        "=",
        field("value", $.expression),
        ";",
      ),

    return_statement: ($) => seq("return", field("value", $.expression), ";"),

    var_statement: ($) =>
      seq(
        "var",
        commaSep1(field("name", $.identifier)),
        optional(seq(":", field("type", $.type_expression))),
        optional(seq("=", field("value", $.expression))),
        ";",
      ),

    assignment_statement: ($) =>
      seq(field("left", $.expression), "=", field("right", $.expression), ";"),

    expression_statement: ($) => seq($.expression, ";"),

    type_expression: ($) =>
      prec.right(
        seq(
          field("name", $.identifier),
          optional($.type_named_args),
          optional($.type_index),
          optional(seq("@", field("domain", $.identifier))),
        ),
      ),

    // Named-section type arguments — supply the def's named-section
    // generic params (typically `dom clk: Clock`). Example: `DF{clk}` binds
    // the port's `dom clk` to the local `clk`. The contents are
    // type arguments in declared order, just like the positional `(…)`
    // section, but applied to the def's named params.
    type_named_args: ($) => seq("{", commaSep1($.type_argument), "}"),

    // Parenthesised list of type arguments after a type name:
    //   uint(8)          — width literal (Number)
    //   uint(N)          — width as a `param N: integer` identifier (TypeExpression)
    //   uint(N + 1)      — width arithmetic (ConstExpression)
    //   uint(cfg.bits)   — const field projection (ConstExpression)
    //   Bus(uint(8))     — type argument (TypeExpression)
    //   Bus(A)           — type-kinded parameter reference (TypeExpression)
    type_index: ($) => seq("(", commaSep1($.type_argument), ")"),

    type_argument: ($) => choice($.type_expression, $.number, $.const_expression),

    // The restricted const grammar in type positions: arithmetic over
    // literals, names, and field projections. Anything bigger (a call, an
    // if/else) goes through a `let`: `let w = f(n); uint(w)`. A *bare* name
    // stays a type_expression (the lowerer decides type vs const by kind).
    const_expression: ($) => choice($.const_binary, $.const_field, $.const_paren),

    const_field: ($) =>
      seq(
        field("base", $.identifier),
        repeat1(seq(".", field("field", $.identifier))),
      ),

    const_binary: ($) =>
      choice(
        prec.left(
          PREC.multiplicative,
          seq(
            field("left", $._const_operand),
            field("operator", "*"),
            field("right", $._const_operand),
          ),
        ),
        prec.left(
          PREC.additive,
          seq(
            field("left", $._const_operand),
            field("operator", choice("+", "-")),
            field("right", $._const_operand),
          ),
        ),
      ),

    const_paren: ($) => seq("(", $._const_operand, ")"),

    _const_operand: ($) =>
      choice($.number, $.identifier, $.const_field, $.const_binary, $.const_paren),

    expression: ($) =>
      choice(
        $.binary_expression,
        $.postfix_expression,
        $.record_constructor_expression,
        $.path_expression,
        $.number,
        $.parenthesized_expression,
        $.block,
        $.if_expression,
        $.when_expression,
      ),

    // Rust-style `if cond { … } else { … }`. Both branches are required;
    // Polar has no statement-form `if`. The two branches must produce the
    // same type, which is the if-expression's type. The condition uses
    // `_expression_no_struct` so that `if Foo { x: 1 } { … }` doesn't
    // parse `Foo { x: 1 }` as a record-constructor (Rust does the same).
    if_expression: ($) =>
      seq(
        "if",
        field("condition", $._if_condition),
        field("then_branch", $.block),
        "else",
        field("else_branch", $.block),
      ),

    // The condition of an `if` is strictly limited so a trailing `{ … }`
    // can't be ambiguously parsed as a record-constructor or a postfix
    // named-args step. For any condition shape outside this set, wrap it
    // in parens — the parenthesized form lets the inner expression be
    // anything because `)` terminates the cond before the block opens.
    _if_condition: ($) =>
      choice(
        $.number,
        $.path_expression,
        $.parenthesized_expression,
      ),

    // `when EVENT { … }` — Polar's primitive for registered state. EVENT
    // is conventionally `clk.posedge()`, but any expression yielding a
    // value of type `Event @D` works. The event slot uses the same
    // restricted form as if-conditions for the same reason: a trailing
    // `{` opens the body block, so the event expression must end before
    // the parser sees `{`.
    when_expression: ($) =>
      seq(
        "when",
        field("event", $._when_event),
        field("body", $.block),
      ),

    // Restricted event expression. A field-access chain (`clk.posedge()`)
    // is the common case; complex events go in parens. Note: we DO allow
    // `postfix_expression` here because the common case `clk.posedge()`
    // is a postfix that ends with `)` — the parser can tell where the
    // event ends before `{ … }` begins.
    _when_event: ($) =>
      choice(
        $.path_expression,
        $.postfix_expression,
        $.parenthesized_expression,
      ),

    binary_expression: ($) =>
      choice(
        prec.left(
          PREC.multiplicative,
          seq(field("left", $.expression), field("operator", "*"), field("right", $.expression)),
        ),
        prec.left(
          PREC.additive,
          seq(
            field("left", $.expression),
            field("operator", choice("+", "-")),
            field("right", $.expression),
          ),
        ),
      ),

    postfix_expression: ($) =>
      prec.left(
        PREC.postfix,
        seq(
          field(
            "receiver",
            choice($.path_expression, $.number, $.parenthesized_expression),
          ),
          repeat1(
            choice(
              $.field_access,
              seq($.named_argument_list, $.argument_list),
              $.argument_list,
            ),
          ),
        ),
      ),

    field_access: ($) => seq(".", field("field", $.identifier)),

    named_argument_list: ($) =>
      seq("{", commaSep($.named_or_shorthand_argument), optional(","), "}"),

    named_or_shorthand_argument: ($) =>
      choice(
        seq(
          optional(field("direction", choice("in", "out"))),
          field("name", $.identifier),
          "=>",
          field("target", $.identifier),
        ),
        seq(
          optional(field("direction", choice("in", "out"))),
          field("name", $.identifier),
          "=",
          field("value", $.expression),
        ),
        field("name", $.identifier),
      ),

    argument_list: ($) =>
      seq(
        "(",
        commaSep(choice($.expression, $.out_argument)),
        optional(","),
        ")",
      ),

    // Positional out-arg: `[out] => target`. Binds a caller-side local to
    // an `out`-direction positional parameter on the callee. The `out`
    // keyword is optional — `=>` unambiguously means source-arrow.
    out_argument: ($) =>
      seq(
        optional(field("direction", "out")),
        "=>",
        field("target", $.identifier),
      ),

    record_constructor_expression: ($) =>
      seq(field("constructor", $.identifier), field("body", $.record_literal)),

    record_literal: ($) => seq("{", commaSep($.record_field_value), optional(","), "}"),

    // `name = value` supplies a field; `name => target` binds an
    // opposite-direction field of a constructed *port* to a local (the
    // record-literal analogue of a named-arg out-connection). `=` matches
    // named parameters/arguments — `:` always means "type", `=`/`=>`
    // always mean "value/connection".
    record_field_value: ($) =>
      choice(
        seq(field("name", $.identifier), "=", field("value", $.expression)),
        seq(field("name", $.identifier), "=>", field("target", $.identifier)),
      ),

    // A name reference: `a`, `a::b::c`, `crate::m::f`, `super::x`. 1+ segments
    // — a bare name is a single-segment path (there is no separate identifier
    // expression; the lowering decides bare-name vs path by segment count).
    // `crate`/`super`/`self` anchors are ordinary identifier segments handled
    // by the resolver.
    path_expression: ($) =>
      seq(
        field("segment", $.identifier),
        repeat(seq("::", field("segment", $.identifier))),
      ),

    parenthesized_expression: ($) => seq("(", $.expression, ")"),

    identifier: () => /[A-Za-z_][A-Za-z0-9_]*/,
    number: () => /[0-9]+/,
  },
});

function commaSep(rule) {
  return optional(commaSep1(rule));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)));
}
