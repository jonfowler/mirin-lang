
module.exports = grammar({
  name: 'polar',

  extras: $ => [
    /\s/, // Whitespace (spaces, tabs, newlines)
    $.line_comment,
  ],

  rules: {
    // The top-level rule: a source file can contain one or more function items.
    source_file: $ => repeat1($.function_item),

    // Function definition: fn name ( params ) -> return_type { body }
    function_item: $ => seq(
      'fn',
      field('name', $.identifier),
      field('parameters', $.parameters),
      optional(seq('->', field('return_type', $.type))),
      field('body', $.block)
    ),

    // Parameter list: (param1, param2, ...)
    parameters: $ => seq(
      '(',
      optional(sepBy(',', $.parameter)),
      ')'
    ),

    // Single parameter: name: type
    parameter: $ => seq(
      field('name', $.identifier),
      ':',
      field('type', $.type)
    ),

    // Type annotation: base_type (@ domain_name)?
    type: $ => seq(
      field('base_type', $._type_identifier), // e.g., "u8", "clock"
      optional(field('domain_tag', $.domain_tag))
    ),

    // Domain tag: @ domain_name
    domain_tag: $ => seq(
      '@',
      field('name', $.identifier) // e.g., "clk"
    ),

    // Block: { statement* }
    block: $ => seq(
      '{',
      repeat($._statement),
      '}'
    ),

    // A statement can be a let declaration or a return statement.
    _statement: $ => choice(
      $.let_declaration,
      $.return_statement
      // Future: expression_statement, if_statement, etc.
    ),

    // Let declaration: let name : type = expression ;
    let_declaration: $ => seq(
      'let',
      field('name', $.identifier),
      ':',
      field('type', $.type),
      '=',
      field('initializer', $._expression),
      ';'
    ),

    // Return statement: return expression ;
    return_statement: $ => seq(
      'return',
      field('value', $._expression),
      ';'
    ),

    // Expressions
    // This is a simplified expression model. Real-world languages have more complex precedence.
    _expression: $ => choice(
      $.identifier,
      $.binary_expression,
      $.call_expression,
      $.field_expression
      // Future: $.literal, $.parenthesized_expression, $.unary_expression, etc.
    ),

    // Binary expression: left_operand operator right_operand
    // `prec.left(1, ...)` means left-associative with precedence 1.
    binary_expression: $ => prec.left(1, seq(
      field('left', $._expression),
      field('operator', $.operator_identifier), // For now, only supporting ' + '
      field('right', $._expression)
    )),

    operator_identifier: $ => /[+\-%<>*&|^~!]+/,

    // Field expression: object.field
    // e.g., `c.flipflop` (this is the field access part)
    // `prec(3, ...)` gives it higher precedence than binary operators.
    field_expression: $ => prec(3, seq(
      field('object', $._expression),
      '.',
      field('field', $.identifier)
    )),

    // Call expression: function_expr()
    // e.g., `c.flipflop()` or `some_function()`
    // `prec(2, ...)` ensures it binds correctly. `function` can be an identifier or a field_expression.
    call_expression: $ => prec(2, seq(
      field('function', $._expression), // This can be an identifier or a field_expression like `c.flipflop`
      '(',
      // TODO: Add rule for arguments: optional(sepBy(',', $._expression))
      ')'
    )),

    // Identifier: typical programming language identifier
    identifier: _ => /[_\p{XID_Start}][_\p{XID_Continue}]*/,

    _type_identifier: $ => alias($.identifier, $.type_identifier),
    _field_identifier: $ => alias($.identifier, $.field_identifier),

    // Line comment: // ...
    line_comment: $ => token(seq('//', /.*/)),
  }
});

// Helper function for comma-separated lists (optional)
function sepBy(sep, rule) {
  return optional(sepBy1(sep, rule));
}

// Helper function for comma-separated lists (at least one)
function sepBy1(sep, rule) {
  return seq(rule, repeat(seq(sep, rule)));
}
