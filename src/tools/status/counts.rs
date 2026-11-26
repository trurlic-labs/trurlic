//! The counts of the `status` report: the graph's nodes, edges and issues,
//! the findings of each section that counts them, and what `[quality]`
//! adds to the built-in write rules.

use crate::store::{Level, PROJECT_COMPONENT, ProjectState, Quality, Status};
use crate::tools::spec::output::{Count, Nested};
use crate::tools::spec::{Output, Shown};

use super::anchors::AnchorCounts;
use super::lint::LintCounts;

pub(crate) struct Counts {
    pub(crate) components: usize,
    pub(crate) decisions: usize,
    pub(crate) project_wide: usize,
    pub(crate) proposed: usize,
    pub(crate) edges: usize,
    issues: usize,
    pub(crate) non_canonical: usize,
    pub(crate) lint: Option<LintCounts>,
    /// The anchor findings by kind, when the report has the section.
    pub(crate) anchors: Option<AnchorCounts>,
}

/// What `[quality]` adds to the built-in rules, counted.
pub(crate) struct QualityCounts {
    /// The limits set, `require_origin` among them.
    pub(crate) limits: usize,
    pub(crate) refusing: usize,
    pub(crate) warning: usize,
}

pub(super) fn counts(state: &ProjectState, origin: Option<&str>, issues: usize) -> Counts {
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

pub(super) fn quality_counts(quality: &Quality) -> QualityCounts {
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
        let lint = "The `lint` findings of each kind, when the section is asked for.";
        let kind = Nested::<LintCounts>::new();
        fields.optional("lint", lint, &kind, |counts| counts.lint.as_ref());
        let anchors = "The anchor findings by kind, under `origin` when given, when the \
                       report has the `anchors` section.";
        let kind = Nested::<AnchorCounts>::new();
        fields.optional("anchors", anchors, &kind, |counts| counts.anchors.as_ref());
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
