; foo(...)
(call
  function: (identifier) @call.callee) @call

; receiver.foo(...)
(call
  function: (attribute
    object: (_) @call.receiver
    attribute: (identifier) @call.callee)) @call
