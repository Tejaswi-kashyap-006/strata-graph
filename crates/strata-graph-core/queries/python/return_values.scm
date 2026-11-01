; Returned or yielded expressions, used to infer the class a function returns
; when it has no return annotation (e.g. `return app.test_client()`).
(return_statement (_) @value)
(yield (_) @value)
