; One match per import spec, so the folded count is the number of imports.
(source_file (import_declaration (import_spec) @collapse) @collapse)
(source_file (import_declaration (import_spec_list (import_spec) @collapse)) @collapse)

(source_file [
  (function_declaration body: (_) @body)
  (method_declaration body: (_) @body)
] @item)
(source_file [(type_declaration) (const_declaration) (var_declaration)] @item)
