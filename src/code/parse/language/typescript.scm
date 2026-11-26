; TypeScript, and with tsx.scm TSX and JavaScript, read by the code
; index's general resolver under the contract it holds every query file
; to.
;
; A declaration's span grows over the `export`, `declare` or statement
; around it. A declaration at a module's or a namespace's top is private
; unless an `export` holds it, an export list names it or an ambient
; module or namespace holds it; a member is public unless `private`,
; `protected` or `#` says otherwise.

; Functions: declared, an overload's or an ambient one's signature, and an
; arrow function, function expression or class expression a `const`,
; `let` or `var` binds, at any level.
([(function_declaration name: (identifier) @name)
  (generator_function_declaration name: (identifier) @name)
  (function_signature name: (identifier) @name)] @definition
  (#set! kind "function")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

((variable_declarator
   name: (identifier) @name
   value: [(arrow_function) (function_expression) (generator_function)]) @definition
  (#set! kind "function")
  (#set! exclusive "true")
  (#set! definition.to "lexical_declaration variable_declaration")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

((variable_declarator name: (identifier) @name value: (class)) @definition
  (#set! kind "class")
  (#set! exclusive "true")
  (#set! definition.to "lexical_declaration variable_declaration")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

; A named class expression an assignment binds (`module.exports = class
; A {}`, `X.Y = class Y {}`) is that class; an unnamed one defines
; nothing, nor do its members.
([(assignment_expression right: (class name: (type_identifier) @name) @definition)
  (augmented_assignment_expression right: (class name: (type_identifier) @name) @definition)]
  (#set! kind "class")
  (#set! visibility "private"))

; Any other name a module or a namespace binds: each of `const a = 1, b`,
; and each name a destructuring binds. A function's locals define nothing.
((variable_declarator name: (identifier) @name) @definition
  (#set! kind "variable")
  (#set! within "top module")
  (#set! definition.to "lexical_declaration variable_declaration")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

([(object_pattern (shorthand_property_identifier_pattern) @name @definition)
  (pair_pattern value: (identifier) @name @definition)
  (array_pattern (identifier) @name @definition)
  (rest_pattern (identifier) @name @definition)
  (object_assignment_pattern left: (shorthand_property_identifier_pattern) @name @definition)
  (assignment_pattern left: (identifier) @name @definition)]
  (#set! kind "variable")
  (#set! within "top module")
  (#set! definition.via "object_pattern array_pattern pair_pattern rest_pattern object_assignment_pattern assignment_pattern variable_declarator")
  (#set! definition.to "lexical_declaration variable_declaration")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

; Types: classes, interfaces, type aliases, enums and namespaces. A
; namespace `A.B` is two segments; `declare module "x"` is named `x`.
([(class_declaration name: (type_identifier) @name)
  (abstract_class_declaration name: (type_identifier) @name)] @definition
  (#set! kind "class")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

((interface_declaration name: (type_identifier) @name) @definition
  (#set! kind "interface")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

((type_alias_declaration name: (type_identifier) @name) @definition
  (#set! kind "type_alias")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

((enum_declaration name: (identifier) @name) @definition
  (#set! kind "enum")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

([(internal_module name: (_) @name) (module name: (_) @name)] @definition
  (#set! kind "module")
  (#set! name.separator ".")
  (#set! definition.wrap "export_statement ambient_declaration expression_statement")
  (#set! visibility "private"))

; An unnamed default export is named `default`: a function or a class by
; its kind, any other value of more than one leaf (`export default x`
; names `x`, which the export list rule makes public) a variable. What
; `module.exports` is, CommonJS's default, is named so too when it is an
; unnamed function or class, or the value an unnamed function's call
; returns (`module.exports = (function () { ... })()`), whose body is
; then under `default`; a named function keeps its name, as a named class
; does. `exports = module.exports = f` is the same export.
((export_statement "default" @name value: [(function_expression) (arrow_function) (generator_function)]) @definition
  (#set! kind "function")
  (#set! exclusive "true"))

((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @name)
   right: [(function_expression !name) (arrow_function) (generator_function !name)])
   @definition
  (#eq? @_module "module")
  (#eq? @name "exports")
  (#set! kind "function")
  (#set! name.text "default")
  (#set! definition.via "assignment_expression")
  (#set! definition.to "expression_statement")
  (#set! visibility "public"))

((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @_exports)
   right: [(function_expression name: (identifier) @name) (generator_function name: (identifier) @name)] @definition)
  (#eq? @_module "module")
  (#eq? @_exports "exports")
  (#set! kind "function")
  (#set! visibility "public"))

((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @name)
   right: (class !name) @scope.none)
   @definition
  (#eq? @_module "module")
  (#eq? @name "exports")
  (#set! kind "class")
  (#set! name.text "default")
  (#set! definition.via "assignment_expression")
  (#set! definition.to "expression_statement")
  (#set! visibility "public"))

((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @name)
   right: [(call_expression
             function: [(parenthesized_expression [(function_expression) (arrow_function)])
                        (member_expression object: (parenthesized_expression [(function_expression) (arrow_function)]))])
           (parenthesized_expression
             (call_expression function: [(function_expression) (member_expression object: (function_expression))]))])
   @definition
  (#eq? @_module "module")
  (#eq? @name "exports")
  (#set! kind "variable")
  (#set! name.text "default")
  (#set! definition.via "assignment_expression")
  (#set! definition.to "expression_statement")
  (#set! visibility "public"))

((export_statement "default" @name value: (class)) @definition
  (#set! kind "class")
  (#set! exclusive "true"))

((export_statement "default" @name value: (_ _)) @definition
  (#set! kind "variable"))

; Members. A class's methods, accessors and constructor are methods, its
; properties fields; a method's decorators stand beside it in the class
; body. An interface's members, an enum's and those of a type literal a
; type alias names are its own.
((class_body [(method_definition name: (_) @name)
              (method_signature name: (_) @name)
              (abstract_method_signature name: (_) @name)] @definition)
  (#set! kind "method")
  (#set! within "class")
  (#set! definition.preceding "decorator")
  (#set! visibility "public"))

((class_body (public_field_definition name: (_) @name) @definition)
  (#set! kind "field")
  (#set! within "class")
  (#set! visibility "public"))

([(interface_body (property_signature name: (_) @name) @definition)
  (type_alias_declaration value: (object_type (property_signature name: (_) @name) @definition))]
  (#set! kind "field")
  (#set! visibility "public"))

([(interface_body (method_signature name: (_) @name) @definition)
  (type_alias_declaration value: (object_type (method_signature name: (_) @name) @definition))]
  (#set! kind "method")
  (#set! visibility "public"))

([(enum_body name: (_) @name @definition)
  (enum_body (enum_assignment name: (_) @name) @definition)]
  (#set! kind "variant")
  (#set! visibility "public"))

; A method of an object literal that a `const` binds at a module's or a
; namespace's top, or that a module exports as its default: `api::get`.
([(program (lexical_declaration kind: "const" (variable_declarator value: (object
    [(method_definition name: (_) @name)
     (pair key: (_) @name value: [(arrow_function) (function_expression) (generator_function)])] @definition))))
  (program (export_statement declaration: (lexical_declaration kind: "const" (variable_declarator value: (object
    [(method_definition name: (_) @name)
     (pair key: (_) @name value: [(arrow_function) (function_expression) (generator_function)])] @definition)))))
  (internal_module body: (statement_block (lexical_declaration kind: "const" (variable_declarator value: (object
    [(method_definition name: (_) @name)
     (pair key: (_) @name value: [(arrow_function) (function_expression) (generator_function)])] @definition)))))
  (internal_module body: (statement_block (export_statement declaration: (lexical_declaration kind: "const" (variable_declarator value: (object
    [(method_definition name: (_) @name)
     (pair key: (_) @name value: [(arrow_function) (function_expression) (generator_function)])] @definition))))))
  (export_statement value: (object
    [(method_definition name: (_) @name)
     (pair key: (_) @name value: [(arrow_function) (function_expression) (generator_function)])] @definition))]
  (#set! kind "method")
  (#set! visibility "public"))

; Tests are calls of a runner, named by their first argument, a string,
; under the tests whose callbacks hold them. The callee is a runner's
; name, or a chain of up to three of its members (`it.only`,
; `test.describe.parallel.only`), called directly or through a call
; (`describe.each(table)`, `test.extend({})`), each name and member from
; the runners' lists here. The string is followed by a function, a
; function's name, or an options object and then one of those
; (Playwright's details). So `test("abc")`, `test.set("a", "b")`,
; `suite.add("x", fn)` and `describe.bind(null)("x", fn)` are no tests, nor
; is node:test's `t.test("sub", fn)`, a member call on a parameter no
; rule tells from a project's own object.
((call_expression
   function: [(identifier) @_runner
              (member_expression object: (identifier) @_runner property: (property_identifier) @_member)
              (member_expression
                object: (member_expression object: (identifier) @_runner property: (property_identifier) @_member)
                property: (property_identifier) @_member)
              (member_expression
                object: (member_expression
                          object: (member_expression object: (identifier) @_runner property: (property_identifier) @_member)
                          property: (property_identifier) @_member)
                property: (property_identifier) @_member)
              (call_expression
                function: [(member_expression object: (identifier) @_runner property: (property_identifier) @_member)
                           (member_expression
                             object: (member_expression object: (identifier) @_runner property: (property_identifier) @_member)
                             property: (property_identifier) @_member)
                           (member_expression
                             object: (member_expression
                                       object: (member_expression object: (identifier) @_runner property: (property_identifier) @_member)
                                       property: (property_identifier) @_member)
                             property: (property_identifier) @_member)])]
   arguments: [(arguments . [(string) (template_string)] @name . [(arrow_function) (function_expression) (identifier)] @_test)
               (arguments . [(string) (template_string)] @name . (object) . [(arrow_function) (function_expression) (identifier)] @_test)])
   @definition @role.test
  (#any-of? @_runner "describe" "context" "suite" "it" "test" "specify" "bench" "fdescribe" "xdescribe" "fit" "xit")
  (#any-of? @_member "only" "skip" "each" "for" "concurrent" "sequential" "shuffle" "failing" "fails" "fail" "fixme" "slow" "serial" "parallel" "runIf" "skipIf" "if" "extend" "describe" "step")
  (#set! kind "test"))

; Unnamed scopes: a function, method or class no pattern names. Its
; locals define nothing; a function in it is under the definition around
; it (`describe("a", () => { function f() {} })` is `a::f`), or defines
; nothing when no definition is (`app.get("/", (req) => { ... })`).
[(arrow_function) (function_expression) (generator_function) (method_definition) (class)] @scope.unnamed

; No scope: a class a name binds, whose members are that name's, and a
; wrapper of the whole file, which is the module. A wrapper is an unnamed
; function that a statement of the file's top calls, the call alone, in
; parentheses or after a unary operator: the function, in parentheses
; or not (a statement's own call takes it in parentheses), or its `call`
; or `apply` (`(function () {})()`, `(function () {}())`, `!function ()
; {}()`, `;(function () {}.call(this))`); and each function such a call
; passes, in parentheses or not (a UMD factory); and the factory of a
; top-level `define` (AMD). A bundle's other shapes are no wrapper.
[(variable_declarator value: (class) @scope.none)
 (export_statement "default" value: (class) @scope.none)]

((program (expression_statement [
   (call_expression
     function: [(parenthesized_expression [(function_expression) (arrow_function)] @scope.none)
                (member_expression
                  object: (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)
                  property: (property_identifier) @_invoke)])
   (parenthesized_expression (call_expression
     function: [(function_expression) @scope.none
                (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)
                (member_expression
                  object: [(function_expression) @scope.none
                           (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)]
                  property: (property_identifier) @_invoke)]))
   (unary_expression argument: (call_expression
     function: [(function_expression) @scope.none
                (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)
                (member_expression
                  object: [(function_expression) @scope.none
                           (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)]
                  property: (property_identifier) @_invoke)]))]))
  (#any-of? @_invoke "call" "apply"))

((program (expression_statement [
   (call_expression
     function: [(parenthesized_expression [(function_expression) (arrow_function)])
                (member_expression
                  object: (parenthesized_expression [(function_expression) (arrow_function)])
                  property: (property_identifier) @_invoke)]
     arguments: (arguments [(function_expression) @scope.none (arrow_function) @scope.none
                            (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)]))
   (parenthesized_expression (call_expression
     function: [(function_expression)
                (parenthesized_expression [(function_expression) (arrow_function)])
                (member_expression
                  object: [(function_expression) (parenthesized_expression [(function_expression) (arrow_function)])]
                  property: (property_identifier) @_invoke)]
     arguments: (arguments [(function_expression) @scope.none (arrow_function) @scope.none
                            (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)])))
   (unary_expression argument: (call_expression
     function: [(function_expression)
                (parenthesized_expression [(function_expression) (arrow_function)])
                (member_expression
                  object: [(function_expression) (parenthesized_expression [(function_expression) (arrow_function)])]
                  property: (property_identifier) @_invoke)]
     arguments: (arguments [(function_expression) @scope.none (arrow_function) @scope.none
                            (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)])))]))
  (#any-of? @_invoke "call" "apply"))

((program (expression_statement
  (call_expression
    function: (identifier) @_define
    arguments: (arguments [(function_expression) (arrow_function)] @scope.none))))
  (#eq? @_define "define"))

; Visibility: an `export` holds its declaration; an export list, a default
; export of a name and `export =` name a declaration of the module or
; namespace whose body holds them; `module.exports` names one in
; CommonJS, or is a class. A re-export (`export { a } from`) names
; nothing here. What an ambient module, an ambient namespace or `declare
; global` holds is exported at any depth, as TypeScript reads it, unless
; its body holds an export declaration or assignment: an export list,
; `export *`, `export =` or `export default` of a name (`export default
; class {}` declares a class, a namespace takes an export list alone);
; then only what an export marks is. `declare module "x"` is public.
(export_statement) @visibility.public

(ambient_declaration [(module body: (statement_block) @visibility.within.public)
                      (internal_module body: (statement_block) @visibility.within.public)
                      (statement_block) @visibility.within.public])

(ambient_declaration
  (module body: (statement_block
    (export_statement [(export_clause) (namespace_export) "*" "=" value: [(identifier) (member_expression)]]))
    @visibility.within.private))

(ambient_declaration
  (internal_module body: (statement_block (export_statement (export_clause))) @visibility.within.private))

(ambient_declaration (module name: (string)) @visibility.public)

((export_statement (export_clause (export_specifier name: (identifier) @export.name)) !source))
((export_statement value: (identifier) @export.name))
((export_statement "=" (identifier) @export.name))
((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @_exports)
   right: [(identifier) @export.name
           (object [(shorthand_property_identifier) @export.name
                    (pair value: (identifier) @export.name)])
           (class) @visibility.public])
  (#eq? @_module "module")
  (#eq? @_exports "exports"))

([(method_definition (accessibility_modifier) @_access)
  (method_signature (accessibility_modifier) @_access)
  (abstract_method_signature (accessibility_modifier) @_access)
  (public_field_definition (accessibility_modifier) @_access)] @visibility.private
  (#eq? @_access "private"))

([(method_definition (accessibility_modifier) @_access)
  (method_signature (accessibility_modifier) @_access)
  (abstract_method_signature (accessibility_modifier) @_access)
  (public_field_definition (accessibility_modifier) @_access)] @visibility.protected
  (#eq? @_access "protected"))

(private_property_identifier) @visibility.private

; The hash: a string's text and a regular expression are one token each,
; spaces kept; a string's quotes hash alike, and its escapes by what they
; mean, an escaped quote as the quote, which a formatter trades when it
; changes the quotes (`'it\'s'` is `"it's"`); a JSX attribute's string has
; no escapes. A template string is walked, so a comment or a space
; inside its `${}` moves nothing. Each statement and each member of a
; class, an interface or a type literal is a group, so `return` and `x`
; on two lines (two statements, by automatic semicolon insertion) hash
; apart from `return x`; the `;` that ends a statement, an empty
; statement in a block, and the `;` or `,` that separates two members are
; left out, as a newline in their place is.
(regex) @hash.unit
(string ["'" "\""] @hash.delimiter)
(string [(string_fragment) (escape_sequence)] @hash.string)
(string (escape_sequence) @hash.escape)

(program (_) @hash.group)
(statement_block (_) @hash.group)
(switch_case ":" (_) @hash.group)
(switch_default ":" (_) @hash.group)
(class_body (_) @hash.group)
(interface_body (_) @hash.group)
(object_type (_) @hash.group)

[(statement_block (empty_statement) @hash.skip)
 (switch_case ":" (empty_statement) @hash.skip)
 (switch_default ":" (empty_statement) @hash.skip)]

; A trailing comma means nothing in a list that has no holes and takes
; one after a rest element: a call's arguments, an object, an import or
; export list, type arguments and parameters, an enum, a tuple type; in a
; definition's parameters and an object pattern after any element but a
; rest, which takes none. An array and an array pattern keep it where a
; hole precedes it (`[a,,]`, `[,]`): where the text from a comma or the
; `[` to the `]` holds a second comma and only whitespace and comments,
; read as the lexer reads them. An anchor in a query passes over
; anonymous nodes, so it cannot tell the last comma from one before it.
; Each alternation of elements is captured: uncaptured, one before an
; anchor and a repeated child matches nothing.
[(arguments "," @hash.skip . (comment)* . ")")
 (object "," @hash.skip . (comment)* . "}")
 (named_imports "," @hash.skip . (comment)* . "}")
 (export_clause "," @hash.skip . (comment)* . "}")
 (type_arguments "," @hash.skip . (comment)* . ">")
 (type_parameters "," @hash.skip . (comment)* . ">")
 (enum_body "," @hash.skip . (comment)* . "}")
 (tuple_type "," @hash.skip . (comment)* . "]")
 (formal_parameters
   [(required_parameter pattern: [(identifier) (this) (object_pattern) (array_pattern)])
    (optional_parameter)] @_element . "," @hash.skip . (comment)* . ")")
 (object_pattern
   [(pair_pattern) (shorthand_property_identifier_pattern) (object_assignment_pattern)] @_element
   . "," @hash.skip . (comment)* . "}")]

([(array "," @hash.skip . (comment)* . "]")
  (array_pattern
    [(identifier) (member_expression) (subscript_expression) (non_null_expression)
     (object_pattern) (array_pattern) (assignment_pattern) (undefined)] @_element
    . "," @hash.skip . (comment)* . "]")] @_list
  (#not-match? @_list "[,\\[](\\s|/\\*([^*]|\\*+[^*/])*\\*+/|//[^\\n\\r]*[\\n\\r])*,(\\s|/\\*([^*]|\\*+[^*/])*\\*+/|//[^\\n\\r]*[\\n\\r])*\\]$"))

; The parentheses of an arrow function's one parameter, a name with no
; type or default, when the arrow has no return type: `(x) => 1` is
; `x => 1`; `(x: T) => x`, `(x): T => x` and `(x = y) => 1` keep them, as
; `x: T => x` is a labeled statement and `x = y => 1` an assignment.
(arrow_function !return_type
  parameters: (formal_parameters
    . "(" @hash.skip . (required_parameter pattern: (identifier) !type !value) . ")" @hash.skip .))

; Parentheses that change no meaning: around the value a `return` or a
; `throw` takes, around an expression (no sequence) that is a branch of a
; conditional, and around a conditional that is an arrow function's body.
; A `return` whose value starts on the next line is a statement of its own
; (automatic semicolon insertion), whose group the hash holds.
[(return_statement (parenthesized_expression "(" @hash.skip ")" @hash.skip))
 (throw_statement (parenthesized_expression "(" @hash.skip ")" @hash.skip))
 (ternary_expression
   consequence: (parenthesized_expression "(" @hash.skip . (comment)* . (expression) . (comment)* . ")" @hash.skip))
 (ternary_expression
   alternative: (parenthesized_expression "(" @hash.skip . (comment)* . (expression) . (comment)* . ")" @hash.skip))
 (arrow_function
   body: (parenthesized_expression "(" @hash.skip . (comment)* . (ternary_expression) . (comment)* . ")" @hash.skip))]

; Parentheses around a type that is an element of a tuple or a branch of
; a conditional type, each a whole type: `[(A | B)]` is `[A | B]`; around
; an array's element type (`(A | B)[]`) they bind.
[(tuple_type (parenthesized_type "(" @hash.skip ")" @hash.skip))
 (conditional_type consequence: (parenthesized_type "(" @hash.skip ")" @hash.skip))
 (conditional_type alternative: (parenthesized_type "(" @hash.skip ")" @hash.skip))]

; A union's or an intersection's leading operator, which has no operand
; before it.
[(union_type . "|" @hash.skip)
 (intersection_type . "&" @hash.skip)]

[(expression_statement ";" @hash.skip)
 (lexical_declaration ";" @hash.skip)
 (variable_declaration ";" @hash.skip)
 (export_statement ";" @hash.skip)
 (import_statement ";" @hash.skip)
 (do_statement ";" @hash.skip)
 (break_statement ";" @hash.skip)
 (continue_statement ";" @hash.skip)
 (debugger_statement ";" @hash.skip)
 (return_statement ";" @hash.skip)
 (throw_statement ";" @hash.skip)
 (function_signature ";" @hash.skip)
 (ambient_declaration ";" @hash.skip)
 (import_alias ";" @hash.skip)
 (type_alias_declaration ";" @hash.skip)
 (class_body [";" ","] @hash.skip)
 (interface_body [";" ","] @hash.skip)
 (object_type [";" ","] @hash.skip)]
