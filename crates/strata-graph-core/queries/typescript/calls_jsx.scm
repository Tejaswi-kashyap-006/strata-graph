; JSX component use counts as a call (lower-case intrinsic tags are dropped in Rust).
(jsx_opening_element name: (identifier) @call.callee) @call
(jsx_self_closing_element name: (identifier) @call.callee) @call
(jsx_opening_element
  name: (member_expression object: (_) @call.receiver property: (property_identifier) @call.callee)) @call
(jsx_self_closing_element
  name: (member_expression object: (_) @call.receiver property: (property_identifier) @call.callee)) @call
