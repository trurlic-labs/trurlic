; TypeScript, and with tsx.scm TSX and JavaScript, read by the general
; resolver (src/code/general.rs) under the contract of src/code/query.rs.
;
; A declaration's span grows over the `export`, `declare` or statement
; around it. A declaration at a module's or a namespace's top is private
; unless an `export` holds it or an export list names it; a member is
; public unless `private`, `protected` or `#` says otherwise.

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

; Any other name a module or a namespace binds: each of `const a = 1, b`,
; and each name a destructuring binds. A function's locals define nothing.
((variable_declarator name: (identifier) @name) @definition
  (#set! kind "variable")
  (#set! within "top module")
  (#set! definition.to "lexical_declaration variable_declaration")
  (#set! definition.wrap "export_statement ambient_declaration")
  (#set! visibility "private"))

([(object_pattern (shorthand_property_identifier_pattern) @name)
  (pair_pattern value: (identifier) @name)
  (array_pattern (identifier) @name)
  (rest_pattern (identifier) @name)
  (object_assignment_pattern left: (shorthand_property_identifier_pattern) @name)
  (assignment_pattern left: (identifier) @name)] @definition
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
; its kind, any other value but a single token (`export default x` names
; `x`, which the export list rule makes public) a variable.
((export_statement "default" @name value: [(function_expression) (arrow_function) (generator_function)]) @definition
  (#set! kind "function")
  (#set! exclusive "true"))

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
  (#set! definition.preceding "decorator")
  (#set! visibility "public"))

((class_body (public_field_definition name: (_) @name) @definition)
  (#set! kind "field")
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

; Tests are calls: `describe`, `it`, `test` and their kin, with `.only`,
; `.skip`, `.each(..)` and the like, named by their first argument when it
; is a string, under the tests whose callbacks hold them.
((call_expression
   function: [(identifier) @_test
              (member_expression object: (identifier) @_test)
              (call_expression function: (member_expression object: (identifier) @_test))]
   arguments: (arguments . [(string) (template_string)] @name)) @definition @role.test
  (#any-of? @_test "describe" "context" "suite" "it" "test" "specify" "bench" "fdescribe" "xdescribe" "fit" "xit")
  (#set! kind "test"))

; Visibility: an `export` holds its declaration; an export list, a default
; export of a name and `export =` name a declaration of the module's top;
; `module.exports` names one in CommonJS. A re-export (`export { a } from`)
; names nothing here.
(export_statement) @visibility.public

((export_statement (export_clause (export_specifier name: (identifier) @export.name)) !source))
((export_statement value: (identifier) @export.name))
((export_statement "=" (identifier) @export.name))
((assignment_expression
   left: (member_expression object: (identifier) @_module property: (property_identifier) @_exports)
   right: [(identifier) @export.name
           (object [(shorthand_property_identifier) @export.name
                    (pair value: (identifier) @export.name)])])
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
