; import Def from "x"
(import_statement
  (import_clause (identifier) @import.default)
  source: (string (string_fragment) @import.source))
; import { A, B as C } from "x"   (alias read from the specifier node)
(import_statement
  (import_clause (named_imports (import_specifier name: (identifier) @import.name)))
  source: (string (string_fragment) @import.source))
; import * as ns from "x"
(import_statement
  (import_clause (namespace_import (identifier) @import.namespace))
  source: (string (string_fragment) @import.source))
; export { A, B as C } from "x"
(export_statement
  (export_clause (export_specifier name: (identifier) @import.name))
  source: (string (string_fragment) @import.source))
; export * as ns from "x"
(export_statement
  (namespace_export (identifier) @import.namespace)
  source: (string (string_fragment) @import.source))
; export * from "x"
(export_statement
  "*"
  source: (string (string_fragment) @import.star))
; export default foo;  export default class Foo {}
(export_statement "default" value: (identifier) @export.default)
(export_statement
  "default"
  declaration: [(class_declaration name: (_) @export.default)
                (abstract_class_declaration name: (_) @export.default)
                (function_declaration name: (_) @export.default)])
; export { local as exported }   (no source: an alias inside this module)
(export_statement (export_clause (export_specifier name: (identifier) @export.local)))
