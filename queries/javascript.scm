(program (import_statement) @collapse)

(program [
  (function_declaration body: (_) @body)
  (generator_function_declaration body: (_) @body)
  (class_declaration body: (_) @body)
  (lexical_declaration (variable_declarator value: [(arrow_function body: (_) @body) (function_expression body: (_) @body) (class body: (_) @body)]))
  (variable_declaration (variable_declarator value: [(arrow_function body: (_) @body) (function_expression body: (_) @body) (class body: (_) @body)]))
  (export_statement declaration: [
    (function_declaration body: (_) @body)
    (generator_function_declaration body: (_) @body)
    (class_declaration body: (_) @body)
    (lexical_declaration (variable_declarator value: [(arrow_function body: (_) @body) (function_expression body: (_) @body) (class body: (_) @body)]))
    (variable_declaration (variable_declarator value: [(arrow_function body: (_) @body) (function_expression body: (_) @body) (class body: (_) @body)]))
  ])
] @item)

(program [
  (lexical_declaration)
  (variable_declaration)
  (export_statement declaration: (_))
  (export_statement value: (_))
] @item)

(class_body (method_definition body: (_) @body) @item)

; Top-level calls taking a callback: describe(...), test(...), app.get(...).
(program (expression_statement (call_expression arguments: (arguments [(arrow_function) (function_expression)]))) @item)
