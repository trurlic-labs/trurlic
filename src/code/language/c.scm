; C, read by the general resolver (src/code/general.rs) under the contract
; of src/code/query.rs, after the tree's attribute macros are blanked
; (src/code/language/c_family.rs).
;
; A declarator nests (pointers, arrays, parentheses, function
; declarators). A function's span is its definition or its declaration
; itself, its name met through the declarators real code writes; any
; other name is matched where it stands, and its span grows through the
; declarators to its declaration. A struct nested in another is under it
; (`outer::inner`), and so is an enum's member (`color::RED`), as written.
;
; Visibility: a definition is public unless `static` makes it private; a
; macro states none.

; Functions with a return type: defined, a pointer or a function pointer
; returned among them, to two levels (a function that returns a pointer
; to a function that returns a function pointer), and declared (a
; prototype) at the file's top; a name in parentheses
; (`int (max)(int, int)`) is the function's.
((function_definition type: (_)
   declarator: [(function_declarator declarator: (identifier) @name)
                (function_declarator declarator: (parenthesized_declarator (identifier) @name))
                (_ declarator: (function_declarator declarator: (parenthesized_declarator (identifier) @name)))
                (_ declarator: (function_declarator declarator: (identifier) @name))
                (_ declarator: (_ declarator: (function_declarator declarator: (identifier) @name)))
                (function_declarator declarator: (parenthesized_declarator (_ declarator: (function_declarator declarator: (identifier) @name))))
                (function_declarator declarator: (parenthesized_declarator (_ declarator: (function_declarator declarator: (parenthesized_declarator (_ declarator: (function_declarator declarator: (identifier) @name)))))))]) @definition
  (#set! kind "function")
  (#set! visibility "public"))

((declaration type: (_)
   declarator: [(function_declarator declarator: (identifier) @name)
                (function_declarator declarator: (parenthesized_declarator (identifier) @name))
                (_ declarator: (function_declarator declarator: (parenthesized_declarator (identifier) @name)))
                (_ declarator: (function_declarator declarator: (identifier) @name))
                (function_declarator declarator: (parenthesized_declarator (_ declarator: (function_declarator declarator: (identifier) @name))))
                (function_declarator declarator: (parenthesized_declarator (_ declarator: (function_declarator declarator: (parenthesized_declarator (_ declarator: (function_declarator declarator: (identifier) @name)))))))]) @definition
  (#set! kind "function")
  (#set! container "false")
  (#set! within "top")
  (#set! visibility "public"))

; Variables at the file's top, a function pointer among them; a
; parameter's or a local's name is none, and a body no definition
; captures (a misparse's) is a scope whose locals define nothing.
(compound_statement) @scope.unnamed

([(declaration declarator: (identifier) @name)
  (init_declarator declarator: (identifier) @name)
  (pointer_declarator declarator: (identifier) @name)
  (array_declarator declarator: (identifier) @name)] @definition
  (#set! kind "variable")
  (#set! container "false")
  (#set! within "top")
  (#set! definition.via "pointer_declarator array_declarator parenthesized_declarator function_declarator init_declarator attributed_declarator")
  (#set! definition.to "declaration")
  (#set! visibility "public"))

; Structs, unions and enums with a body; one with no name a typedef names
; is named by it, and holds its members.
((struct_specifier name: (type_identifier) @name body: (_)) @definition
  (#set! kind "struct")
  (#set! visibility "public"))

((union_specifier name: (type_identifier) @name body: (_)) @definition
  (#set! kind "union")
  (#set! visibility "public"))

((enum_specifier name: (type_identifier) @name body: (_)) @definition
  (#set! kind "enum")
  (#set! visibility "public"))

((type_definition type: (struct_specifier !name body: (_)) declarator: (type_identifier) @name) @definition
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

; A typedef is a type alias, a function pointer's among them.
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

; An enum's members, under it.
((enumerator name: (identifier) @name) @definition
  (#set! kind "variant")
  (#set! visibility "public"))

; Fields, a function pointer's among them. A field whose type is an
; unnamed struct or union holds its members; the members of one with no
; field name are the outer struct's own (C11).
([(field_declaration declarator: (field_identifier) @name)
  (pointer_declarator declarator: (field_identifier) @name)
  (array_declarator declarator: (field_identifier) @name)
  (parenthesized_declarator (field_identifier) @name)] @definition
  (#set! kind "field")
  (#set! container "false")
  (#set! definition.via "pointer_declarator array_declarator parenthesized_declarator function_declarator")
  (#set! definition.to "field_declaration")
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

; `static`: internal linkage.
([(function_definition (storage_class_specifier) @_static)
  (declaration (storage_class_specifier) @_static)] @visibility.private
  (#eq? @_static "static"))

; The hash: a literal is one token, its spaces kept; a macro's body one
; unit a token of the C lexer, so its spaces and the line breaks it
; continues over move nothing.
[(string_literal) (char_literal) (system_lib_string)] @hash.unit

(preproc_arg) @hash.tokens
