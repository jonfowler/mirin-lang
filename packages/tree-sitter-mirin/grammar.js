const PREC = {
  assign: 1,
  logical_or: 2,
  logical_and: 3,
  comparison: 4,
  bitwise_or: 5,
  bitwise_xor: 6,
  bitwise_and: 7,
  shift: 8,
  additive: 9,
  multiplicative: 10,
  unary: 11,
  postfix: 12,
};

module.exports = grammar({
  name: "mirin",

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
    // `uint(6)::4` vs `a::b`: a bare identifier before `::` could open either
    // a typed_literal's type or a path — GLR decides at the next token (a
    // number ends a typed_literal; an identifier continues a path).
    [$.return_type_expression, $.path_expression],
    // Header positions (if/for/when): after a path, `{` is either the body
    // block (path = the whole header) or a method's named-argument list
    // (which the grammar REQUIRES to be followed by `(…)`). GLR keeps both
    // alive; the named-args fork dies at body content or the missing parens.
    [$._header_expression, $.header_postfix],
    [$.header_postfix],
    // `return …`: the bare `return` (a referrable place — return_expression)
    // and the `return EXPR;` statement both start with `return`. GLR keeps
    // both alive and resolves at the next token (`.`/`=`/`[` → place; an
    // expression start → the whole-result statement).
    [$.return_statement, $.return_expression],
  ],

  rules: {
    source_file: ($) => repeat($._item),

    comment: () => token(seq("//", /.*/)),

    _item: ($) =>
      choice(
        $.function_definition,
        $.struct_definition,
        $.port_definition,
        $.trait_definition,
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
    // nest arbitrarily); `mod foo;` loads the body from `foo.mrn` at load time.
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
        optional(field("named_parameters", $.named_parameter_section)),
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

    // A trait declaration: method signatures (no bodies) and associated
    // const declarations. `Self` is an ordinary identifier, resolved during
    // lowering. See planning/traits.md.
    trait_definition: ($) =>
      seq(
        optional(field("visibility", $.visibility_modifier)),
        "trait",
        field("name", $.identifier),
        field("body", $.trait_body),
      ),

    trait_body: ($) =>
      seq("{", repeat(choice($.trait_method, $.trait_const)), "}"),

    trait_method: ($) =>
      seq(
        "fn",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        field("parameters", $.parameter_section),
        optional(
          seq(
            "->",
            field(
              "return_type",
              choice($.return_type_expression, $.tuple_type, $.named_return),
            ),
          ),
        ),
        ";",
      ),

    trait_const: ($) =>
      seq(
        "const",
        field("name", $.identifier),
        ":",
        field("type", $._type),
        ";",
      ),

    // Binder-first: `impl {dom clk: Clock} Stream8 { … }` — the braces after
    // `impl` DECLARE generics. A generic owner is APPLIED (`impl {dom clk: Clock,
    // A: Type} Bus(A) { … }`), the same way a trait impl writes its self type
    // (`impl {param n: integer} Add for uint(n) { … }`); both use the restricted
    // (no named-args) type form so a trailing `{` opens the body. For an
    // inherent impl the subject IS the self type; for a trait impl it is the
    // trait and `for` introduces the self type.
    // Outer attributes (`#[inline]`, `#[derive(BitPack)]`) on an item, Rust
    // shape. v1 carries them on `fn` and `impl` items (planning/attributes.md);
    // semantics are attached by later passes (`#[inline]` emission).
    attribute: ($) =>
      seq(
        "#",
        "[",
        field("name", $.identifier),
        optional(field("arguments", $.attribute_arguments)),
        "]",
      ),

    attribute_arguments: ($) => seq("(", commaSep1($.identifier), optional(","), ")"),

    impl_block: ($) =>
      seq(
        repeat(field("attribute", $.attribute)),
        "impl",
        optional(field("named_parameters", $.named_parameter_section)),
        field("name", $.return_type_expression),
        optional(seq("for", field("self_type", $.return_type_expression))),
        optional(field("where", $.where_clause)),
        field("body", $.impl_body),
      ),

    impl_body: ($) =>
      seq("{", repeat(choice($.function_definition, $.impl_const)), "}"),

    // An associated const's value in an impl. The value is a const
    // expression; semantics land with associated consts (planning/traits.md
    // T4) — the grammar carries them from the start.
    impl_const: ($) =>
      seq(
        "const",
        field("name", $.identifier),
        ":",
        field("type", $._type),
        "=",
        field("value", $.expression),
        ";",
      ),

    // `where T: Add + Bits, U: Bits` — trait-bound predicates on generic
    // params. Bounds use the restricted form (name + optional positional
    // args): a brace after a bound would open the item's body. Domain
    // predicates (`T @ clk`) join when planning/domain_checking.md
    // lands.
    where_clause: ($) =>
      seq("where", commaSep1($.where_predicate), optional(",")),

    where_predicate: ($) =>
      seq(
        field("name", $.identifier),
        ":",
        field("bound", $.trait_bound),
        repeat(seq("+", field("bound", $.trait_bound))),
      ),

    trait_bound: ($) =>
      prec.right(seq(field("name", $.identifier), optional($.type_index))),

    function_definition: ($) =>
      seq(
        repeat(field("attribute", $.attribute)),
        optional(field("visibility", $.visibility_modifier)),
        "fn",
        field("name", $.identifier),
        optional(field("named_parameters", $.named_parameter_section)),
        field("parameters", $.parameter_section),
        optional(
          seq(
            "->",
            field(
              "return_type",
              choice($.return_type_expression, $.tuple_type, $.named_return),
            ),
          ),
        ),
        optional(field("where", $.where_clause)),
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

    // Named result(s): `-> (output: DF @clk)` names the whole result;
    // `-> (sum: uint(8), carry: bool)` names the parts of a tuple result. The
    // names become referrable result places in the body (the `return` keyword
    // doesn't exist when results are named). A single element is the result
    // type itself; two or more form a tuple (planning/return_variable.md). The
    // `:` distinguishes this from a bare tuple_type return `(A, B)`.
    named_return: ($) =>
      seq("(", commaSep1($.named_result), optional(","), ")"),

    named_result: ($) =>
      seq(field("name", $.identifier), ":", field("type", $._type)),

    named_parameter_section: ($) => seq("{", commaSep($.named_parameter), optional(","), "}"),

    parameter_section: ($) => seq("(", commaSep($.parameter), optional(","), ")"),

    named_parameter: ($) =>
      prec(
        1,
        seq(
          optional(field("direction", choice("in", "out"))),
          optional(field("kind", choice("param", "dom"))),
          field("name", $.identifier),
          optional(
            seq(
              ":",
              field("type", $._type),
              repeat(seq("+", field("bound", $.trait_bound))),
            ),
          ),
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
          field("type", $._type),
          repeat(seq("+", field("bound", $.trait_bound))),
          optional(seq("=", field("default", $.expression))),
        ),
      ),

    record_type_body: ($) =>
      seq("{", commaSep($.record_field_type), optional(","), "}"),

    port_body: ($) => seq("{", commaSep($.port_field), optional(","), "}"),

    record_field_type: ($) =>
      seq(field("name", $.identifier), ":", field("type", $._type)),

    port_field: ($) =>
      seq(
        field("direction", choice("in", "out")),
        field("name", $.identifier),
        ":",
        field("type", $._type),
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
        $.for_statement,
        $.return_statement,
        $.var_statement,
        $.assignment_statement,
        $.expression_statement,
      ),

    // `for x in v { … }` / `for (i, x) in v.enumerate() { … }` — structural
    // replication over a vector, emitted as a NAMED SV generate-for
    // (planning/for_loops.md). The binder is a pattern (planning/tuples.md);
    // the iterable uses the same restricted forms as if-conditions (a
    // trailing `{` opens the body).
    for_statement: ($) =>
      seq(
        "for",
        field("pattern", $._pattern),
        "in",
        field("iter", $._for_iterable),
        field("body", $.block),
      ),

    // HEADER expressions (if-conditions, for-iterables, when-events): the
    // full expression grammar minus BARE record literals — Rust's
    // no-struct-literal contexts. A record constructor in a header goes in
    // parens (which reset to the full grammar). Named-arg method calls
    // (`v.f{a}(x)`) survive because a named-arg list is always followed by
    // a positional list — the GLR fork resolves at the `(`.
    _header_expression: ($) =>
      choice(
        alias($.header_binary, $.binary_expression),
        alias($.header_unary, $.unary_expression),
        alias($.header_postfix, $.postfix_expression),
        $.typed_literal,
        $.vec_literal,
        $.path_expression,
        $.number,
        $.parenthesized_expression,
        $.tuple_expression,
      ),

    header_binary: ($) =>
      choice(
        prec.left(
          PREC.comparison,
          seq(
            field("left", $._header_expression),
            field("operator", choice("==", "!=", "<", "<=", ">", ">=")),
            field("right", $._header_expression),
          ),
        ),
        prec.left(
          PREC.additive,
          seq(
            field("left", $._header_expression),
            field("operator", choice("+", "-")),
            field("right", $._header_expression),
          ),
        ),
        prec.left(
          PREC.multiplicative,
          seq(
            field("left", $._header_expression),
            field("operator", choice("*", "/", "%")),
            field("right", $._header_expression),
          ),
        ),
      ),

    header_unary: ($) =>
      prec(
        PREC.unary,
        seq(field("operator", "-"), field("operand", $._header_expression)),
      ),

    header_postfix: ($) =>
      seq(
        field(
          "receiver",
          choice(
            $.path_expression,
            $.number,
            $.parenthesized_expression,
            $.tuple_expression,
          ),
        ),
        repeat1(
          choice(
            $.field_access,
            $.index_access,
            seq($.named_argument_list, $.argument_list),
            $.argument_list,
          ),
        ),
      ),

    _for_iterable: ($) => $._header_expression,

    let_statement: ($) =>
      seq(
        "let",
        field("pattern", $._pattern),
        optional(seq(":", field("type", $._type))),
        "=",
        field("value", $.expression),
        ";",
      ),

    // `(a, b)` / `(a, (b, c))` in binder position — destructuring
    // (planning/tuples.md). Identifiers only; lowering desugars patterns to
    // field projections, so there is no pattern IR. Arity ≥ 2.
    tuple_pattern: ($) =>
      seq("(", $._pattern, ",", commaSep1($._pattern), optional(","), ")"),

    // `pair { valid = vld, data = dat }` in binder position — struct
    // destructuring (planning/tuples.md). `name = binding` maps a field to a
    // (possibly nested) sub-pattern; `=` matches the record-literal field form
    // (`:` always means "type"). Like tuple patterns, this desugars to field
    // projection lets — no pattern IR. Only structs and positive tuples are
    // pattern-matchable; ports are not.
    struct_pattern: ($) =>
      seq(
        field("constructor", $.identifier),
        "{",
        commaSep1($.struct_pattern_field),
        optional(","),
        "}",
      ),

    struct_pattern_field: ($) =>
      seq(field("name", $.identifier), "=", field("binding", $._pattern)),

    _pattern: ($) => choice($.identifier, $.tuple_pattern, $.struct_pattern),

    return_statement: ($) => seq("return", field("value", $.expression), ";"),

    var_statement: ($) =>
      seq(
        "var",
        commaSep1(field("name", $.identifier)),
        optional(seq(":", field("type", $._type))),
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

    type_argument: ($) =>
      choice($.type_expression, $.tuple_type, $.number, $.const_expression),

    // Any type form: a named type or a tuple (planning/tuples.md).
    _type: ($) => choice($.type_expression, $.tuple_type),

    // `(A, B)` — a structural product type; each element may carry its own
    // domain, and a trailing `@clk` is the default for elements without one
    // (planning/tuples.md). Arity ≥ 2.
    tuple_type: ($) =>
      seq(
        "(",
        $._type,
        ",",
        commaSep1($._type),
        optional(","),
        ")",
        optional(seq("@", field("domain", $.identifier))),
      ),

    // The restricted const grammar in type positions: arithmetic over
    // literals, names, and field projections. Anything bigger (a call, an
    // if/else) goes through a `let`: `let w = f(n); uint(w)`. A *bare* name
    // stays a type_expression (the lowerer decides type vs const by kind).
    const_expression: ($) =>
      choice($.const_binary, $.const_field, $.const_path, $.const_paren),

    // `T::width` — an associated const projected from a bounded type param
    // (planning/traits.md T4).
    const_path: ($) =>
      seq(field("base", $.identifier), "::", field("item", $.identifier)),

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
            field("operator", choice("*", "/", "%")),
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
        $.unary_expression,
        $.typed_literal,
        $.vec_literal,
        $.postfix_expression,
        $.record_constructor_expression,
        $.path_expression,
        $.return_expression,
        $.number,
        $.parenthesized_expression,
        $.tuple_expression,
        $.block,
        $.if_expression,
        $.when_expression,
      ),

    // Rust-style `if cond { … } else { … }`. Both branches are required;
    // Mirin has no statement-form `if`. The two branches must produce the
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
    _if_condition: ($) => $._header_expression,

    // `when EVENT { … }` — Mirin's primitive for registered state. EVENT
    // is conventionally `clk.posedge()`, but any expression yielding a
    // value of type `Event @D` works. The event slot uses the same
    // restricted form as if-conditions for the same reason: a trailing
    // `{` opens the body block, so the event expression must end before
    // the parser sees `{`.
    // An optional `init VALUE` preceder is POWER-ON state for the register
    // this `when` produces (an SV initial block; sim + FPGA bitstream, NOT
    // reset). Attached HERE so init-on-a-wire is unrepresentable —
    // planning/when_ram.md.
    when_expression: ($) =>
      seq(
        optional(seq("init", field("init", $._header_expression))),
        "when",
        field("event", $._when_event),
        field("body", $.block),
      ),

    // Restricted event expression. A field-access chain (`clk.posedge()`)
    // is the common case; complex events go in parens. Note: we DO allow
    // `postfix_expression` here because the common case `clk.posedge()`
    // is a postfix that ends with `)` — the parser can tell where the
    // event ends before `{ … }` begins.
    _when_event: ($) => $._header_expression,

    // Explicit literal construction `uint(6)::4` — the value in the type's
    // associated-const namespace (planning/numeric_literals.md L4). The
    // restricted type form, like return types: a brace would be ambiguous.
    typed_literal: ($) =>
      seq(
        field("type", $.return_type_expression),
        "::",
        field("value", $.number),
      ),

    // Prefix `-` (`Neg::neg`), `!` (`Not::not`, logical), and `~` (`BitNot::
    // bitnot`, bitwise) — never a negative literal (planning/numeric_literals.md
    // L5). Binds tighter than any binary op.
    unary_expression: ($) =>
      prec(
        PREC.unary,
        seq(field("operator", choice("-", "!", "~")), field("operand", $.expression)),
      ),

    binary_expression: ($) =>
      choice(
        prec.left(
          PREC.logical_or,
          seq(
            field("left", $.expression),
            field("operator", "||"),
            field("right", $.expression),
          ),
        ),
        prec.left(
          PREC.logical_and,
          seq(
            field("left", $.expression),
            field("operator", "&&"),
            field("right", $.expression),
          ),
        ),
        prec.left(
          PREC.comparison,
          seq(
            field("left", $.expression),
            field("operator", choice("==", "!=", "<", "<=", ">", ">=")),
            field("right", $.expression),
          ),
        ),
        prec.left(
          PREC.multiplicative,
          seq(field("left", $.expression), field("operator", choice("*", "/", "%")), field("right", $.expression)),
        ),
        prec.left(
          PREC.additive,
          seq(
            field("left", $.expression),
            field("operator", choice("+", "-")),
            field("right", $.expression),
          ),
        ),
        prec.left(
          PREC.shift,
          seq(
            field("left", $.expression),
            field("operator", choice("<<", ">>")),
            field("right", $.expression),
          ),
        ),
        prec.left(
          PREC.bitwise_and,
          seq(
            field("left", $.expression),
            field("operator", "&"),
            field("right", $.expression),
          ),
        ),
        prec.left(
          PREC.bitwise_xor,
          seq(
            field("left", $.expression),
            field("operator", "^"),
            field("right", $.expression),
          ),
        ),
        prec.left(
          PREC.bitwise_or,
          seq(
            field("left", $.expression),
            field("operator", "|"),
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
            choice(
            $.path_expression,
            $.return_expression,
            $.number,
            $.parenthesized_expression,
            $.tuple_expression,
          ),
          ),
          repeat1(
            choice(
              $.field_access,
              $.index_access,
              seq($.named_argument_list, $.argument_list),
              $.argument_list,
            ),
          ),
        ),
      ),

    // `.f` named projection, `.0` tuple projection (planning/tuples.md) —
    // numbers never contain dots, so `x.0.1` lexes cleanly.
    field_access: ($) =>
      seq(".", field("field", choice($.identifier, $.number))),

    // `v[i]` — single-element indexing (planning/vectors.md).
    index_access: ($) => seq("[", field("index", $.expression), "]"),

    // `[a, b, c]` / `[e; N]` — vector construction (planning/vectors.md).
    // The repeat form's length is a const expression (a literal or a
    // Const-kind generic); it is required for parametric lengths.
    vec_literal: ($) =>
      choice(
        seq("[", commaSep($.expression), optional(","), "]"),
        seq(
          "[",
          field("elem", $.expression),
          ";",
          field("len", $.expression),
          "]",
        ),
      ),

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

    // `return` as a referrable place: the function's result binding (a
    // var-like signal node of the return type). `return.valid = …` drives an
    // out-leaf; `return.ready` reads an in-leaf (a returned port's
    // backpressure). Distinct from `return EXPR;` (return_statement), which is
    // the whole-result drive. See planning/return_variable.md.
    return_expression: ($) => "return",

    parenthesized_expression: ($) => seq("(", $.expression, ")"),

    // `(a, b)` — tuple construction (planning/tuples.md). The comma makes
    // the tuple: `(e)` is a parenthesized expression. Arity ≥ 2.
    tuple_expression: ($) =>
      seq(
        "(",
        $.expression,
        ",",
        commaSep1($.expression),
        optional(","),
        ")",
      ),

    identifier: () => /[A-Za-z_][A-Za-z0-9_]*/,
    // Decimal / 0x hex / 0b binary, `_` separators after the first char
    // (planning/numeric_literals.md L1). Width never rides the literal.
    number: () => /0[xX][0-9a-fA-F][0-9a-fA-F_]*|0[bB][01][01_]*|[0-9][0-9_]*/,
  },
});

function commaSep(rule) {
  return optional(commaSep1(rule));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)));
}
