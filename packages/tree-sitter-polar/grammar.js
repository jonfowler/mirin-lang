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

  conflicts: ($) => [
    [$.named_argument_list, $.record_literal],
  ],

  rules: {
    source_file: ($) => repeat($._item),

    comment: () => token(seq("//", /.*/)),

    _item: ($) =>
      choice(
        $.component_definition,
        $.struct_definition,
        $.port_definition,
        $.impl_block,
      ),

    component_definition: ($) =>
      seq(
        "cmp",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        optional(field("parameters", $.parameter_section)),
        optional(seq("->", field("return_type", $.type_expression))),
        field("body", $.block),
      ),

    struct_definition: ($) =>
      seq(
        "struct",
        field("name", $.identifier),
        optional(field("parameters", $.parameter_section)),
        optional(seq("=", field("constructor", $.identifier))),
        field("body", $.record_type_body),
      ),

    port_definition: ($) =>
      seq(
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
        "fn",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        field("parameters", $.parameter_section),
        optional(seq("->", field("return_type", $.type_expression))),
        field("body", $.block),
      ),

    named_parameter_section: ($) => seq("{", commaSep($.named_parameter), optional(","), "}"),

    parameter_section: ($) => seq("(", commaSep($.parameter), optional(","), ")"),

    named_parameter: ($) =>
      prec(
        1,
        seq(
        optional(field("inferable", "#")),
        optional(field("const", "const")),
        field("name", $.identifier),
        optional(seq(":", field("type", $.type_expression))),
        optional(seq("=", field("default", $.expression))),
        ),
      ),

    parameter: ($) =>
      seq(
        optional(field("direction", choice("in", "out"))),
        optional(field("inferable", "#")),
        optional(field("const", "const")),
        field("name", $.identifier),
        ":",
        field("type", $.type_expression),
        optional(seq("=", field("default", $.expression))),
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

    block: ($) => seq("{", repeat($.statement), "}"),

    statement: ($) =>
      choice(
        $.let_statement,
        $.return_statement,
        $.rec_statement,
        $.assignment_statement,
        $.expression_statement,
      ),

    let_statement: ($) =>
      seq("let", field("name", $.identifier), "=", field("value", $.expression), ";"),

    return_statement: ($) => seq("return", field("value", $.expression), ";"),

    rec_statement: ($) =>
      seq("rec", field("name", $.identifier), "=", field("body", $.block)),

    assignment_statement: ($) =>
      seq(field("left", $.expression), "=", field("right", $.expression), ";"),

    expression_statement: ($) => seq($.expression, ";"),

    type_expression: ($) =>
      prec.right(
        seq(
        field("name", $.identifier),
        repeat(choice($.type_index, $.type_named_arguments, $.type_arguments)),
        optional(seq("@", field("domain", $.identifier))),
        ),
      ),

    type_index: ($) => seq("[", field("index", $.expression), "]"),

    type_named_arguments: ($) =>
      seq("{", commaSep($.named_or_shorthand_argument), optional(","), "}"),

    type_arguments: ($) => seq("(", commaSep($.type_expression), optional(","), ")"),

    expression: ($) =>
      choice(
        $.binary_expression,
        $.postfix_expression,
        $.record_constructor_expression,
        $.path_expression,
        $.identifier,
        $.number,
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
          repeat1(choice($.field_access, $.named_argument_list, $.argument_list, $.slice_expression)),
        ),
      ),

    field_access: ($) => seq(".", field("field", $.identifier)),

    slice_expression: ($) =>
      seq("[", field("start", $.expression), ":", field("end", $.expression), "]"),

    named_argument_list: ($) =>
      seq("{", commaSep($.named_or_shorthand_argument), optional(","), "}"),

    named_or_shorthand_argument: ($) =>
      choice(
        seq(field("name", $.identifier), "=", field("value", $.expression)),
        field("name", $.identifier),
      ),

    argument_list: ($) => seq("(", commaSep($.expression), optional(","), ")"),

    record_constructor_expression: ($) =>
      seq(field("constructor", $.identifier), field("body", $.record_literal)),

    record_literal: ($) => seq("{", commaSep($.record_field_value), optional(","), "}"),

    record_field_value: ($) =>
      seq(field("name", $.identifier), ":", field("value", $.expression)),

    path_expression: ($) =>
      seq(field("type", $.identifier), "::", field("member", $.identifier)),

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
