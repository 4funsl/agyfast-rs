(module [(import_statement) (import_from_statement) (future_import_statement)] @collapse)

(module [
  (function_definition body: (_) @body)
  (class_definition body: (_) @body)
  (decorated_definition definition: (_ body: (_) @body))
] @item)

(class_definition body: (block [
  (function_definition body: (_) @body)
  (class_definition body: (_) @body)
  (decorated_definition definition: (_ body: (_) @body))
] @item))
