; Lightweight type facts used to resolve calls on variables and attributes.
; None of these run inference; each records one syntactic hint.

; x = Foo(...)
(assignment
  left: (identifier) @type.var
  right: (call
    function: [(identifier) (attribute)] @type.ctor))

; self.x = Foo(...)
(assignment
  left: (attribute
    object: (identifier) @type.self
    attribute: (identifier) @type.attr)
  right: (call
    function: [(identifier) (attribute)] @type.ctor))

; x: Foo = ...   and   x: Foo   (class fields in dataclasses, locals)
(assignment
  left: (identifier) @type.var
  type: (type) @type.annot)

; self.x: Foo = ...
(assignment
  left: (attribute
    object: (identifier) @type.self
    attribute: (identifier) @type.attr)
  type: (type) @type.annot)

; def f(x: Foo)  and  def f(x: Foo = None)
(typed_parameter
  (identifier) @type.var
  type: (type) @type.annot)

(typed_default_parameter
  name: (identifier) @type.var
  type: (type) @type.annot)
