; Definitions. Nesting (module > class > method, function > inner function)
; is computed from byte ranges, so these patterns only name the definitions.

(class_definition
  name: (identifier) @name) @def.class

(function_definition
  name: (identifier) @name) @def.function
