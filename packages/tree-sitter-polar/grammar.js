const PREC = {
  assign: 1,
  additive: 2,
  multiplicative: 3,
  postfix: 4,
};

module.exports = grammar({
  name: "polar",

  extras: ($) => [/\s/, $.comment],

  word: ($) => $.identifier,

  conflicts: ($) => [[$.use_path]],

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

    struct_definition: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "struct",
        field("name", $.identifier),
        optional(field("parameters", $.parameter_section)),
        optional(seq("=", field("constructor", $.identifier))),
        field("body", $.record_type_body),
      ),

    port_definition: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "port",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        optional(field("parameters", $.parameter_section)),
        optional(seq("=", field("constructor", $.identifier))),
        field("body", $.port_body),
      ),

    impl_block: ($) =>
      seq(
        "impl",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        optional(field("parameters", $.parameter_section)),
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
        field("body", $.block),
      ),

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
      seq("let", field("name", $.identifier), "=", field("value", $.expression), ";"),

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
    //   uint(8)        — width literal (Number)
    //   uint(N)        — width as a `param N: usize` identifier (TypeExpression)
    //   Bus(uint(8))   — type argument (TypeExpression)
    //   Bus(A)         — type-kinded parameter reference (TypeExpression)
    // For now, args are bare type expressions or number literals; arithmetic
    // widths (`uint(N+1)`) are not yet in scope.
    type_index: ($) => seq("(", commaSep1($.type_argument), ")"),

    type_argument: ($) => choice($.type_expression, $.number),

    expression: ($) =>
      choice(
        $.binary_expression,
        $.postfix_expression,
        $.record_constructor_expression,
        $.path_expression,
        $.identifier,
        $.number,
        $.parenthesized_expression,
        $.block_expression,
        $.if_expression,
        $.when_expression,
      ),

    // `block_expression` is a block used in expression position. Same shape
    // as a function body — `{ stmt; ...; tail }`. The tail (if present) is
    // the block's value. Statements without a tail evaluate to unit-style
    // (no value), which the type-checker rejects in value contexts.
    block_expression: ($) =>
      seq("{", repeat($.statement), optional(field("tail", $.expression)), "}"),

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
        $.identifier,
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
        $.identifier,
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
          seq(field("left", $.expression), field("operator", "+"), field("right", $.expression)),
        ),
      ),

    postfix_expression: ($) =>
      prec.left(
        PREC.postfix,
        seq(
          field(
            "receiver",
            choice($.path_expression, $.identifier, $.number, $.parenthesized_expression),
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

    record_field_value: ($) =>
      seq(field("name", $.identifier), ":", field("value", $.expression)),

    // Multi-segment path: `a::b::c`, `crate::m::f`, `super::x`. Always ≥2
    // segments (a single name is an `identifier`). `crate`/`super`/`self`
    // anchors are ordinary identifier segments handled by the resolver.
    path_expression: ($) =>
      seq(
        field("segment", $.identifier),
        repeat1(seq("::", field("segment", $.identifier))),
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
