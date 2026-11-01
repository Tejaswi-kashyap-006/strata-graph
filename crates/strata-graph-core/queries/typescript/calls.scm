(call_expression function: (identifier) @call.callee) @call
(call_expression
  function: (member_expression
    object: (_) @call.receiver
    property: [(property_identifier) (private_property_identifier)] @call.callee)) @call
(new_expression constructor: (identifier) @call.callee) @call
(new_expression
  constructor: (member_expression
    object: (_) @call.receiver
    property: (property_identifier) @call.callee)) @call
