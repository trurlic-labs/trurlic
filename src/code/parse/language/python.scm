; Python, read by the code index's general resolver under the contract
; it holds every query file to. Each name is captured by one pattern.

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

((type_alias_statement
  left: (type [(identifier) @name (generic_type (identifier) @name)])) @definition
  (#set! kind "type_alias")
  (#set! container "false")
  (#set! within "top class")
  (#set! visibility "public"))

; A name with a leading underscore is private; a dunder name is not.
((identifier) @visibility.private
  (#match? @visibility.private "^_")
  (#not-match? @visibility.private "^__.*__$"))

; Tests as pytest and unittest collect them: a `test_*` function, a
; `Test*` class and a `TestCase` subclass (`TestCase[int]` too), and all
; they hold.
((function_definition name: (identifier) @_name) @role.test
  (#match? @_name "^test_"))

((class_definition name: (identifier) @_name) @role.test
  (#match? @_name "^Test"))

((class_definition
  superclasses: (argument_list
    [(identifier) @_base
     (attribute attribute: (identifier) @_base)
     (subscript value: [(identifier) @_base
                        (attribute attribute: (identifier) @_base)])])) @role.test
  (#match? @_base "TestCase$"))

; The hash: a string's text, a format specifier and an interpolation
; with the `=` specifier, whose spaces the output keeps, are one token
; each, spaces kept and comments left out. A format specifier is cut out
; of an `=` interpolation and taken as its own, and a nested field of a
; format specifier as code. A string's delimiters, a number and an escape
; of a string that is not raw are hashed by what they mean, so a
; formatter's quotes, prefix case, `1.0` for `1.`, `1e5` for `1e+5`,
; `\xab` for `\xAB` and `"'"` for `'\''` move nothing. A raw string's
; text is every byte between its quotes, as written, so `r'\"'` and
; `r"\""` hash alike. Python reads the parts of an implicit concatenation
; as one string, so parts that start alike and are no f-string or
; template hash as one; an octal escape spelled in three digits keeps
; `'\0' '1'` apart from `'\01'`. A class's empty `()` means nothing.
(string_content) @hash.string
((string (string_start) @_start (string_content (escape_sequence) @hash.escape))
  (#not-match? @_start "[rR]"))
(format_specifier) @hash.unit
((interpolation "=") @hash.unit)
(format_specifier (format_expression) @hash.group)
[(string_start) (string_end)] @hash.delimiter
((concatenated_string (string) . (comment)* . (string (string_start) @hash.join))
  (#not-match? @hash.join "[fFtT]"))
[(integer) (float)] @hash.literal
(class_definition superclasses: (argument_list . "(" @hash.skip . ")" @hash.skip .))

; A trailing comma means nothing in a call's arguments, a definition's
; parameters, an import list, a parenthesized `with`, a list, a set, a
; dictionary and their patterns, and after two elements or more in a
; tuple, bare or parenthesized, a subscript, an annotation's too, and a
; type parameter list; `(a,)` is a tuple, `(a)` is `a`, and `x[a,]` is
; `x[(a,)]`.
(argument_list "," @hash.skip . (comment)* . ")")
(parameters "," @hash.skip . (comment)* . ")")
(import_from_statement "," @hash.skip . (comment)* . ")")
(with_clause "," @hash.skip . (comment)* . ")")
(class_pattern "," @hash.skip . (comment)* . ")")
(list "," @hash.skip . (comment)* . "]")
(list_pattern "," @hash.skip . (comment)* . "]")
(set "," @hash.skip . (comment)* . "}")
(dictionary "," @hash.skip . (comment)* . "}")
(dict_pattern "," @hash.skip . (comment)* . "}")
(tuple "," . (comment)* . (_) . (comment)* . "," @hash.skip . (comment)* . ")")
(tuple_pattern "," . (comment)* . (_) . (comment)* . "," @hash.skip . (comment)* . ")")
(expression_list "," . (comment)* . (_) . (comment)* . "," @hash.skip .)
(pattern_list "," . (comment)* . (_) . (comment)* . "," @hash.skip .)
(subscript "," . (comment)* . (_) . (comment)* . "," @hash.skip . (comment)* . "]")
(type_parameter "," . (comment)* . (_) . (comment)* . "," @hash.skip . (comment)* . "]")

; Parentheses mean nothing around a primary (a name, a call, a literal,
; an implicit concatenation, which Python's grammar reads as one atom, a
; display, parentheses), a power under a unary operator (`-(x**2)` is
; `-x**2`), the whole expression of an assignment, a `return`, a `yield`,
; a `del`, a `for` or a condition, a tuple or a target there, a whole
; annotation, a tuple that is a whole statement, an import list, a
; `with`'s items, a comprehension's target, and an `assert`'s message and
; its condition but a `yield`. Elsewhere around an operator they bind it,
; and a comma may follow that a `yield` inside would take in: `f((yield
; a), b)` is no `f((yield a, b))`. A tuple keeps them with no comma: `()`
; is a tuple.
(parenthesized_expression "("
  @hash.skip [(identifier) (attribute) (subscript) (call) (string) (concatenated_string)
              (integer) (float) (true) (false) (none) (ellipsis) (list) (dictionary)
              (set) (tuple) (list_comprehension) (dictionary_comprehension)
              (set_comprehension) (generator_expression) (parenthesized_expression)]
  ")" @hash.skip)
(unary_operator
  argument: (parenthesized_expression "(" @hash.skip
              (binary_operator operator: "**") ")" @hash.skip))
(assignment right: [(parenthesized_expression "(" @hash.skip ")" @hash.skip)
                     (tuple "(" @hash.skip "," ")" @hash.skip)])
(augmented_assignment right: [(parenthesized_expression "(" @hash.skip ")" @hash.skip)
                               (tuple "(" @hash.skip "," ")" @hash.skip)])
(return_statement [(parenthesized_expression "(" @hash.skip ")" @hash.skip)
                   (tuple "(" @hash.skip "," ")" @hash.skip)])
(yield [(parenthesized_expression "(" @hash.skip ")" @hash.skip)
        (tuple "(" @hash.skip "," ")" @hash.skip)])
(delete_statement [(parenthesized_expression "(" @hash.skip ")" @hash.skip)
                   (tuple "(" @hash.skip "," ")" @hash.skip)])
(for_statement right: [(parenthesized_expression "(" @hash.skip ")" @hash.skip)
                       (tuple "(" @hash.skip "," ")" @hash.skip)])
(for_statement left: (tuple_pattern "(" @hash.skip (_) ")" @hash.skip))
(assignment left: (tuple_pattern "(" @hash.skip (_) ")" @hash.skip))
(if_statement condition: (parenthesized_expression "(" @hash.skip ")" @hash.skip))
(elif_clause condition: (parenthesized_expression "(" @hash.skip ")" @hash.skip))
(while_statement condition: (parenthesized_expression "(" @hash.skip ")" @hash.skip))
(assert_statement "," . (parenthesized_expression "(" @hash.skip ")" @hash.skip) .)
(assert_statement . "assert" .
  (parenthesized_expression "("
    @hash.skip [(comparison_operator) (not_operator) (boolean_operator) (binary_operator)
                (unary_operator) (conditional_expression) (lambda) (await)]
    ")" @hash.skip))
(type (parenthesized_expression "(" @hash.skip ")" @hash.skip))
(expression_statement . (tuple "(" @hash.skip "," ")" @hash.skip) .)
(import_from_statement "(" @hash.skip ")" @hash.skip)
(with_clause "(" @hash.skip ")" @hash.skip)
(for_in_clause left: (tuple_pattern "(" @hash.skip (_) ")" @hash.skip))

; A conditional expression takes in no comma and reaches as far without
; parentheses (`a if b else c, d`), so they mean nothing around one where
; a whole expression stands: an argument, a keyword's value, an element
; of a list or a tuple, a dictionary's value, the element of a list
; comprehension or a generator, and another's `else` branch, which it
; continues (`a if b else c if d else e`). A lambda's body is a whole
; expression too, so they mean nothing around any but a `yield` there.
(argument_list
  (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(list (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(tuple (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(keyword_argument
  value: (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(pair
  value: (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(list_comprehension
  body: (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(generator_expression
  body: (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(conditional_expression "else" .
  (parenthesized_expression "(" @hash.skip (conditional_expression) ")" @hash.skip))
(lambda body: (parenthesized_expression "("
  @hash.skip [(comparison_operator) (not_operator) (boolean_operator) (binary_operator)
              (unary_operator) (conditional_expression) (lambda) (await)]
  ")" @hash.skip))

; Each statement is a group, so a statement moved into or out of a block,
; or split in two, moves the hash, and a `;` between statements is a
; statement's end. A block needs no group of its own: the statement that
; holds it closes right after its last statement. A docstring, the first
; statement of a class or function whose one child is a plain string, is
; left out, as a comment is.
(block (_) @hash.group)
(module (_) @hash.group)
(block ";" @hash.skip)

(class_definition body: (block . (expression_statement . (string (string_start) @_start) .) @hash.skip)
  (#not-match? @_start "[fFtTbB]"))
(function_definition body: (block . (expression_statement . (string (string_start) @_start) .) @hash.skip)
  (#not-match? @_start "[fFtTbB]"))
