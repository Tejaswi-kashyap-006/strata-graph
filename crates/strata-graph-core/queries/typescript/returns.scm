; Declared return types, keyed to their definition node.
(function_declaration return_type: (type_annotation) @ret) @def
(generator_function_declaration return_type: (type_annotation) @ret) @def
(method_definition return_type: (type_annotation) @ret) @def
(method_signature return_type: (type_annotation) @ret) @def
(abstract_method_signature return_type: (type_annotation) @ret) @def
(variable_declarator
  value: [(arrow_function return_type: (type_annotation) @ret)
          (function_expression return_type: (type_annotation) @ret)]) @def
(public_field_definition
  value: [(arrow_function return_type: (type_annotation) @ret)
          (function_expression return_type: (type_annotation) @ret)]) @def
