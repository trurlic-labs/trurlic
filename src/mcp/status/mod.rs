//! The `status` tool: the graph at a glance, as counts and sections, each
//! section fitted to the result budget. A section that leaves items out
//! says how many and where the rest is; `section` gives one section the
//! whole budget.
//!
//! It reports what the graph's files and, under `shared`, the graph's own
//! history hold. Where the graph sits in the project's git is `check`'s:
//! asking it costs several git processes, too many for a call made often.

use serde_json::Value;

use crate::budget::{self, MAX_TOOL_RESULT_BYTES, Shrinkable};
use crate::store::limits::MAX_TRAILER_BYTES;
use crate::store::{HistoryFinding, Issue, PROJECT_COMPONENT, ProjectState, Status, Store};

use super::spec::kinds::{Choice, Text};
use super::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use super::spec::{Args, Fields, Output, to_value};

mod report;

pub(crate) use report::{ComponentItem, Counts, HistoryItem, IssueItem, Part, Report};

pub(crate) static STATUS: ToolSpec<StatusArgs, Report> = ToolSpec {
    name: "status",
    title: "Status",
    description: DESCRIPTION,
    destructive: Destructive::No,
    access: Access::Read(status),
};

const DESCRIPTION: &str = "The decision graph at a glance: its counts, the validation \
issues it holds, what a shared graph's history lacks, and the decisions of each \
component. Each section fits the result budget; one that leaves items out gives \
`omitted` and `more`, which says where the rest is. `section` returns one section \
with the whole budget. `origin` narrows the counts to the decisions recorded under it. \
Where the graph sits in the project's git is reported by `trurlic check`.";

/// The bytes a report keeps, beyond its frame, for the `truncated`
/// notice of a cut, which no report needs unless its frame alone, its
/// paths, fills the budget.
const NOTICE_BYTES: usize = 64;

/// A part of the report that can be asked for alone.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    #[default]
    Issues,
    History,
    Components,
}

impl Section {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::History => "history",
            Self::Components => "components",
        }
    }

    /// Where a section's omitted items are, every one of them.
    const fn more(self) -> &'static str {
        match self {
            Self::Issues => "`trurlic check` lists every issue",
            Self::History => "`git status` in the graph directory lists every file",
            Self::Components => "the files under .trurlic/components/ and .trurlic/decisions/",
        }
    }
}

const SECTIONS: [(&str, Section); 3] = [
    (Section::Issues.as_str(), Section::Issues),
    (Section::History.as_str(), Section::History),
    (Section::Components.as_str(), Section::Components),
];

/// The arguments of `status`, and the flags of `trurlic status`.
#[derive(Debug, Default)]
pub(crate) struct StatusArgs {
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
    let issue_items = shown(Section::Issues).then(|| issues.iter().map(issue_item).collect());
    let history_items = if shown(Section::History) {
        let findings = store.history_findings(state.project.placement)?;
        Some(findings.iter().map(history_item).collect())
    } else {
        None
    };
    let component_items = shown(Section::Components).then(|| component_items(state, origin));
    let local = store.is_local();
    let mut report = Report {
        // clone: the report outlives the state it reads.
        project: state.project.name.clone(),
        placement: state.project.placement,
        graph: (!local).then(|| store.root().display().to_string()),
        code_root: (!local).then(|| store.code_root().display().to_string()),
        counts: counts(state, origin, issues.len()),
        issues: issue_items.as_deref().map(Part::frame),
        history: history_items.as_deref().map(Part::frame),
        components: component_items.as_deref().map(Part::frame),
    };
    fill(&mut report, issue_items, history_items, component_items);
    Ok(report)
}

/// Put into `report`, whose sections hold no item yet, each section's
/// leading items that fit the budget beside the rest of it.
fn fill(
    report: &mut Report,
    issues: Option<Vec<IssueItem>>,
    history: Option<Vec<HistoryItem>>,
    components: Option<Vec<ComponentItem>>,
) {
    let sizes: Vec<Sizes> = [
        issues
            .as_deref()
            .map(|items| Sizes::of(Section::Issues, items)),
        history
            .as_deref()
            .map(|items| Sizes::of(Section::History, items)),
        components
            .as_deref()
            .map(|items| Sizes::of(Section::Components, items)),
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

fn issue_item(issue: &Issue) -> IssueItem {
    IssueItem {
        kind: issue.kind,
        // clone: the report outlives the validation it reads.
        message: issue.message.clone(),
    }
}

fn history_item(finding: &HistoryFinding) -> HistoryItem {
    let kind = match finding {
        HistoryFinding::Missing { .. } => "missing",
        HistoryFinding::IndexLocked { .. } => "index_locked",
        HistoryFinding::Uncommitted { .. } => "uncommitted",
    };
    HistoryItem {
        kind: kind.to_owned(),
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

// -- Fitting -----------------------------------------------------------------

/// The sizes of a section's items as JSON: what [`budget::level`] cuts.
struct Sizes {
    section: Section,
    /// The bytes of the first `k + 1` items with the commas between them.
    ends: Vec<usize>,
}

impl Sizes {
    fn of<T: Output>(section: Section, items: &[T]) -> Self {
        let mut ends = Vec::with_capacity(items.len());
        let mut end = 0usize;
        for (index, item) in items.iter().enumerate() {
            end = end
                .saturating_add(usize::from(index > 0))
                .saturating_add(json_len(&to_value(item)));
            ends.push(end);
        }
        Self { section, ends }
    }

    /// The items of `section` kept at `level`, with the count of the rest.
    fn part<T>(all: &[Self], section: Section, mut items: Vec<T>, level: usize) -> Part<T> {
        let total = items.len();
        let limit = all
            .iter()
            .find(|sizes| sizes.section == section)
            .and_then(|sizes| budget::limit(sizes, level).map(|limit| sizes.count_within(limit)));
        if let Some(keep) = limit {
            items.truncate(keep);
        }
        let omitted = total - items.len();
        Part {
            total,
            items,
            omitted,
            more: (omitted > 0).then(|| section.more().to_owned()),
        }
    }

    fn count_within(&self, limit: usize) -> usize {
        self.ends.partition_point(|&end| end <= limit)
    }
}

impl Shrinkable for Sizes {
    fn full(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    fn note(&self) -> usize {
        // `,"omitted":N,"more":"..."`, N at most 20 digits.
        40 + self.section.more().len()
    }

    fn kept(&self, limit: usize) -> usize {
        self.count_within(limit)
            .checked_sub(1)
            .and_then(|last| self.ends.get(last))
            .copied()
            .unwrap_or(0)
    }
}

fn json_len(value: &Value) -> usize {
    serde_json::to_string(value).map_or(0, |text| text.len())
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
        let items: Vec<ComponentItem> = (0..100).map(|i| item(&format!("c{i:02}"))).collect();
        let sizes = [Sizes::of(Section::Components, &items)];
        let whole = sizes[0].full();
        let level = budget::level(&sizes, 0, whole / 2).unwrap();

        let part = Sizes::part(&sizes, Section::Components, items, level);

        assert_eq!(part.total, 100);
        assert!(part.items.len() < 50, "{}", part.items.len());
        assert_eq!(part.omitted, 100 - part.items.len());
        assert_eq!(part.items[0].name, "c00");
        assert_eq!(part.more.as_deref(), Some(Section::Components.more()));
        let fits = Sizes::part(&sizes, Section::Components, vec![item("c00")], whole);
        assert_eq!((fits.omitted, fits.more), (0, None));
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
