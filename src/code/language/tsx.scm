; TSX's rules beside TypeScript's (typescript.scm): the text of a JSX
; element is hashed by its characters as JSX renders it, so a reformat
; that breaks or indents its lines moves nothing; and an expression that
; holds none, `{}` or the `{/* c */}` of a comment, is left out as a
; comment is, the text around it hashed as joined.
(jsx_text) @hash.chars

(jsx_expression "{" @hash.skip . (comment)* . "}" @hash.skip)
