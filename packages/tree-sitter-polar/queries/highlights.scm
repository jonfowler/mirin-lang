[
  "struct"
  "port"
  "impl"
  "trait"
  "for"
  "where"
  "const"
  "fn"
  "mod"
  "use"
  "as"
  "let"
  "var"
  "return"
  "if"
  "else"
  "when"
  "init"
  "self"
  "verilog"
] @keyword

[
  "pub"
  "crate"
  "super"
  "in"
  "out"
  "param"
  "dom"
] @keyword.modifier

(comment) @comment
(number) @number

; Operators and punctuation
"->" @operator
"=>" @operator
"@" @punctuation.special
"::" @punctuation.delimiter

; Top-level declaration names
(function_definition name: (identifier) @type)
(struct_definition name: (identifier) @type)
(struct_definition constructor: (identifier) @constructor)
(port_definition name: (identifier) @type)
(port_definition constructor: (identifier) @constructor)
(function_definition name: (identifier) @function)
(module_definition name: (identifier) @namespace)

; Constructor uses — `packet { .. }` / `option { .. }` in expressions
(record_constructor_expression constructor: (identifier) @constructor)

; Builtin type and constant names — plain identifiers in the grammar, resolved
; in nameres. Keep these lists in sync with `builtin_type_names()` in
; polar-compiler's def_map (a polar-lsp test enforces it). These must come
; before the path-expression pattern, which tags every bare segment @type.
((identifier) @type
  (#any-of? @type "uint" "sint" "bits" "integer" "bool" "Clock" "Reset" "Event" "Vec" "Type"))
((identifier) @constant
  (#any-of? @constant "true" "false" "high" "low"))

; Type expressions — the head name is always a type
(type_expression name: (identifier) @type)

; Path expressions — `a::b::c`. Segments are module/type names; the final
; binding's kind isn't known syntactically, so tag all segments as types.
(path_expression segment: (identifier) @type)

; Use paths
(use_path (identifier) @type)

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
