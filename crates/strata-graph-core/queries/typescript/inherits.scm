; extends and implements both become INHERITS edges (implements lets impact
; analysis find every class behind an interface).
(class_declaration (class_heritage (extends_clause value: (_) @base))) @class
(abstract_class_declaration (class_heritage (extends_clause value: (_) @base))) @class
(class_declaration (class_heritage (implements_clause (_) @base))) @class
(abstract_class_declaration (class_heritage (implements_clause (_) @base))) @class
(interface_declaration (extends_type_clause type: (_) @base)) @class
