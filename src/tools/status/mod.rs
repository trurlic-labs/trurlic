//! The `status` tool: the graph at a glance, as counts and sections, each
//! section fitted to the result budget. A section that leaves items out
//! says how many and where the rest is; `section` gives one section the
//! whole budget.
//!
//! It reports what the graph's files and, under `shared`, the graph's own
//! history hold. Where the graph sits in the project's git is `check`'s:
//! asking it costs several git processes, too many for a call made often.

use crate::store::limits::{MAX_NAME_BYTES, MAX_TRAILER_BYTES};
use crate::store::{
    HistoryFinding, Issue, Level, PROJECT_COMPONENT, ProjectState, Quality, Status, Store,
};

use super::spec::kinds::{Choice, Text};
use super::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use super::spec::{Args, Fields};

mod anchors;
mod fit;
mod lint;
mod report;

pub(crate) use anchors::AnchorCounts;
use anchors::anchor_section;
use fit::{Items, Listed, fill, lint_frame};
pub(crate) use lint::{Cut, LintItem, PAIRS_MORE, PairCounts, PatternCounts};
use lint::{LINT_MORE, Patterns};
use report::{ComponentItem, Counts, HistoryItem, HistoryKind, IssueItem};
pub(crate) use report::{Part, QualityCounts, Report};

pub(crate) static STATUS: ToolSpec<StatusArgs, Report> = ToolSpec {
    name: "status",
    title: "Status",
    description: DESCRIPTION,
    destructive: Destructive::No,
    access: Access::Read(status),
};

const DESCRIPTION: &str = "The decision graph at a glance: its counts, the write rules \
`[quality]` in project.toml adds to the built-in ones (counted), the validation issues \
it holds, what a shared graph's history lacks, the decisions of each component, and \
`lint`: each rule of the quality bar a stored decision breaks, the decisions whose anchors \
all name a file and no symbol, and the best pairs of likely duplicate decisions (a TF-IDF \
cosine of choice and reason of 0.5 or more), counted by kind. The call judges the bar's \
limits and compares no pair. `section: \"lint\"` compares the pairs too, by decision name \
within a bound on the work: past it `counts.lint.pairs` says how many decisions it \
compared, and `origin` narrows it. It also judges the bar's patterns, a `warn` pattern's \
included, over the decisions by name within a bound on the bytes they read: \
`counts.lint.patterns` says how many decisions they read and names in `next` the first \
left unread, and the same call with `from` set to it reads the quality findings on from \
there, comparing no pair. Each section fits the result budget; one that leaves items out gives \
`omitted` and `more`, which says where the rest is. `section` returns one section \
with the whole budget. `origin` narrows the counts and the findings to the decisions \
recorded under it. `section: \"anchors\"` alone reads the code tree: the anchors that do \
not resolve here (unresolved: stale or pending, which `trurlic check` tells apart), are \
malformed, ambiguous, of a file no resolver reads (unverified), or are one name, not the \
whole path of what it names, that ten or more files define (weak). \
Where the graph sits in the project's git is reported by `trurlic check`.";

/// A part of the report that can be asked for alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Issues,
    History,
    Components,
    Lint,
    Anchors,
}

impl Section {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::History => "history",
            Self::Components => "components",
            Self::Lint => "lint",
            Self::Anchors => "anchors",
        }
    }

    /// Where a section's omitted items are, every one of them; for the
    /// ranked findings of `lint`, where they rank and how they come into
    /// view, since no surface lists them all.
    const fn more(self) -> &'static str {
        match self {
            Self::Issues => "`trurlic check` lists every issue",
            Self::History => "`git status` in the graph directory lists every file",
            Self::Components => "the files under .trurlic/components/ and .trurlic/decisions/",
            Self::Lint => LINT_MORE,
            Self::Anchors => "`trurlic check` lists every finding",
        }
    }
}

const SECTIONS: [(&str, Section); 5] = [
    (Section::Issues.as_str(), Section::Issues),
    (Section::History.as_str(), Section::History),
    (Section::Components.as_str(), Section::Components),
    (Section::Lint.as_str(), Section::Lint),
    (Section::Anchors.as_str(), Section::Anchors),
];

