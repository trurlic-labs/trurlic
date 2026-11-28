//! A search of the code tree as `map` shows it: the query, parsed once,
//! and the hits from a position on in the one rank `code` gives them, each
//! with its address, kind, name and match. Read from the walk of the
//! calling tree alone; no decision is searched.

use std::fmt;

use crate::code::{self, CodeTree, Found};
use crate::store::Store;

use super::level::{ChildKind, walked};

/// The fewest bytes a query holds.
pub(crate) const MIN_QUERY_BYTES: usize = 1;

/// The most bytes a query holds.
pub(crate) const MAX_QUERY_BYTES: usize = 256;

/// A search's text: [`MIN_QUERY_BYTES`] to [`MAX_QUERY_BYTES`] bytes with
/// no control character. `sources` owns it, as both `tools`, whose
/// argument parses it, and the search here name it; the search in `code`
/// takes its text, any text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Query(String);

/// The value `tools::spec`'s `Fields::required` gives a required argument
/// where it parses none, which no search reads: in the walk that writes
/// the schema, and where the argument is absent or its own parse faults,
/// the arguments then being refused. A query of one byte, as a valid one
/// may be.
impl Default for Query {
    fn default() -> Self {
        Self("?".to_owned())
    }
}

/// Why a text is no query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryFault {
    Empty,
    TooLong {
        bytes: usize,
        max: usize,
    },
    /// A control character at this byte.
    Control {
        at: usize,
    },
}

impl fmt::Display for QueryFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("is empty: a search needs a text to match"),
            Self::TooLong { bytes, max } => {
                write!(f, "is {bytes} bytes, over the {max} a query may hold")
            }
            Self::Control { at } => write!(f, "holds a control character at byte {at}"),
        }
    }
}

impl Query {
    /// `text` as a query.
    pub(crate) fn parse(text: &str) -> Result<Self, QueryFault> {
        if text.len() < MIN_QUERY_BYTES {
            return Err(QueryFault::Empty);
        }
        if text.len() > MAX_QUERY_BYTES {
            return Err(QueryFault::TooLong {
                bytes: text.len(),
                max: MAX_QUERY_BYTES,
            });
        }
        if let Some((at, _)) = text.char_indices().find(|(_, c)| c.is_control()) {
            return Err(QueryFault::Control { at });
        }
        Ok(Self(text.to_owned()))
    }
}

/// The hits of a search a page reads: where they start and how many at
/// most.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) from: usize,
    pub(crate) most: usize,
}

/// A search's hits in a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Search {
    /// Every hit of the query, in and out of the window.
    pub(crate) total: usize,
    /// In rank, from the window's start.
    pub(crate) hits: Vec<Hit>,
}

/// A node or a definition whose name or path holds a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hit {
    /// A node's alone: its address.
    pub(crate) address: Option<String>,
    pub(crate) kind: HitKind,
    /// A node's name ([`CodeTree::name`]), a definition's last segment.
    pub(crate) name: String,
    pub(crate) matched: Match,
    /// A definition's alone.
    pub(crate) definition: Option<HitDefinition>,
}

/// What a hit is: a node of a kind, as a level names it, or a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HitKind {
    Node(ChildKind),
    Definition,
}

impl HitKind {
    pub(crate) const ALL: [Self; 6] = [
        Self::Node(ChildKind::Crate),
        Self::Node(ChildKind::OutsideCrates),
        Self::Node(ChildKind::Module),
        Self::Node(ChildKind::Directory),
        Self::Node(ChildKind::File),
        Self::Definition,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Node(kind) => kind.as_str(),
            Self::Definition => "definition",
        }
    }
}

/// How a hit matched its query ([`code::Match`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Match {
    ExactName,
    NamePrefix,
    PathSubstring,
}

impl Match {
    pub(crate) const ALL: [Self; 3] = [Self::ExactName, Self::NamePrefix, Self::PathSubstring];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ExactName => "exact_name",
            Self::NamePrefix => "name_prefix",
            Self::PathSubstring => "path_substring",
        }
    }

    const fn of(matched: code::Match) -> Self {
        match matched {
            code::Match::ExactName => Self::ExactName,
            code::Match::NamePrefix => Self::NamePrefix,
            code::Match::PathSubstring => Self::PathSubstring,
        }
    }
}

/// What a definition's hit has of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HitDefinition {
    /// Its file's address.
    pub(crate) file: String,
    /// One of [`DEFINITION_KINDS`](super::DEFINITION_KINDS).
    pub(crate) kind: &'static str,
    /// Its path's segments, outermost first.
    pub(crate) path: Vec<String>,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
}

/// The hits of `query` in `window` of the tree at `store`'s code root.
/// It reads every source file the walk takes, and fills the index's
/// cache.
pub(crate) fn search(store: &Store, query: &Query, window: Window) -> Search {
    let facts = walked(store);
    search_in(&CodeTree::of(&facts), query, window)
}

/// [`search`] in `tree`.
pub(crate) fn search_in(tree: &CodeTree<'_>, query: &Query, window: Window) -> Search {
    let found = tree.search(&query.0);
    let total = found.len();
    let hits = found.into_iter().skip(window.from).take(window.most);
    Search {
        total,
        hits: hits.map(|hit| owned(tree, hit)).collect(),
    }
}

/// `hit`, owned and named as `map` shows it.
fn owned(tree: &CodeTree<'_>, hit: code::Hit<'_>) -> Hit {
    let matched = Match::of(hit.matched);
    match hit.found {
        Found::Node(node) => Hit {
            address: Some(tree.address(node)),
            kind: HitKind::Node(ChildKind::of(node)),
            name: tree.name(node).to_owned(),
            matched,
            definition: None,
        },
        Found::Definition(definition) => Hit {
            address: None,
            kind: HitKind::Definition,
            name: definition.name().to_owned(),
            matched,
            definition: Some(HitDefinition {
                file: tree.address(definition.file),
                kind: definition.kind.name(),
                path: definition.segments(),
                first_line: definition.first_line,
                last_line: definition.last_line,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_empty_too_long_or_holding_a_control_character_is_no_query() {
        let long = "a".repeat(MAX_QUERY_BYTES + 1);

        let refused = ["", long.as_str(), "ab\tc"].map(Query::parse);

        assert_eq!(
            refused.map(Result::err),
            [
                Some(QueryFault::Empty),
                Some(QueryFault::TooLong {
                    bytes: MAX_QUERY_BYTES + 1,
                    max: MAX_QUERY_BYTES
                }),
                Some(QueryFault::Control { at: 2 }),
            ]
        );
        assert!(Query::parse(&"a".repeat(MAX_QUERY_BYTES)).is_ok());
    }
}
