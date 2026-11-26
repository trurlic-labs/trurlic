; A TypeScript declaration file's rules beside TypeScript's
; (typescript.scm): its top is an ambient context, as TypeScript reads
; it. A script's declarations are global and a module's are its exports,
; each public, unless the file holds an export declaration or
; assignment: an export list, `export *`, `export =` or `export default`
; of a name; then only what an export marks or names is.
(program) @visibility.within.public

((program
   (export_statement [(export_clause) (namespace_export) "*" "=" value: [(identifier) (member_expression)]]))
  @visibility.within.private)
