//! The `status` tool: the graph at a glance, as counts and sections, each
//! section fitted to the result budget. A section that leaves items out
//! says how many and where the rest is; `section` gives one section the
//! whole budget.
//!
//! It reports what the graph's files and, under `shared`, the graph's own
//! history hold. Where the graph sits in the project's git is `check`'s:
//! asking it costs several git processes, too many for a call made often.

use crate::budget::{self, MAX_TOOL_RESULT_BYTES, Shrinkable};
use crate::lint::duplicates::{Score, Similarity};
use crate::store::limits::MAX_TRAILER_BYTES;
use crate::store::{
    HistoryFinding, Issue, Level, PROJECT_COMPONENT, ProjectState, Quality, Status, Store,
};

use super::spec::kinds::{Choice, Text};
use super::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use super::spec::{Args, Fields, Output, to_value};
use super::truncate::json_len;

mod report;

use report::{ComponentItem, Counts, HistoryItem, HistoryKind, IssueItem};
pub(crate) use report::{LintItem, Part, QualityCounts, Report};

pub(super) static STATUS: ToolSpec<StatusArgs, Report> = ToolSpec {
    name: "status",
    title: "Status",
    description: DESCRIPTION,
    destructive: Destructive::No,
    access: Access::Read(status),
};

const DESCRIPTION: &str = "The decision graph at a glance: its counts, the write rules \
`[quality]` in project.toml adds to the built-in ones (counted), the validation issues \
it holds, what a shared graph's history lacks, the decisions of each component, and \
`lint`: the best pairs of likely duplicate decisions, whose choice and reason have a TF-IDF \
cosine of 0.5 or more, best first. Each section fits the result budget; one that leaves items out gives \
`omitted` and `more`, which says where the rest is. `section` returns one section \
with the whole budget. `origin` narrows the counts to the decisions recorded under it. \
Where the graph sits in the project's git is reported by `trurlic check`.";

/// The bytes a report keeps, beyond its frame, for the `truncated`
/// notice of a cut, which no report needs unless its frame alone, its
/// paths, fills the budget.
const NOTICE_BYTES: usize = 64;

/// A part of the report that can be asked for alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Issues,
    History,
    Components,
    Lint,
}

impl Section {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::History => "history",
            Self::Components => "components",
            Self::Lint => "lint",
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
        }
    }
}

/// What the `lint` section's omitted findings are, in the words every
/// surface uses of them.
pub(crate) const LINT_MORE: &str = "the section lists the best pairs; the ones left out rank \
     at or below the last listed, and come into view as the listed ones are settled (merged, \
     or revised apart)";

const SECTIONS: [(&str, Section); 4] = [
    (Section::Issues.as_str(), Section::Issues),
    (Section::History.as_str(), Section::History),
    (Section::Components.as_str(), Section::Components),
    (Section::Lint.as_str(), Section::Lint),
];

/// The names `section` takes, in the order the schema lists them.
pub(crate) fn section_names() -> impl Iterator<Item = &'static str> {
    SECTIONS.iter().map(|&(name, _)| name)
}

/// The arguments of `status`, and the flags of `trurlic status`.
#[derive(Debug)]
pub(super) struct StatusArgs {
    section: Option<Section>,
    origin: Option<String>,
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
        }
    }
}

