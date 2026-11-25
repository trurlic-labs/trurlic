; C++, and every C or C++ header, read by the general resolver
; (src/code/general.rs) under the contract of src/code/query.rs, after the
; tree's attribute and namespace macros are blanked
; (src/code/language/c_family.rs).
;
; As C (c.scm), plus namespaces, classes and their members, templates (a
; template's span grows over its `template <...>`, and its arguments are
; no part of a name, so a specialization shares its template's path),
; names qualified out of their class (`void a::Foo::f() {}` is
; `a::Foo::f`, a function: no syntax says whether `a::Foo` is a class),
; operators, conversions, constructors and destructors, and googletest's
; tests. A friend is a function under the class it is written in.
;
; Visibility: a definition at a namespace's top is public unless `static`
; or an anonymous namespace makes it private; a member of a class is
; private and one of a struct or a union public, up to an access label.

; Namespaces: `namespace a::b {}` is two segments, an inline one is one;
; an anonymous one adds none.
((namespace_definition name: (_) @name) @definition
  (#set! kind "module")
  (#set! visibility "public"))

; Classes, structs, unions and enums with a body, named as written
; (`Box<int>` is `Box`, `DBImpl::Writer` two segments).
((class_specifier name: (_) @name body: (_)) @definition
  (#set! kind "class")
  (#set! name.skip "template_argument_list")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

((struct_specifier name: (_) @name body: (_)) @definition
  (#set! kind "struct")
  (#set! name.skip "template_argument_list")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

((union_specifier name: (_) @name body: (_)) @definition
  (#set! kind "union")
  (#set! name.skip "template_argument_list")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

((enum_specifier name: (_) @name body: (_)) @definition
  (#set! kind "enum")
  (#set! visibility "public"))

; A struct, union or enum with no name a typedef names is named by it.
((type_definition type: [(struct_specifier !name body: (_)) (class_specifier !name body: (_))] declarator: (type_identifier) @name) @definition
  (#set! kind "struct")
  (#set! exclusive "true")
  (#set! visibility "public"))

((type_definition type: (union_specifier !name body: (_)) declarator: (type_identifier) @name) @definition
  (#set! kind "union")
  (#set! exclusive "true")
  (#set! visibility "public"))

((type_definition type: (enum_specifier !name body: (_)) declarator: (type_identifier) @name) @definition
  (#set! kind "enum")
  (#set! exclusive "true")
  (#set! visibility "public"))

; Functions with a return type: defined, or declared at a namespace's top
; or in a class; a method in a class. The span is the definition or the
; declaration itself, its name met through the declarators real code
; writes (a pointer or a reference returned). A name in parentheses is a
; function pointer's, and one holding a brace or a semicolon a misparse's.
((function_definition type: (_)
   declarator: [(function_declarator declarator: (_) @name)
                (_ declarator: (function_declarator declarator: (_) @name))
                (reference_declarator (function_declarator declarator: (_) @name))
                (_ declarator: (_ declarator: (function_declarator declarator: (_) @name)))]) @definition
  (#not-match? @name "^[(]|[{};]")
  (#set! kind "function")
  (#set! kind.in "class:method struct:method union:method")
  (#set! name.skip "template_argument_list")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

([(declaration type: (_)
   declarator: [(function_declarator declarator: (_) @name)
                (_ declarator: (function_declarator declarator: (_) @name))
                (reference_declarator (function_declarator declarator: (_) @name))])
  (field_declaration type: (_)
   declarator: [(function_declarator declarator: (_) @name)
                (_ declarator: (function_declarator declarator: (_) @name))
                (reference_declarator (function_declarator declarator: (_) @name))])] @definition
  (#not-match? @name "^[(]|[{};]")
  (#set! kind "function")
  (#set! kind.in "class:method struct:method union:method")
  (#set! container "false")
  (#set! within "top module class struct union")
  (#set! name.skip "template_argument_list")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

; With no return type: a constructor, a destructor or a conversion,
; defined out of its class or in it, or declared in it. A declaration
; with no type at a namespace's top, or named as no class is, is a macro's
; call (`BIT(flag);`), no definition.
((function_definition !type
   declarator: [(function_declarator declarator: [(qualified_identifier) (destructor_name)] @name)
                (operator_cast) @name
                (qualified_identifier) @name]) @definition
  (#set! kind "function")
  (#set! kind.in "class:method struct:method union:method")
  (#set! name.skip "template_argument_list abstract_function_declarator")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

((field_declaration_list
   (declaration !type
     declarator: [(function_declarator declarator: (destructor_name) @name) (operator_cast) @name]) @definition)
  (#set! kind "method")
  (#set! name.skip "abstract_function_declarator")
  (#set! visibility "public"))

([(class_specifier
    name: [(type_identifier) @_class (template_type name: (type_identifier) @_class)]
    body: (field_declaration_list
      (_ !type declarator: (function_declarator declarator: (identifier) @name)) @definition))
  (struct_specifier
    name: [(type_identifier) @_class (template_type name: (type_identifier) @_class)]
    body: (field_declaration_list
      (_ !type declarator: (function_declarator declarator: (identifier) @name)) @definition))]
  (#eq? @_class @name)
  (#set! kind "method")
  (#set! visibility "public"))

; A friend, declared or defined in its class, is a function of the
; class's namespace, written under the class.
([(friend_declaration
   (declaration
     declarator: [(function_declarator declarator: (_) @name)
                  (_ declarator: (function_declarator declarator: (_) @name))]) @definition)
  (friend_declaration
   (function_definition
     declarator: [(function_declarator declarator: (_) @name)
                  (_ declarator: (function_declarator declarator: (_) @name))]) @definition)]
  (#set! kind "function")
  (#set! exclusive "true")
  (#set! name.skip "template_argument_list")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

; A lambda a namespace binds is a function; any other lambda is a scope
; of its own, whose locals and types stay its own.
(lambda_expression) @scope.unnamed

((init_declarator declarator: (identifier) @name value: (lambda_expression)) @definition
  (#set! kind "function")
  (#set! exclusive "true")
  (#set! within "top module")
  (#set! definition.to "declaration")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

; Variables at a namespace's top, a function pointer among them, and a
; static member defined out of its class (`int Foo::count = 0;`).
([(declaration declarator: [(identifier) (qualified_identifier)] @name)
  (init_declarator declarator: [(identifier) (qualified_identifier)] @name)
  (pointer_declarator declarator: [(identifier) (qualified_identifier)] @name)
  (reference_declarator [(identifier) (qualified_identifier)] @name)
  (array_declarator declarator: [(identifier) (qualified_identifier)] @name)
  (parenthesized_declarator [(identifier) (qualified_identifier)] @name)] @definition
  (#set! kind "variable")
  (#set! container "false")
  (#set! within "top module")
  (#set! definition.via "pointer_declarator reference_declarator array_declarator parenthesized_declarator function_declarator init_declarator attributed_declarator")
  (#set! definition.to "declaration")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

; Type aliases: a `typedef`, a function pointer's among them, and a
; `using`.
((type_definition declarator: (type_identifier) @name) @definition
  (#set! kind "type_alias")
  (#set! container "false")
  (#set! visibility "public"))

([(pointer_declarator declarator: (type_identifier) @name)
  (array_declarator declarator: (type_identifier) @name)
  (parenthesized_declarator (type_identifier) @name)
  (function_declarator declarator: (type_identifier) @name)] @definition
  (#set! kind "type_alias")
  (#set! container "false")
  (#set! definition.via "pointer_declarator array_declarator parenthesized_declarator function_declarator")
  (#set! definition.to "type_definition")
  (#set! visibility "public"))

((alias_declaration name: (type_identifier) @name) @definition
  (#set! kind "type_alias")
  (#set! container "false")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

; An enum's members, under it.
((enumerator name: (identifier) @name) @definition
  (#set! kind "variant")
  (#set! visibility "public"))

; Data members, a static one and a function pointer among them. A member
; whose type is an unnamed struct or union holds its members; those of
; one with no member name are the outer type's own.
([(field_declaration declarator: (field_identifier) @name)
  (pointer_declarator declarator: (field_identifier) @name)
  (reference_declarator (field_identifier) @name)
  (array_declarator declarator: (field_identifier) @name)
  (parenthesized_declarator (field_identifier) @name)] @definition
  (#set! kind "field")
  (#set! container "false")
  (#set! definition.via "pointer_declarator reference_declarator array_declarator parenthesized_declarator function_declarator")
  (#set! definition.to "field_declaration")
  (#set! definition.wrap "template_declaration")
  (#set! visibility "public"))

((field_declaration
   type: [(struct_specifier !name body: (_)) (union_specifier !name body: (_))]
   declarator: (field_identifier) @name) @definition
  (#set! kind "field")
  (#set! exclusive "true")
  (#set! visibility "public"))

; Macros, object-like and function-like, where their `#define` stands.
([(preproc_def name: (identifier) @name)
  (preproc_function_def name: (identifier) @name)] @definition
  (#set! kind "macro")
  (#set! container "false"))

; googletest's tests: `TEST(Suite, Name) {}` and its kin are `Suite::Name`.
((function_definition
   declarator: (function_declarator
     declarator: (identifier) @_macro
     parameters: (parameter_list . (parameter_declaration type: (_) @name) . (parameter_declaration type: (_) @name) .))) @definition @role.test
  (#any-of? @_macro "TEST" "TEST_F" "TEST_P" "TYPED_TEST" "TYPED_TEST_P")
  (#set! kind "test")
  (#set! exclusive "true"))

; Visibility. `static` is internal linkage, and an anonymous namespace
; makes all it holds private; a class's members are private and a
; struct's or a union's public, up to an access label, which wins over
; `static` in a class.
([(function_definition (storage_class_specifier) @_static)
  (declaration (storage_class_specifier) @_static)] @visibility.private
  (#eq? @_static "static"))

(namespace_definition !name body: (_) @visibility.within.private)

(class_specifier body: (field_declaration_list "{" @visibility.after.private))

([(struct_specifier body: (field_declaration_list "{" @visibility.after.public))
  (union_specifier body: (field_declaration_list "{" @visibility.after.public))])

((access_specifier) @_access @visibility.after.public (#eq? @_access "public"))
((access_specifier) @_access @visibility.after.private (#eq? @_access "private"))
((access_specifier) @_access @visibility.after.protected (#eq? @_access "protected"))

; The hash: a literal is one token, its spaces kept; a macro's body one
; token per word, so its spaces and the line breaks it continues over
; move nothing.
[(string_literal) (char_literal) (raw_string_literal) (system_lib_string)] @hash.unit

(preproc_arg) @hash.words
