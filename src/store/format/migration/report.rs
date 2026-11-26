//! What a migration reports: per step, the values it dropped and the facts
//! the owner must be able to read after the fact.

use std::fmt;

use crate::store::model::FormatVersion;
use crate::store::model::Placement;

/// A value a migration dropped: `field` is its dotted key path in `file`,
/// a path under `.trurlic/`, and `value` its TOML text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Loss {
    pub(crate) file: String,
    pub(crate) field: String,
    pub(crate) value: String,
}

/// What one step dropped, and what else it reports.
#[derive(Debug)]
pub(crate) struct StepReport {
    pub(crate) from: FormatVersion,
    pub(crate) to: FormatVersion,
    /// The format change, in a few words.
    pub(crate) change: &'static str,
    pub(crate) losses: Vec<Loss>,
    /// Sorted, so each kind of fact is one run.
    pub(crate) facts: Vec<Fact>,
}

/// What a step reports besides its losses: what it rewrote, decided or took
/// out of the graph. The variants are in the order a report shows them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Fact {
    /// The placement the step wrote to `project.toml`, and how it was told.
    Placement {
        placement: Placement,
        basis: PlacementBasis,
    },
    /// A node file whose `name` differs from its stem, which is the name now.
    Renamed { file: String, name: String },
    /// `to depends_on from`, written for the edge `from constrains to`.
    Reversed { from: String, to: String },
    /// An anchor of the decision `file` rewritten to its 0.5 spelling.
    Normalized {
        file: String,
        old: RefText,
        new: RefText,
    },
    /// An anchor of the decision `file` kept as written, which 0.5 may
    /// read as another name; `why` says what the spelling leaves open.
    Unnormalized {
        file: String,
        anchor: RefText,
        why: &'static str,
    },
    /// The history entries of the decision `file`, dropped.
    History { file: String, entries: usize },
    /// A pattern, taken out of the graph whole.
    Pattern(PatternRecord),
    /// The tags of the component `node`, taken out of the graph.
    Tags { node: String, tags: Vec<String> },
}

/// How the step to format 0.5 was told the placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PlacementBasis {
    /// The owner stated it.
    Stated,
    /// Git tracks every one of the store's `files`.
    AllTracked { files: usize },
    /// Git tracks none of the store's `files`.
    NoneTracked { files: usize },
}

/// A code ref as format 0.4.1 or an anchor as 0.5 wrote it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RefText {
    pub(in crate::store) file: String,
    pub(in crate::store) symbol: Option<String>,
}

impl RefText {
    pub(in crate::store) fn to_value(&self) -> toml::Value {
        let mut fields = toml::Table::new();
        fields.insert("file".into(), self.file.as_str().into());
        if let Some(symbol) = &self.symbol {
            fields.insert("symbol".into(), symbol.as_str().into());
        }
        toml::Value::Table(fields)
    }
}

impl fmt::Display for RefText {
    /// An inline TOML table, as the 0.5 file writes it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_value())
    }
}

/// A pattern of format 0.4.1: its stem, the fields of its file, and the
/// edges and tags `graph.toml` gave it. Each list is sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PatternRecord {
    pub(crate) name: String,
    /// The `name` field: the pattern's title.
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) tags: Vec<String>,
    /// The decisions of the pattern.
    pub(crate) member_of: Vec<String>,
    /// The components the pattern applies to.
    pub(crate) applies_to: Vec<String>,
}

/// What a migration does to the store, in the order it happens.
#[derive(Debug)]
pub(crate) struct MigrationReport {
    pub(crate) from: FormatVersion,
    pub(crate) steps: Vec<StepReport>,
    /// The files the commit writes, by path under `.trurlic/`, in path order.
    pub(crate) rewritten: Vec<String>,
    /// The files the commit removes, by path under `.trurlic/`.
    pub(crate) removed: Vec<String>,
}