fn status(store: &Store, state: &ProjectState, arguments: StatusArgs) -> Outcome<Report> {
    let StatusArgs { section, origin } = arguments;
    let shown = |shown| section.is_none_or(|asked| asked == shown);
    let origin = origin.as_deref();
    let issues = state.validate();
    let issue_count = issues.len();
    let issue_items =
        shown(Section::Issues).then(|| Listed::all(issues.into_iter().map(issue_item)));
    let history_items = if shown(Section::History) {
        let findings = store.history_findings(state.project.placement)?;
        Some(Listed::all(findings.iter().map(history_item)))
    } else {
        None
    };
    let component_items =
        shown(Section::Components).then(|| Listed::all(component_items(state, origin)));
    let lint_items = shown(Section::Lint).then(|| lint_items(state));
    let local = store.is_local();
    let mut report = Report {
        // clone: the report outlives the state it reads.
        project: state.project.name.clone(),
        placement: state.project.placement,
        graph: (!local).then(|| store.root().display().to_string()),
        code_root: (!local).then(|| store.code_root().display().to_string()),
        counts: counts(state, origin, issue_count),
        quality: quality_counts(&state.project.quality),
        issues: issue_items.as_ref().map(Listed::frame),
        history: history_items.as_ref().map(Listed::frame),
        components: component_items.as_ref().map(Listed::frame),
        lint: lint_items.as_ref().map(Listed::frame),
    };
    let items = Items {
        issues: issue_items,
        history: history_items,
        components: component_items,
        lint: lint_items,
    };
    fill(&mut report, items);
    Ok(report)
}

/// A section's items before the budget cuts them, out of `total`: every
/// item but a ranked section's, which holds only the best a result can.
struct Listed<T> {
    total: usize,
    items: Vec<T>,
}

impl<T> Listed<T> {
    fn all(items: impl IntoIterator<Item = T>) -> Self {
        let items: Vec<T> = items.into_iter().collect();
        Self {
            total: items.len(),
            items,
        }
    }

    /// The section with none of its items shown: what a report measures
    /// before it is filled.
    const fn frame(&self) -> Part<T> {
        Part {
            total: self.total,
            items: Vec::new(),
            omitted: 0,
            more: None,
        }
    }
}

/// The items of each section asked for, before the budget cuts them.
struct Items {
    issues: Option<Listed<IssueItem>>,
    history: Option<Listed<HistoryItem>>,
    components: Option<Listed<ComponentItem>>,
    lint: Option<Listed<LintItem>>,
}

