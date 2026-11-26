; Imports and #[cfg(test)] modules fold into one-line counts.
(source_file [(use_declaration) (extern_crate_declaration)] @collapse)
(source_file
  (attribute_item (attribute (identifier) @_cfg arguments: (token_tree (identifier) @_test))) @collapse
  .
  (mod_item) @collapse
  (#eq? @_cfg "cfg")
  (#eq? @_test "test"))

; Top-level items and items inside impl/trait/mod bodies.
(source_file (_ body: (_) @body) @item)
(source_file [(struct_item) (enum_item) (union_item) (type_item) (const_item) (static_item)
              (mod_item) (macro_definition) (function_signature_item)] @item)
(declaration_list (_ body: (_) @body) @item)
(declaration_list [(function_signature_item) (const_item) (type_item) (struct_item)
                   (enum_item) (mod_item) (macro_definition)] @item)
