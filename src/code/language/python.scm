; Python, read by the general resolver (src/code/general.rs) under the
; contract of src/code/query.rs. Each name is captured by one pattern.

; A `def` is a function, and a method directly in a class; a `class` is a
; class. A decorated one's span starts at its first decorator.
((function_definition name: (identifier) @name) @definition
  (#set! kind "function")
  (#set! kind.in "class:method")
  (#set! definition.wrap "decorated_definition")
  (#set! visibility "public"))

((class_definition name: (identifier) @name) @definition
  (#set! kind "class")
  (#set! definition.wrap "decorated_definition")
  (#set! visibility "public"))

; A name a module or a class binds: `x = 1`, `x: int`, each of `a = b = 1`,
; each of `a, (b, [c, *d]) = ...`, and `type T = ...`. Its span is the
; statement; what a function binds is no definition.
((assignment left: (identifier) @name) @definition
  (#set! kind "variable")
  (#set! container "false")
  (#set! within "top class")
  (#set! definition.via "assignment")
  (#set! definition.to "expression_statement")
  (#set! visibility "public"))

([(pattern_list (identifier) @name)
  (tuple_pattern (identifier) @name)
  (list_pattern (identifier) @name)
  (list_splat_pattern (identifier) @name)] @definition
  (#set! kind "variable")
  (#set! container "false")
  (#set! within "top class")
  (#set! definition.via "pattern_list tuple_pattern list_pattern list_splat_pattern assignment")
  (#set! definition.to "expression_statement")
  (#set! visibility "public"))

((type_alias_statement left: (type (identifier) @name)) @definition
  (#set! kind "type_alias")
  (#set! container "false")
  (#set! within "top class")
  (#set! visibility "public"))

; A name with a leading underscore is private; a dunder name is not.
((identifier) @visibility.private
  (#match? @visibility.private "^_")
  (#not-match? @visibility.private "^__.*__$"))

; Tests as pytest and unittest collect them: a `test_*` function, a
; `Test*` class and a `TestCase` subclass, and all they hold.
((function_definition name: (identifier) @_name) @role.test
  (#match? @_name "^test_"))

((class_definition name: (identifier) @_name) @role.test
  (#match? @_name "^Test"))

((class_definition
  superclasses: (argument_list
    [(identifier) @_base
     (attribute attribute: (identifier) @_base)])) @role.test
  (#match? @_base "TestCase$"))

; The hash: a string's text and a format specifier are one token each,
; spaces kept; each statement and each block is a group, so a statement
; moved into or out of a block, or split in two, moves it, and a `;`
; between statements is a statement's end. A docstring, the first
; statement of a module, class or function that is a plain string, is
; left out, as a comment is.
[(string_content) (format_specifier)] @hash.unit

(block) @hash.group
(block (_) @hash.group)
(module (_) @hash.group)
(block ";" @hash.skip)
(module ";" @hash.skip)

(module . (expression_statement (string (string_start) @_start)) @hash.skip
  (#not-match? @_start "[fFtTbB]"))
(class_definition body: (block . (expression_statement (string (string_start) @_start)) @hash.skip)
  (#not-match? @_start "[fFtTbB]"))
(function_definition body: (block . (expression_statement (string (string_start) @_start)) @hash.skip)
  (#not-match? @_start "[fFtTbB]"))
