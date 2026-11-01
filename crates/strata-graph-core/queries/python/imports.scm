; import a.b.c
(import_statement
  name: (dotted_name) @import.module)

; import a.b as x
(import_statement
  name: (aliased_import
    name: (dotted_name) @import.module
    alias: (identifier) @import.alias))

; from m import n
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.from
  name: (dotted_name) @import.name)

; from m import n as k
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.from
  name: (aliased_import
    name: (dotted_name) @import.name
    alias: (identifier) @import.alias))

; from m import *
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.from
  (wildcard_import) @import.wildcard)