/// The names `section` takes, in the order the schema lists them.
pub(crate) fn section_names() -> impl Iterator<Item = &'static str> {
    SECTIONS.iter().map(|&(name, _)| name)
}

/// The arguments of `status`, and the flags of `trurlic status`.
#[derive(Debug)]
pub(crate) struct StatusArgs {
    section: Option<Section>,
    origin: Option<String>,
    from: Option<String>,
}

impl Args for StatusArgs {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        let section = Choice { values: &SECTIONS };
        Self {
            section: fields.optional("section", "The one section to return.", &section),
            origin: fields.optional(
                "origin",
                "Count only the decisions recorded under this origin.",
                &Text {
                    max_bytes: MAX_TRAILER_BYTES,
                },
            ),
            from: fields.optional(
                "from",
                "With `section: \"lint\"`: the quality findings of the decisions named this \
                 or after, the `[quality]` patterns reading on from here: a previous call's \
                 `counts.lint.patterns.next`. A call with it compares no likely duplicate pair.",
                &Text {
                    max_bytes: MAX_NAME_BYTES,
                },
            ),
        }
    }
}

fn status(store: &Store, state: &ProjectState, arguments: StatusArgs) -> Outcome<Report> {
    let StatusArgs {
        section,
        origin,
        from,
    } = arguments;
    let patterns = Patterns::of(section, from.as_deref())?;
    let shown = |shown| section.is_none_or(|asked| asked == shown);
    let origin = origin.as_deref();
    let issues = state.validate();
    let issue_count = issues.len();
    let issue_items =
        shown(Section::Issues).then(|| Listed::all(issues.into_iter().map(issue_item)));
    let history_items = shown(Section::History)
        .then(|| store.history_findings(state.project.placement))
        .transpose()?
        .map(|findings| Listed::all(findings.iter().map(history_item)));
    let component_items =
        shown(Section::Components).then(|| Listed::all(component_items(state, origin)));
    let mut counts = counts(state, origin, issue_count);
    let lint_items = shown(Section::Lint).then(|| {
        let gathered = lint::gather(state, origin, patterns);
        counts.lint = Some(gathered.counts);
        gathered.items
    });
    // Asked for alone: it reads every source file of the tree.
    let anchor_items = (section == Some(Section::Anchors)).then(|| {
        let (items, anchor_counts) = anchor_section(store, state, origin);
        counts.anchors = Some(anchor_counts);
        Listed::all(items)
    });
    let items = Items {
        issues: issue_items,
        history: history_items,
        components: component_items,
        lint: lint_items,
        anchors: anchor_items,
    };
    let mut report = frame(store, state, counts, &items);
    fill(&mut report, items);
    Ok(report)
}

/// The report of `items` with no item shown: what [`fill`] measures.
fn frame(store: &Store, state: &ProjectState, counts: Counts, items: &Items<'_>) -> Report {
    let local = store.is_local();
    Report {
        // clone: the report outlives the state it reads.
        project: state.project.name.clone(),
        placement: state.project.placement,
        graph: (!local).then(|| store.root().display().to_string()),
        code_root: (!local).then(|| store.code_root().display().to_string()),
        counts,
        quality: quality_counts(&state.project.quality),
        issues: items.issues.as_ref().map(Listed::frame),
        history: items.history.as_ref().map(Listed::frame),
        components: items.components.as_ref().map(Listed::frame),
        lint: items.lint.as_ref().map(|lint| lint_frame(&lint.kinds)),
        anchors: items.anchors.as_ref().map(Listed::frame),
    }
}

fn counts(state: &ProjectState, origin: Option<&str>, issues: usize) -> Counts {
    let decisions: Vec<_> = state
        .decisions
        .values()
        .filter(|decision| origin.is_none_or(|origin| decision.origin.as_deref() == Some(origin)))
        .collect();
    let edges = state
        .components
        .values()
        .map(|component| component.connects_to.len())
        .chain(
            state
                .decisions
                .values()
                .map(|decision| decision.depends_on.len()),
        )
        .sum();
    Counts {
        components: state.components.len(),
        decisions: decisions.len(),
        project_wide: decisions
            .iter()
            .filter(|decision| decision.component == PROJECT_COMPONENT)
            .count(),
        proposed: decisions
            .iter()
            .filter(|decision| matches!(decision.status, Status::Proposed { .. }))
            .count(),
        edges,
        issues,
        non_canonical: state.non_canonical.len(),
        lint: None,
        anchors: None,
    }
}

