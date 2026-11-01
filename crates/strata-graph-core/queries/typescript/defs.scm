; Definitions. @fn marks the function node of a definition whose body lives in
; a separate node (const f = () => ..., class fields holding arrows).
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.class
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
; Methods count only inside classes and interfaces; object-literal methods are
; values, not definitions other modules can import.
(class_body (method_definition name: [(property_identifier) (private_property_identifier)] @name) @def.function)
(interface_body (method_signature name: [(property_identifier) (private_property_identifier)] @name) @def.function)
(class_body (abstract_method_signature name: [(property_identifier) (private_property_identifier)] @name) @def.function)
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)] @fn) @def.function
(public_field_definition
  name: [(property_identifier) (private_property_identifier)] @name
  value: [(arrow_function) (function_expression)] @fn) @def.function
