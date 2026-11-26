//! A language's query file and the contract the general resolver holds it
//! to. tree-sitter compiles a query against its grammar and refuses an
//! unknown node kind or field, a syntax error and an impossible shape;
//! it accepts any capture name, property and predicate it does not
//! evaluate itself, so the contract refuses each of those here: a query
//! is read as written or not at all.
//!
//! A pattern is a definition pattern or a mark pattern, and every pattern
//! has one root node (the resolver starts each pattern at one node).
//!
//! - A definition pattern captures `@definition`, the node its span is,
//!   once, and `@name`, the node its own path segments are, once or on
//!   several nodes, whose segments follow one another in the order of the
//!   file (`TEST(Suite, Name)` is `Suite::Name`), and sets its `kind`. Its other properties: `kind.in "class:method"` (its kind
//!   in a container of a kind), `container "false"` (what its span holds
//!   is not under it), `within "top class"` (kept only at the file's top
//!   or in a container of those kinds), `definition.via` and
//!   `definition.to` (its span grows from the captured node up through
//!   ancestors of the `via` kinds to the first of a `to` kind, or it is
//!   dropped), `definition.wrap` (its span grows over each ancestor of
//!   those kinds right above it), `definition.preceding` (its span takes
//!   in the run of siblings of those kinds right before it, comments
//!   between them included), `exclusive "true"` (a match of it drops
//!   every match of a pattern without it that captured the same node as
//!   its `@definition`), `name.separator "."` (each leaf of the name with
//!   that text ends one segment and starts the next), `name.skip
//!   "template_argument_list"` (the leaves under nodes of those kinds are
//!   no part of its name), `name.text "default"` (its one segment,
//!   whatever its name node's text), and `visibility` (its visibility when
//!   no mark states one).
//! - A mark pattern captures any of `@role.test` (a definition whose name
//!   lies inside is test code), `@visibility.<name>` (a definition whose
//!   span holds the node, and the node its name, has that visibility; the
//!   innermost such mark wins), `@visibility.within.<name>` (a definition
//!   whose name lies inside has that visibility when no mark of the first
//!   form or export states one; the innermost wins, and of two on one node
//!   the one `Visibility` orders first), `@visibility.after.<name>` (a
//!   label: a definition whose name lies in a later sibling of the node,
//!   up to the next such label among them, has that visibility over a
//!   mark of either other form: a C++ `public:`, or a class body's `{`
//!   for its default), `@visibility.through` (a label among the
//!   node's children holds for the node's later siblings too: a `#if`
//!   arm's), `@export.name` (a definition named as the node's text,
//!   directly in the container that holds the node, is public when no
//!   mark of the first form or label states its visibility),
//!   `@scope.unnamed` (a scope no name stands for: see below),
//!   `@scope.none` (no scope, whatever `@scope.unnamed` captures),
//!   `@hash.unit` (one token of its text, the comments in it left out, cut
//!   where each of its children with a hash mark of its own is taken by
//!   it), `@hash.tokens` (one token per token its
//!   grammar's lexicon lexes in it: a C macro's body), `@hash.chars` (its text as JSX renders it, with the text
//!   of such nodes only whitespace parts from it: a token a character, an
//!   empty one a space, a run of whitespace that holds a line break one
//!   space between text and none at an end), `@hash.delimiter` (a string's delimiter, hashed as what
//!   it means: its prefix letters in any case and order, `u` none, each
//!   quote `'` or `"` alike; the escapes a raw string's end holds are its
//!   text), `@hash.join` (a delimiter that starts a string the one before
//!   continues: left out with that one's end where both start alike, so
//!   their texts are one), `@hash.string` (a string's text, one token with
//!   the text that continues it, its escapes spelled as
//!   `@hash.escape` captures them), `@hash.escape` (an escape of a string's
//!   text, hashed as what it means: a quote escape as its quote, hex digits
//!   and a name in any case, an octal one of three digits),
//!   `@hash.literal` (a number, hashed as what it means: its letters in any
//!   case, a `.` with no digit beside it as one with `0`, an exponent's
//!   `+` none), `@hash.skip` (left out of every hash) and
//!   `@hash.group` (a statement or block, whose extent the hash holds),
//!   and sets no property. A definition pattern may capture these too.
//!
//! A definition is under the innermost definition or unnamed scope whose
//! captured node holds its own `@definition` node. An unnamed scope that a
//! definition captures as its `@definition` is that definition's. Any
//! other is a container of its own that `within` never admits, whose path
//! is the definition around it: a function's locals stay locals, and a
//! function in a callback is under the definition that holds the callback.
//! What an unnamed scope holds that no definition holds defines nothing.
//!
//! A name is the text of its node's leaves, comments left out, one space
//! where bytes lie between two leaves that a word character ends and
//! starts (`operator ==` is `operator==`, `operator bool` keeps its
//! space); a name written as a string literal
//! is the text between its quotes; and a `::` in a name splits it as it
//! splits a symbol. A capture whose name starts with `_` is read by a text
//! predicate alone (`#eq?`, `#match?`, `#any-of?` and their `not-` forms,
//! which tree-sitter evaluates). Kinds and visibilities are named as
//! `DefinitionKind::name` and `Visibility::name` write them. Which
//! definition a match makes never depends on the order of the patterns.

mod compile;
mod rules;

pub(in crate::code::parse) use rules::{Capture, Container, Definer};
pub(in crate::code) use rules::{QueryFault, Rules};