fn quality_counts(quality: &Quality) -> QualityCounts {
    let limits = [
        quality.reason_words.min,
        quality.reason_words.max,
        quality.alternatives_max,
        quality.alternative_words_max,
        quality.anchors_max,
    ]
    .iter()
    .filter(|limit| limit.is_some())
    .count()
        + usize::from(quality.require_origin);
    let at = |level| {
        quality
            .patterns
            .iter()
            .filter(|pattern| pattern.level == level)
            .count()
    };
    QualityCounts {
        limits,
        refusing: at(Level::Refuse),
        warning: at(Level::Warn),
    }
}

fn issue_item(issue: Issue) -> IssueItem {
    IssueItem {
        kind: issue.kind,
        message: issue.message,
    }
}

fn history_item(finding: &HistoryFinding) -> HistoryItem {
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

/// The project-wide decisions, then each component's, by name; under an
/// origin, only the components with a decision recorded under it.
fn component_items(state: &ProjectState, origin: Option<&str>) -> Vec<ComponentItem> {
    let names =
        std::iter::once(PROJECT_COMPONENT).chain(state.components.keys().map(String::as_str));
    names
        .filter_map(|name| {
            let decisions: Vec<_> = state
                .decisions_of(name)
                .into_iter()
                .filter(|(_, decision)| {
                    origin.is_none_or(|origin| decision.origin.as_deref() == Some(origin))
                })
                .collect();
            let proposed = decisions
                .iter()
                .filter(|(_, decision)| matches!(decision.status, Status::Proposed { .. }))
                .count();
            let empty = decisions.is_empty() && (origin.is_some() || name == PROJECT_COMPONENT);
            (!empty).then(|| ComponentItem {
                name: name.to_owned(),
                decisions: decisions.len(),
                proposed,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{empty_state, sample_component, sample_decision};

    /// The limits set, `require_origin` among them, and the patterns by
    /// level.
    #[test]
    fn the_quality_counts_follow_the_table() {
        use crate::store::{DecisionField, Pattern};

        let pattern = |level| Pattern {
            pattern: "x".into(),
            fields: [DecisionField::Choice].into(),
            level,
            message: "M".into(),
        };
        let mut quality = Quality::default();
        let none = quality_counts(&quality);
        assert_eq!((none.limits, none.refusing, none.warning), (0, 0, 0));
        quality.reason_words.max = Some(9);
        quality.anchors_max = Some(8);
        quality.require_origin = true;
        quality.patterns = vec![
            pattern(Level::Warn),
            pattern(Level::Refuse),
            pattern(Level::Warn),
        ];
        let some = quality_counts(&quality);
        assert_eq!((some.limits, some.refusing, some.warning), (3, 1, 2));
    }

    /// Without an origin every component has a row, the project's when it
    /// has decisions; under one, only those with a decision recorded there.
    #[test]
    fn the_component_rows_follow_the_origin() {
        let mut state = empty_state();
        for name in ["auth", "billing"] {
            state.components.insert(name.into(), sample_component());
        }
        let mut recorded = sample_decision("auth");
        recorded.origin = Some("row 1".into());
        recorded.status = Status::Proposed {
            accepted_text: None,
        };
        state.decisions.insert("sign".into(), recorded);
        state
            .decisions
            .insert("rotate".into(), sample_decision("auth"));
        let rows = |origin| -> Vec<(String, usize, usize)> {
            component_items(&state, origin)
                .into_iter()
                .map(|row| (row.name, row.decisions, row.proposed))
                .collect()
        };

        assert_eq!(
            rows(None),
            [("auth".into(), 2, 1), ("billing".into(), 0, 0)]
        );
        assert_eq!(rows(Some("row 1")), [("auth".into(), 1, 1)]);
        let counts = counts(&state, Some("row 1"), 0);
        assert_eq!((counts.decisions, counts.proposed), (1, 1));
    }
}
