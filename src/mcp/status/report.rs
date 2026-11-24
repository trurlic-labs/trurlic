//! What `status` answers, declared once for its `outputSchema` and its
//! JSON: the counts, and each section as the items that fit out of its
//! total.

use crate::store::{IssueKind, Placement};

use crate::mcp::spec::output::{Count, Items, Nested, Str, Tag};
use crate::mcp::spec::{Output, Shown};

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
}

pub(crate) struct Counts {
    pub(crate) components: usize,
    pub(crate) decisions: usize,
    pub(crate) project_wide: usize,
    pub(crate) proposed: usize,
    pub(crate) edges: usize,
    pub(crate) issues: usize,
    pub(crate) non_canonical: usize,
}

/// What `[quality]` adds to the built-in rules, counted.
pub(crate) struct QualityCounts {
    /// The limits set, `require_origin` among them.
    pub(crate) limits: usize,
    pub(crate) refusing: usize,
    pub(crate) warning: usize,
}

/// A section: the items that fit, out of `total`.
pub(crate) struct Part<T> {
    pub(crate) total: usize,
    pub(crate) items: Vec<T>,
    pub(crate) omitted: usize,
    pub(crate) more: Option<String>,
}

impl<T> Part<T> {
    /// The section of `items` with none of them shown: what a report
    /// measures before it is filled.
    pub(super) fn frame(items: &[T]) -> Self {
        Self {
            total: items.len(),
            items: Vec::new(),
            omitted: 0,
            more: None,
        }
    }
}

pub(crate) struct IssueItem {
    pub(crate) kind: IssueKind,
    pub(crate) message: String,
}

pub(crate) struct HistoryItem {
    pub(crate) kind: String,
    pub(crate) message: String,
}

pub(crate) struct ComponentItem {
    pub(crate) name: String,
    pub(crate) decisions: usize,
    pub(crate) proposed: usize,
}

const PLACEMENT: Tag<Placement> = Tag {
    all: &Placement::ALL,
    name: Placement::as_str,
};

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
        fields.optional("graph", graph, &Str, |report| report.graph.as_ref());
        let code_root = "The tree anchors resolve against, when the graph is not its own.";
        fields.optional("code_root", code_root, &Str, |report| {
            report.code_root.as_ref()
        });
        let counts = Nested::<Counts>::new();
        fields.required("counts", "The graph's counts.", &counts, |report| {
            &report.counts
        });
        let quality = "What `[quality]` in project.toml adds to the built-in write rules.";
        let kind = Nested::<QualityCounts>::new();
        fields.required("quality", quality, &kind, |report| &report.quality);
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
    }
}

impl Output for Counts {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("components", "Components.", &Count, |counts| {
            &counts.components
        });
        fields.required(
            "decisions",
            "Decisions, under `origin` when given.",
            &Count,
            |counts| &counts.decisions,
        );
        fields.required(
            "project_wide",
            "Of them, the project-wide.",
            &Count,
            |counts| &counts.project_wide,
        );
        fields.required("proposed", "Of them, the proposed.", &Count, |counts| {
            &counts.proposed
        });
        fields.required(
            "edges",
            "`connects_to` and `depends_on` edges.",
            &Count,
            |counts| &counts.edges,
        );
        fields.required("issues", "Validation issues.", &Count, |counts| {
            &counts.issues
        });
        fields.required(
            "non_canonical",
            "Files `trurlic fmt` would rewrite.",
            &Count,
            |counts| &counts.non_canonical,
        );
    }
}

impl Output for QualityCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let limits = "Limits set: `reason_words` bounds, the maxima, `require_origin`.";
        fields.required("limits", limits, &Count, |counts| &counts.limits);
        let refusing = "Patterns that refuse a write.";
        fields.required("refusing", refusing, &Count, |counts| &counts.refusing);
        let warning = "Patterns that warn in the result.";
        fields.required("warning", warning, &Count, |counts| &counts.warning);
    }
}

impl<T: Output> Output for Part<T> {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("total", "The section's items.", &Count, |part| &part.total);
        let items = Items {
            item: Nested::<T>::new(),
        };
        fields.required("items", "The items that fit, in order.", &items, |part| {
            &part.items
        });
        fields.optional("omitted", "The items left out.", &Count, |part| {
            (part.omitted > 0).then_some(&part.omitted)
        });
        fields.optional("more", "Where the items left out are.", &Str, |part| {
            part.more.as_ref()
        });
    }
}

impl Output for IssueItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        let kind = Tag {
            all: &IssueKind::ALL,
            name: IssueKind::as_str,
        };
        fields.required("kind", "The issue's kind.", &kind, |item| &item.kind);
        fields.required(
            "message",
            "The file at fault, then the fault.",
            &Str,
            |item| &item.message,
        );
    }
}

impl Output for HistoryItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required(
            "kind",
            "`missing`, `index_locked` or `uncommitted`.",
            &Str,
            |item| &item.kind,
        );
        fields.required(
            "message",
            "What is lacking and what mends it.",
            &Str,
            |item| &item.message,
        );
    }
}

impl Output for ComponentItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("name", "The component, or `project`.", &Str, |item| {
            &item.name
        });
        fields.required("decisions", "Its decisions.", &Count, |item| {
            &item.decisions
        });
        fields.required("proposed", "Of them, the proposed.", &Count, |item| {
            &item.proposed
        });
    }
}
