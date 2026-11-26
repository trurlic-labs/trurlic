; TSX's rules beside TypeScript's (typescript.scm): the text of a JSX
; element, its character references among it, is hashed by its characters
; as JSX renders it, so a reformat that breaks or indents its lines moves
; nothing; and an expression that holds none, `{}` or the `{/* c */}` of a
; comment, is left out as a comment is, the text around it hashed as
; joined.
[(jsx_text) (html_character_reference)] @hash.chars

(jsx_expression "{" @hash.skip . (comment)* . "}" @hash.skip)

; A string of spaces alone in an expression, `{" "}`, renders as those
; spaces in the text, so it is hashed as they are.
((jsx_expression "{" @hash.skip . (string) @_spaces . "}" @hash.skip)
  (#match? @_spaces "^(\" +\"|' +')$"))

((jsx_expression
   . (string ["'" "\""] @hash.skip (string_fragment) @hash.chars ["'" "\""] @hash.skip) @_spaces .)
  (#match? @_spaces "^(\" +\"|' +')$"))

; Parentheses around an element, which is a primary expression wherever
; it stands.
(parenthesized_expression
  "(" @hash.skip . (comment)* . [(jsx_element) (jsx_self_closing_element)] . (comment)* . ")" @hash.skip)
