//! The `components` section of `status`: the project-wide decisions, then
//! each component's, by name, counted.

use crate::store::{PROJECT_COMPONENT, ProjectState, Status};
use crate::tools::spec::output::{Count, Str};
use crate::tools::spec::{Output, Shown};

pub(crate) struct ComponentItem {
    pub(crate) name: String,
    pub(crate) decisions: usize,
    pub(crate) proposed: usize,
}

/// The project-wide decisions, then each component's, by name; under an
/// origin, only the components with a decision recorded under it.
pub(super) fn component_items(state: &ProjectState, origin: Option<&str>) -> Vec<ComponentItem> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{empty_state, sample_component, sample_decision};
    use crate::tools::status::counts::counts;

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
