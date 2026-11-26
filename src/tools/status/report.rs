//! What `status` answers, declared once for its output schema and its
//! JSON: the counts, and each section as the items that fit out of its
//! total.

use crate::store::Placement;
use crate::tools::spec::output::{Nested, Str, Tag};
use crate::tools::spec::{Output, Shown};

use super::anchors::AnchorItem;
use super::components::ComponentItem;
use super::counts::{Counts, QualityCounts};
use super::history::HistoryItem;
use super::issues::IssueItem;
use super::lint::LintItem;
use super::part::Part;

/// What `status` answers.
pub(crate) struct Report {
    pub(crate) project: String,
    pub(crate) placement: Placement,
    /// The graph directory and the code root, when the tree reaches a
    /// graph it does not hold.
    pub(crate) graph: Option<String>,
    pub(crate) code_root: Option<String>,
    pub(crate) counts: Counts,
    pub(crate) quality: QualityCounts,
    pub(crate) issues: Option<Part<IssueItem>>,
    pub(crate) history: Option<Part<HistoryItem>>,
    pub(crate) components: Option<Part<ComponentItem>>,
    pub(crate) lint: Option<Part<LintItem>>,
    pub(crate) anchors: Option<Part<AnchorItem>>,
}

const PLACEMENT: Tag<Placement> = Tag {
    all: &Placement::ALL,
    name: Placement::as_str,
};

/// The `lint` section's description in the schema.
const LINT: &str = "What to read that refuses nothing: the best likely duplicate pairs, best \
                    first, then the quality findings and the decisions with only file \
                    anchors by decision name, out of `total`.";

impl Output for Report {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("project", "The project's name.", &Str, |report| {
            &report.project
        });
        let placement = "`tracked` when the project's git versions the graph, `shared` \
                         when the graph is a repository of its own.";
        fields.required("placement", placement, &PLACEMENT, |report| {
            &report.placement
        });
        let graph = "The graph directory, when the tree reaches one it does not hold.";
        fields.optional("graph", graph, &Str, |report| report.graph.as_deref());
        let code_root = "The tree anchors resolve against, when the graph is not its own.";
        fields.optional("code_root", code_root, &Str, |report| {
            report.code_root.as_deref()
        });
        let counts = Nested::<Counts>::new();
        fields.required("counts", "The graph's counts.", &counts, |report| {
            &report.counts
        });
        let quality = "What `[quality]` in project.toml adds to the built-in write rules.";
        let kind = Nested::<QualityCounts>::new();
        fields.required("quality", quality, &kind, |report| &report.quality);
        declare_sections(fields);
    }
}

/// The fields of a [`Report`] that hold its sections.
fn declare_sections(fields: &mut Shown<'_, Report>) {
    let issues = Nested::<Part<IssueItem>>::new();
    fields.optional(
        "issues",
        "The validation issues, sorted.",
        &issues,
        |report| report.issues.as_ref(),
    );
    let history = "What a shared graph's own history lacks; empty under `tracked`.";
    let kind = Nested::<Part<HistoryItem>>::new();
    fields.optional("history", history, &kind, |report| report.history.as_ref());
    let components = "The project-wide decisions, then each component's, by name.";
    let kind = Nested::<Part<ComponentItem>>::new();
    fields.optional("components", components, &kind, |report| {
        report.components.as_ref()
    });
    let kind = Nested::<Part<LintItem>>::new();
    fields.optional("lint", LINT, &kind, |report| report.lint.as_ref());
    let anchors = "The anchors that do not plainly resolve in this tree, by kind, \
                   decision, anchor.";
    let kind = Nested::<Part<AnchorItem>>::new();
    fields.optional("anchors", anchors, &kind, |report| report.anchors.as_ref());
}
