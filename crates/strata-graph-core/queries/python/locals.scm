; Names bound locally (parameters, assignments, loop and with targets).
; A call to one of these is a call to a value, so it must never fall back
; to a repo-wide name match.

(parameters (identifier) @local)
(parameters (default_parameter name: (identifier) @local))
(parameters (typed_parameter (identifier) @local))
(parameters (typed_default_parameter name: (identifier) @local))
(parameters (list_splat_pattern (identifier) @local))
(parameters (dictionary_splat_pattern (identifier) @local))
(lambda_parameters (identifier) @local)

(assignment left: (identifier) @local)
(assignment left: (pattern_list (identifier) @local))
(assignment left: (tuple_pattern (identifier) @local))
(augmented_assignment left: (identifier) @local)
(named_expression name: (identifier) @local)

(for_statement left: (identifier) @local)
(for_statement left: (pattern_list (identifier) @local))
(for_in_clause left: (identifier) @local)

(as_pattern alias: (as_pattern_target (identifier) @local))
(except_clause (as_pattern alias: (as_pattern_target (identifier) @local)))
