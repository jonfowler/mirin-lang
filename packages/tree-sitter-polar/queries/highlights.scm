[
  "struct"
  "port"
  "impl"
  "fn"
  "let"
  "var"
  "return"
] @keyword

[
  "in"
  "out"
  "const"
] @keyword.modifier

(comment) @comment
(number) @number

; Operators and punctuation
"->" @operator
"=>" @operator
"@" @punctuation.special
"#" @punctuation.special
"::" @punctuation.delimiter

; Top-level declaration names
(function_definition name: (identifier) @type)
(struct_definition name: (identifier) @type)
(struct_definition constructor: (identifier) @constructor)
(port_definition name: (identifier) @type)
(port_definition constructor: (identifier) @constructor)
(function_definition name: (identifier) @function)

; Type expressions — the head name is always a type
(type_expression name: (identifier) @type)

; Path expressions — Type::member
(path_expression type: (identifier) @type)
(path_expression member: (identifier) @property)

; Parameters — declared names
(named_parameter name: (identifier) @variable.parameter)
(parameter name: (identifier) @variable.parameter)

; Local binding names
(let_statement name: (identifier) @variable)
(var_statement (identifier) @variable)

; Field access and named argument field names
(field_access field: (identifier) @property)
(named_or_shorthand_argument name: (identifier) @property)
(record_field_value name: (identifier) @property)
(record_field_type name: (identifier) @property)
(port_field name: (identifier) @property)

; Fallback: unresolved identifiers
(identifier) @variable
