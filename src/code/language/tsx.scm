; TSX's rules beside TypeScript's (typescript.scm): the text of a JSX
; element is hashed by its words, so a reformat that breaks or indents its
; lines moves nothing.
(jsx_text) @hash.words
