//! The `status` tool: its spec, its arguments, and the handler that draws
//! each section asked for from the graph and fits the report to the
//! budget.

use crate::store::limits::{MAX_NAME_BYTES, MAX_TRAILER_BYTES};
use crate::store::{ProjectState, Store};
use crate::tools::spec::kinds::{Choice, Text};
use crate::tools::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use crate::tools::spec::{Args, Fields};

use super::anchors::anchor_section;
use super::components::component_items;
use super::counts::counts;
use super::fit::{Items, fill, frame};
use super::history::history_item;
use super::issues::issue_item;
use super::lint::{self, Patterns};
use super::part::Listed;
use super::report::Report;
use super::section::{SECTIONS, Section};

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

/// The arguments of `status`, and the flags of `trurlic status`.
#[derive(Debug)]
pub(crate) struct StatusArgs {
    pub(super) section: Option<Section>,
    pub(super) origin: Option<String>,
    pub(super) from: Option<String>,
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

pub(super) fn status(
    store: &Store,
    state: &ProjectState,
    arguments: StatusArgs,
) -> Outcome<Report> {
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
