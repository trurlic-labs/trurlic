//! A language's query as the general resolver reads it ([`Rules`]): what
//! each capture is, how each definition pattern makes a definition, and
//! why a query file breaks the contract ([`QueryFault`]).

use std::fmt;

use tree_sitter::{Language, Query, QueryError};

use crate::code::index::{DefinitionKind, Visibility};

/// What a capture of a query is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::code::parse) enum Capture {
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
pub(in crate::code::parse) enum Container {
    Top,
    Kind(DefinitionKind),
    Unnamed,
}

/// How a definition pattern makes a definition of what it captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::code::parse) struct Definer {
    pub(super) kind: DefinitionKind,
    /// Its kind in a container of a kind, by the container's.
    pub(super) kind_in: Vec<(DefinitionKind, DefinitionKind)>,
    /// What its span holds is under it.
    pub(in crate::code::parse) container: bool,
    /// The containers it is kept in; `None` for every one.
    pub(super) within: Option<Vec<Container>>,
    /// The growth of its span, by its place in [`Rules::growths`].
    pub(in crate::code::parse) growth: Option<usize>,
    /// The wrapping of its span, by its place in [`Rules::wraps`].
    pub(in crate::code::parse) wrap: Option<usize>,
    /// The siblings its span takes in, by their place in
    /// [`Rules::precedings`].
    pub(in crate::code::parse) preceding: Option<usize>,
    /// A match of it drops the matches of patterns without it that
    /// captured its `@definition` node.
    pub(in crate::code::parse) exclusive: bool,
    /// The text of a leaf of its name that separates two segments.
    pub(in crate::code::parse) separator: Option<String>,
    /// The node kinds whose leaves its name leaves out.
    pub(in crate::code::parse) skip: Vec<u16>,
    /// The text of its one segment, in place of its name node's.
    pub(in crate::code::parse) text: Option<String>,
    pub(in crate::code::parse) visibility: Option<Visibility>,
}

impl Definer {
    /// Whether it is kept in `container`.
    pub(in crate::code::parse) fn admits(&self, container: Container) -> bool {
        self.within
            .as_ref()
            .is_none_or(|within| within.contains(&container))
    }

    /// Its kind in `container`.
    pub(in crate::code::parse) fn kind_in(&self, container: Container) -> DefinitionKind {
        self.kind_in
            .iter()
            .find(|(outer, _)| Container::Kind(*outer) == container)
            .map_or(self.kind, |(_, kind)| *kind)
    }
}

/// A span's growth: from the captured node up through ancestors of the
/// `via` kinds to the first of a `to` kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::code::parse) struct Growth {
    pub(in crate::code::parse) via: Vec<u16>,
    pub(in crate::code::parse) to: Vec<u16>,
}

/// A language's query, compiled and read by its contract.
#[derive(Debug)]
pub(in crate::code) struct Rules {
    pub(in crate::code::parse) grammar: Language,
    pub(in crate::code::parse) query: Query,
    /// By capture index.
    pub(in crate::code::parse) captures: Vec<Capture>,
    /// By pattern index: the definer of a definition pattern.
    pub(in crate::code::parse) patterns: Vec<Option<Definer>>,
    /// The distinct growths of the patterns.
    pub(in crate::code::parse) growths: Vec<Growth>,
    /// The distinct sets of kinds a span wraps over.
    pub(in crate::code::parse) wraps: Vec<Vec<u16>>,
    /// The distinct sets of kinds of the siblings a span takes in.
    pub(in crate::code::parse) precedings: Vec<Vec<u16>>,
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
