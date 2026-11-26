//! The `anchors` section of `status`: the anchor findings of the decisions
//! under the call's origin, judged in the calling tree alone, as items,
//! and their counts by kind.
//!
//! `status` runs no git in the project, so it cannot tell an anchor
//! another worktree holds (pending) from one none does (stale): it names
//! either one unresolved, "does not resolve in this tree", and leaves the
//! verdict, and the planned anchors of decisions never accepted, to
//! `trurlic check`, which runs git. A malformed, ambiguous, unverified
//! or weak anchor needs no git, and is reported as `check` reports it.

use crate::lint::anchors::{Finding, Kind, KindName, Unasked, WEAK_FILES};
use crate::mcp::spec::output::{Count, Nested, Str, Tag};
use crate::mcp::spec::{Output, Shown};
use crate::sources;
use crate::store::{ProjectState, Store};

use super::report::{Part, Report};

/// One finding: its decision, its anchor as `file::symbol`, its kind, and
/// what makes it one.
pub(crate) struct AnchorItem {
    pub(crate) decision: String,
    pub(crate) anchor: String,
    pub(crate) kind: KindName,
    pub(crate) cause: String,
}

/// The findings by kind, every one counted, those a cut leaves out
/// included.
#[derive(Default)]
pub(crate) struct AnchorCounts {
    pub(crate) unresolved: usize,
    pub(crate) malformed: usize,
    pub(crate) ambiguous: usize,
    pub(crate) unverified: usize,
    pub(crate) weak: usize,
}

impl AnchorCounts {
    fn of(findings: &[Finding<'_, Unasked>]) -> Self {
        let mut counts = Self::default();
        for finding in findings {
            let count = match finding.kind {
                Kind::Missing(..) => &mut counts.unresolved,
                Kind::Malformed(_) => &mut counts.malformed,
                Kind::Ambiguous(_) => &mut counts.ambiguous,
                Kind::Unverified(_) => &mut counts.unverified,
                Kind::Weak(_) => &mut counts.weak,
            };
            *count += 1;
        }
        counts
    }
}

/// The anchor findings of the decisions recorded under `origin`, or of
/// every decision, in the order `lint` gives them, with their counts.
pub(super) fn anchor_section(
    store: &Store,
    state: &ProjectState,
    origin: Option<&str>,
) -> (Vec<AnchorItem>, AnchorCounts) {
    let decisions = state.decisions.iter().filter(|(_, decision)| {
        origin.is_none_or(|origin| decision.origin.as_deref() == Some(origin))
    });
    let found = sources::tree_findings(store, decisions);
    let counts = AnchorCounts::of(&found);
    let items = found
        .into_iter()
        .map(|finding| AnchorItem {
            decision: finding.decision.to_owned(),
            anchor: finding.anchor.to_string(),
            kind: finding.kind.name(),
            cause: finding.kind.to_string(),
        })
        .collect();
    (items, counts)
}

/// The report's field this section fills: the findings.
pub(super) fn declare(fields: &mut Shown<'_, Report>) {
    let anchors = "The anchors that do not plainly resolve in this tree, by kind, decision, \
                   anchor.";
    let kind = Nested::<Part<AnchorItem>>::new();
    fields.optional("anchors", anchors, &kind, |report| report.anchors.as_ref());
}

// The schema states the bound in words.
const _: () = assert!(WEAK_FILES == 10, "the `kind` description names 10 files");

impl Output for AnchorItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("decision", "The decision.", &Str, |item| &item.decision);
        let anchor = "The anchor, `file` or `file::symbol`.";
        fields.required("anchor", anchor, &Str, |item| &item.anchor);
        let kind = Tag {
            all: &KindName::UNASKED,
            name: KindName::as_str,
        };
        let about = "`unresolved`: no file at its path or no definition its symbol names \
                     in this tree, on a decision ever accepted; stale, or pending on \
                     another worktree's branch, which only `trurlic check` tells apart; \
                     `malformed`: a symbol out of the grammar or on a directory, which \
                     fails `trurlic check`; `ambiguous`: its symbol names definitions of \
                     several paths, which fails it too; `unverified`: its file is of a kind \
                     no resolver reads or cannot be indexed; `weak`: its symbol is one \
                     name, not the whole path of what it names, that 10 or more files of \
                     its kind define.";
        fields.required("kind", about, &kind, |item| &item.kind);
        let cause = "What makes it one: the cause, or the count.";
        fields.required("cause", cause, &Str, |item| &item.cause);
    }
}

impl Output for AnchorCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let unresolved = "Anchors that do not resolve in this tree.";
        fields.required("unresolved", unresolved, &Count, |c| &c.unresolved);
        fields.required("malformed", "Malformed anchors.", &Count, |c| &c.malformed);
        fields.required("ambiguous", "Ambiguous anchors.", &Count, |c| &c.ambiguous);
        fields.required("unverified", "Unverified anchors.", &Count, |c| {
            &c.unverified
        });
        fields.required("weak", "Weak anchors.", &Count, |c| &c.weak);
    }
}