/// Put into `report`, whose sections hold no item yet, each section's
/// leading items that fit the budget beside the rest of it.
fn fill(report: &mut Report, items: Items) {
    let Items {
        issues,
        history,
        components,
        lint,
    } = items;
    let sizes: Vec<Sizes> = [
        issues
            .as_ref()
            .map(|listed| Sizes::of(Section::Issues, listed)),
        history
            .as_ref()
            .map(|listed| Sizes::of(Section::History, listed)),
        components
            .as_ref()
            .map(|listed| Sizes::of(Section::Components, listed)),
        lint.as_ref().map(|listed| Sizes::of(Section::Lint, listed)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let fixed = json_len(&to_value(&*report)) + NOTICE_BYTES;
    let level = budget::level(&sizes, fixed, MAX_TOOL_RESULT_BYTES).unwrap_or(0);
    report.issues = issues.map(|items| Sizes::part(&sizes, Section::Issues, items, level));
    report.history = history.map(|items| Sizes::part(&sizes, Section::History, items, level));
    report.components =
        components.map(|items| Sizes::part(&sizes, Section::Components, items, level));
    report.lint = lint.map(|items| Sizes::part(&sizes, Section::Lint, items, level));
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

/// The pairs of likely duplicates, best first.
fn lint_items(state: &ProjectState) -> Listed<LintItem> {
    let ranked = Similarity::of(state).pairs(lint_limit());
    let items = ranked.best.into_iter().map(|pair| LintItem::Duplicate {
        decision: pair.first.to_owned(),
        other: pair.second.to_owned(),
        score: pair.score.to_string(),
    });
    Listed {
        total: ranked.total,
        items: items.collect(),
    }
}

/// The most lint findings one result can hold: the budget over the
/// smallest finding there can be, with the comma before it, so the
/// selection keeps every finding a result can show and no more.
fn lint_limit() -> usize {
    let smallest = LintItem::Duplicate {
        decision: "a".into(),
        other: "b".into(),
        score: Score::THRESHOLD.to_string(),
    };
    MAX_TOOL_RESULT_BYTES / json_len(&to_value(&smallest)).saturating_add(1)
}

// -- Fitting -----------------------------------------------------------------

/// The sizes of a section's items as JSON: what [`budget::level`] cuts.
struct Sizes {
    section: Section,
    /// The bytes of the first `k + 1` items with the commas between them.
    ends: Vec<usize>,
    /// Whether the section has items beyond those given, so that even
    /// whole it carries the note of what it leaves out.
    beyond: bool,
}

impl Sizes {
    fn of<T: Output>(section: Section, listed: &Listed<T>) -> Self {
        let mut ends = Vec::with_capacity(listed.items.len());
        let mut end = 0usize;
        for (index, item) in listed.items.iter().enumerate() {
            end = end
                .saturating_add(usize::from(index > 0))
                .saturating_add(json_len(&to_value(item)));
            ends.push(end);
        }
        Self {
            section,
            ends,
            beyond: listed.total > listed.items.len(),
        }
    }

    /// The items of `section` kept at `level`, with the count of the rest.
    fn part<T>(all: &[Self], section: Section, listed: Listed<T>, level: usize) -> Part<T> {
        let Listed { total, mut items } = listed;
        let limit = all
            .iter()
            .find(|sizes| sizes.section == section)
            .and_then(|sizes| budget::limit(sizes, level).map(|limit| sizes.count_within(limit)));
        if let Some(keep) = limit {
            items.truncate(keep);
        }
        let omitted = total.saturating_sub(items.len());
        Part {
            total,
            items,
            omitted,
            more: (omitted > 0).then(|| section.more()),
        }
    }

    fn count_within(&self, limit: usize) -> usize {
        self.ends.partition_point(|&end| end <= limit)
    }
}

impl Shrinkable for Sizes {
    fn full(&self) -> usize {
        let items = self.ends.last().copied().unwrap_or(0);
        if self.beyond {
            items.saturating_add(self.note())
        } else {
            items
        }
    }

    fn note(&self) -> usize {
        // `,"omitted":N,"more":"..."`, N at most 20 digits.
        41 + self.section.more().len()
    }

    fn kept(&self, limit: usize) -> usize {
        self.count_within(limit)
            .checked_sub(1)
            .and_then(|last| self.ends.get(last))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{empty_state, sample_component, sample_decision};

    fn item(name: &str) -> ComponentItem {
        ComponentItem {
            name: name.into(),
            decisions: 0,
            proposed: 0,
        }
    }

    /// A section keeps the prefix of its items the level allows, and
    /// counts the rest with where to find them.
    #[test]
    fn a_cut_section_keeps_a_prefix_and_counts_the_rest() {
        let items = Listed::all((0..100).map(|i| item(&format!("c{i:02}"))));
        let sizes = [Sizes::of(Section::Components, &items)];
        let whole = sizes[0].full();
        let level = budget::level(&sizes, 0, whole / 2).unwrap();

        let part = Sizes::part(&sizes, Section::Components, items, level);

        assert_eq!(part.total, 100);
        assert!(part.items.len() < 50, "{}", part.items.len());
        assert_eq!(part.omitted, 100 - part.items.len());
        assert_eq!(part.items[0].name, "c00");
        assert_eq!(part.more, Some(Section::Components.more()));
        let one = Listed::all([item("c00")]);
        let fits = Sizes::part(&sizes, Section::Components, one, whole);
        assert_eq!((fits.omitted, fits.more), (0, None));
    }

    /// A ranked section given its best items out of more counts the rest
    /// even when every given item fits, and its size holds that note.
    #[test]
    fn a_ranked_section_counts_what_its_selection_left_out() {
        let best = Listed {
            total: 1000,
            items: vec![item("c00"), item("c01")],
        };
        let sizes = Sizes::of(Section::Lint, &best);
        assert_eq!(sizes.full(), sizes.ends[1] + sizes.note());

        let part = Sizes::part(&[sizes], Section::Lint, best, MAX_TOOL_RESULT_BYTES);

        assert_eq!((part.total, part.items.len(), part.omitted), (1000, 2, 998));
        assert_eq!(part.more, Some(Section::Lint.more()));
    }

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
