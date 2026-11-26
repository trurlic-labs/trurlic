//! The `history` section of `status`: what a shared graph's own history
//! lacks, each finding by its kind; empty under `tracked`.

use crate::store::HistoryFinding;
use crate::tools::spec::output::{Str, Tag};
use crate::tools::spec::{Output, Shown};

pub(crate) struct HistoryItem {
    kind: HistoryKind,
    pub(crate) message: String,
}

/// The kind of a history finding, one per `HistoryFinding` variant.
#[derive(Debug, Clone, Copy)]
enum HistoryKind {
    Missing,
    IndexLocked,
    Uncommitted,
}

impl HistoryKind {
    const ALL: [Self; 3] = [Self::Missing, Self::IndexLocked, Self::Uncommitted];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::IndexLocked => "index_locked",
            Self::Uncommitted => "uncommitted",
        }
    }
}

pub(super) fn history_item(finding: &HistoryFinding) -> HistoryItem {
    let kind = match finding {
        HistoryFinding::Missing { .. } => HistoryKind::Missing,
        HistoryFinding::IndexLocked { .. } => HistoryKind::IndexLocked,
        HistoryFinding::Uncommitted { .. } => HistoryKind::Uncommitted,
    };
    HistoryItem {
        kind,
        message: finding.to_string(),
    }
}

impl Output for HistoryItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        let kind = Tag {
            all: &HistoryKind::ALL,
            name: HistoryKind::as_str,
        };
        fields.required("kind", "The finding's kind.", &kind, |item| &item.kind);
        fields.required(
            "message",
            "What is lacking and what mends it.",
            &Str,
            |item| &item.message,
        );
    }
}
