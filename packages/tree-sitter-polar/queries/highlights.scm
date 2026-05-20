[
  "cmp"
  "struct"
  "port"
  "impl"
  "fn"
  "let"
  "return"
  "rec"
] @keyword

[
  "in"
  "out"
  "const"
] @keyword.modifier

(comment) @comment
(identifier) @variable
(number) @number

(component_definition name: (identifier) @type)
(struct_definition name: (identifier) @type)
(struct_definition constructor: (identifier) @constructor)
(port_definition name: (identifier) @type)
(port_definition constructor: (identifier) @constructor)
(function_definition name: (identifier) @function)

(field_access field: (identifier) @property)
(record_field_value name: (identifier) @property)
(record_field_type name: (identifier) @property)
(port_field name: (identifier) @property)
