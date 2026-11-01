; const x = new Foo()  /  const x = make()  /  const x = await make()
(variable_declarator
  name: (identifier) @type.var
  value: (new_expression constructor: [(identifier) (member_expression)] @type.ctor))
(variable_declarator
  name: (identifier) @type.var
  value: (call_expression function: [(identifier) (member_expression)] @type.ctor))
(variable_declarator
  name: (identifier) @type.var
  value: (await_expression (call_expression function: [(identifier) (member_expression)] @type.ctor)))
; const x: Foo = ...   and typed parameters
(variable_declarator name: (identifier) @type.var type: (type_annotation) @type.annot)
(required_parameter pattern: (identifier) @type.var type: (type_annotation) @type.annot)
(optional_parameter pattern: (identifier) @type.var type: (type_annotation) @type.annot)
; constructor(private repo: Repo)  ->  this.repo
(required_parameter
  (accessibility_modifier)
  pattern: (identifier) @type.field
  type: (type_annotation) @type.annot)
; class fields:  svc: Service;   cache = new Cache();
(public_field_definition name: (property_identifier) @type.field type: (type_annotation) @type.annot)
(public_field_definition
  name: (property_identifier) @type.field
  value: (new_expression constructor: [(identifier) (member_expression)] @type.ctor))
; this.x = new Foo()  /  this.x = make()
(assignment_expression
  left: (member_expression object: (this) property: (property_identifier) @type.field)
  right: (new_expression constructor: [(identifier) (member_expression)] @type.ctor))
(assignment_expression
  left: (member_expression object: (this) property: (property_identifier) @type.field)
  right: (call_expression function: [(identifier) (member_expression)] @type.ctor))
