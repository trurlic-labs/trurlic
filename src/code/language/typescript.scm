; TypeScript, and with tsx.scm TSX and JavaScript, read by the general
; resolver (src/code/general.rs) under the contract of src/code/query.rs.
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
; names `x`, which the export list rule makes public) a variable. An
; unnamed function or class `module.exports` is, CommonJS's default, is
; named so too.
((export_statement "default" @name value: [(function_expression) (arrow_function) (generator_function)]) @definition
  (#set! kind "function")
  (#set! exclusive "true"))

((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @name)
   right: [(function_expression) (arrow_function) (generator_function)])
   @definition
  (#eq? @_module "module")
  (#eq? @name "exports")
  (#set! kind "function")
  (#set! name.text "default")
  (#set! definition.to "expression_statement")
  (#set! visibility "public"))

((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @name)
   right: (class !name) @scope.none)
   @definition
  (#eq? @_module "module")
  (#eq? @name "exports")
  (#set! kind "class")
  (#set! name.text "default")
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

; Tests are calls of `describe`, `it`, `test` and their kin, named by
; their first argument when it is a string, under the tests whose
; callbacks hold them: the runner itself (`it("a")`), or a chain of one
; or two members from it, called directly or through a call (`it.only`,
; `test.describe.serial`, `describe.each(table)`, `describe.runIf(x)`),
; whose string is followed by a function, right after it or after one
; argument between (Playwright's details). A member call with no
; function there (`test.set("a", "b")`) is no test, and `context`, the
; usual name of a Playwright or canvas context, roots no chain
; (`context.route("x", handler)`).
((call_expression
   function: (identifier) @_test
   arguments: (arguments . [(string) (template_string)] @name)) @definition @role.test
  (#any-of? @_test "describe" "context" "suite" "it" "test" "specify" "bench" "fdescribe" "xdescribe" "fit" "xit")
  (#set! kind "test"))

((call_expression
   function: [(member_expression object: [(identifier) @_test (member_expression object: (identifier) @_test)])
              (call_expression function: (member_expression object: [(identifier) @_test (member_expression object: (identifier) @_test)]))]
   arguments: (arguments . [(string) (template_string)] @name . (_)? . [(arrow_function) (function_expression)])) @definition @role.test
  (#any-of? @_test "describe" "suite" "it" "test" "specify" "bench" "fdescribe" "xdescribe" "fit" "xit")
  (#set! kind "test"))

; Unnamed scopes: a function, method or class no pattern names. Its
; locals define nothing; a function in it is under the definition around
; it (`describe("a", () => { function f() {} })` is `a::f`), or defines
; nothing when no definition is (`app.get("/", (req) => { ... })`).
[(arrow_function) (function_expression) (generator_function) (method_definition) (class)] @scope.unnamed

; No scope: a class a name binds, whose members are that name's, and a
; wrapper of the whole file, which is the module: an unnamed function
; called by a statement of the file's top (an IIFE, `(function () {}())`,
; `!function () {}()`, `(function () {}).call(this)`), a factory such a
; call passes (UMD), and the factory of a top-level `define` (AMD).
[(variable_declarator value: (class) @scope.none)
 (export_statement "default" value: (class) @scope.none)]

(program (expression_statement [
  (call_expression function: (parenthesized_expression [(function_expression) (arrow_function)] @scope.none))
  (call_expression function: (member_expression object: (parenthesized_expression [(function_expression) (arrow_function)] @scope.none)))
  (parenthesized_expression (call_expression function: (function_expression) @scope.none))
  (unary_expression argument: (call_expression function: (function_expression) @scope.none))]))

(program (expression_statement
  (call_expression
    function: (parenthesized_expression [(function_expression) (arrow_function)])
    arguments: (arguments [(function_expression) @scope.none
                           (arrow_function) @scope.none
                           (parenthesized_expression (function_expression) @scope.none)]))))

((program (expression_statement
  (call_expression
    function: (identifier) @_define
    arguments: (arguments [(function_expression) (arrow_function)] @scope.none))))
  (#eq? @_define "define"))

; Visibility: an `export` holds its declaration; an export list, a default
; export of a name and `export =` name a declaration of the module's top;
; `module.exports` names one in CommonJS, or is a class. A re-export
; (`export { a } from`) names nothing here. What an ambient module, an
; ambient namespace or `declare global` holds is exported at any depth
; unless a mark says otherwise, and `declare module "x"` is public.
(export_statement) @visibility.public

(ambient_declaration [(module body: (statement_block) @visibility.within.public)
                      (internal_module body: (statement_block) @visibility.within.public)
                      (statement_block) @visibility.within.public])

(ambient_declaration [(module body: (statement_block (export_statement [(export_clause) (namespace_export) "*" "=" value: (_)])) @visibility.within.private)
                      (internal_module body: (statement_block (export_statement [(export_clause) (namespace_export) "*" "=" value: (_)])) @visibility.within.private)])

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

; The hash: a string and a regular expression are one token each, spaces
; kept; a template string is walked, so a comment or a space inside its
; `${}` moves nothing. Each statement and each member of a class, an
; interface or a type literal is a group, so `return` and `x` on two lines
; (two statements, by automatic semicolon insertion) hash apart from
; `return x`; the `;` that ends a statement and the `;` or `,` that
; separates two members are left out, as a newline in their place is.
[(string) (regex)] @hash.unit

(program (_) @hash.group)
(statement_block (_) @hash.group)
(switch_case body: (_) @hash.group)
(switch_default body: (_) @hash.group)
(class_body (_) @hash.group)
(interface_body (_) @hash.group)
(object_type (_) @hash.group)

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
