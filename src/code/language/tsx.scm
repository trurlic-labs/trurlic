; TSX's rules beside TypeScript's (typescript.scm): the text of a JSX
; element is hashed by its words, so a reformat that breaks or indents its
; lines moves nothing; and an expression that holds none, `{}` or the
; `{/* c */}` of a comment, is left out as a comment is.
(jsx_text) @hash.words

(jsx_expression "{" @hash.skip . (comment)* . "}" @hash.skip)
