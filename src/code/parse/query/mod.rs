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

use std::fmt;

use tree_sitter::{Language, Query, QueryError};

use crate::code::index::{DefinitionKind, Visibility};

/// What a capture of a query is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::code) enum Capture {
    Definition,
    Name,
    Test,
    Visibility(Visibility),
    Within(Visibility),
    /// A label: the later siblings of the node have the visibility.
    After(Visibility),
    /// A label among the node's children holds for its later siblings.
    Through,
    Export,
    Unnamed,
    NoScope,
    Unit,
    Tokens,
    Chars,
    Delimiter,
    Join,
    String,
    Escape,
    Literal,
    Skip,
    Group,
    /// Read by a text predicate alone.
    Predicate,
}

/// Where a definition may stand: at the file's top, in a container of a
/// kind, or in an unnamed scope, which no `within` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::code) enum Container {
    Top,
    Kind(DefinitionKind),
    Unnamed,
}

/// How a definition pattern makes a definition of what it captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::code) struct Definer {
    pub(in crate::code) kind: DefinitionKind,
    /// Its kind in a container of a kind, by the container's.
    pub(in crate::code) kind_in: Vec<(DefinitionKind, DefinitionKind)>,
    /// What its span holds is under it.
    pub(in crate::code) container: bool,
    /// The containers it is kept in; `None` for every one.
    pub(in crate::code) within: Option<Vec<Container>>,
    /// The growth of its span, by its place in [`Rules::growths`].
    pub(in crate::code) growth: Option<usize>,
    /// The wrapping of its span, by its place in [`Rules::wraps`].
    pub(in crate::code) wrap: Option<usize>,
    /// The siblings its span takes in, by their place in
    /// [`Rules::precedings`].
    pub(in crate::code) preceding: Option<usize>,
    /// A match of it drops the matches of patterns without it that
    /// captured its `@definition` node.
    pub(in crate::code) exclusive: bool,
    /// The text of a leaf of its name that separates two segments.
    pub(in crate::code) separator: Option<String>,
    /// The node kinds whose leaves its name leaves out.
    pub(in crate::code) skip: Vec<u16>,
    /// The text of its one segment, in place of its name node's.
    pub(in crate::code) text: Option<String>,
    pub(in crate::code) visibility: Option<Visibility>,
}

impl Definer {
    /// Whether it is kept in `container`.
    pub(in crate::code) fn admits(&self, container: Container) -> bool {
        self.within
            .as_ref()
            .is_none_or(|within| within.contains(&container))
    }

    /// Its kind in `container`.
    pub(in crate::code) fn kind_in(&self, container: Container) -> DefinitionKind {
        self.kind_in
            .iter()
            .find(|(outer, _)| Container::Kind(*outer) == container)
            .map_or(self.kind, |(_, kind)| *kind)
    }
}

/// A span's growth: from the captured node up through ancestors of the
/// `via` kinds to the first of a `to` kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::code) struct Growth {
    pub(in crate::code) via: Vec<u16>,
    pub(in crate::code) to: Vec<u16>,
}

/// A language's query, compiled and read by its contract.
#[derive(Debug)]
pub(in crate::code) struct Rules {
    pub(in crate::code) grammar: Language,
    pub(in crate::code) query: Query,
    /// By capture index.
    pub(in crate::code) captures: Vec<Capture>,
    /// By pattern index: the definer of a definition pattern.
    pub(in crate::code) patterns: Vec<Option<Definer>>,
    /// The distinct growths of the patterns.
    pub(in crate::code) growths: Vec<Growth>,
    /// The distinct sets of kinds a span wraps over.
    pub(in crate::code) wraps: Vec<Vec<u16>>,
    /// The distinct sets of kinds of the siblings a span takes in.
    pub(in crate::code) precedings: Vec<Vec<u16>>,
}

/// Why a query file breaks its contract.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::code) enum QueryFault {
    Compile(QueryError),
    UnknownCapture(String),
    NotRooted { pattern: usize },
    UnknownPredicate { pattern: usize, operator: String },
    UnknownProperty { pattern: usize, key: String },
    BadValue { pattern: usize, key: String },
    NotADefinition { pattern: usize },
    UnknownNodeKind { pattern: usize, name: String },
    UnknownContainer { pattern: usize, name: String },
}

impl fmt::Display for QueryFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => write!(f, "does not compile: {error}"),
            Self::UnknownCapture(name) => {
                write!(f, "captures `@{name}`, which the resolver does not read")
            }
            Self::NotRooted { pattern } => {
                write!(f, "pattern {pattern} has more than one root node")
            }
            Self::UnknownPredicate { pattern, operator } => write!(
                f,
                "pattern {pattern} uses `#{operator}`, which the resolver does not evaluate"
            ),
            Self::UnknownProperty { pattern, key } => write!(
                f,
                "pattern {pattern} sets `{key}`, which the resolver does not read there"
            ),
            Self::BadValue { pattern, key } => write!(
                f,
                "pattern {pattern} sets `{key}` to a value the resolver does not read"
            ),
            Self::NotADefinition { pattern } => write!(
                f,
                "pattern {pattern} does not capture `@definition` and `@name` once each"
            ),
            Self::UnknownNodeKind { pattern, name } => write!(
                f,
                "pattern {pattern} names `{name}`, which is no kind of a node of the grammar's trees"
            ),
            Self::UnknownContainer { pattern, name } => write!(
                f,
                "pattern {pattern} names the container `{name}`, which no pattern defines"
            ),
        }
    }
}
