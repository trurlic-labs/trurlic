//! System prompt construction for design conversations.

use crate::store;

/// Build the system prompt from component context, existing decisions, and mode.
pub(crate) fn build_system_prompt(
    component: &str,
    state: &store::ProjectState,
    revisit: bool,
) -> String {
    let mut p = String::with_capacity(2048);

    p.push_str(
        "You are Trurl, a meticulous architectural design assistant. \
         You conduct focused Socratic design conversations, one question at a time.\n\n",
    );

    // Component context
    if let Some(comp) = state.components.get(component) {
        p.push_str(&format!("## Component: {}\n", comp.component.name));
        if !comp.component.description.is_empty() {
            p.push_str(&format!("Description: {}\n", comp.component.description));
        }
        if !comp.component.connects_to.is_empty() {
            p.push_str(&format!(
                "Connects to: {}\n",
                comp.component.connects_to.join(", ")
            ));
        }
        p.push('\n');
    } else if component == "project" {
        p.push_str(&format!("## Project: {}\n\n", state.project.project.name));
    }

    // Existing decisions for this component
    let comp_decisions: Vec<_> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == component)
        .collect();

    if !comp_decisions.is_empty() {
        p.push_str("## Existing decisions for this component\n");
        for (name, dec) in &comp_decisions {
            p.push_str(&format!(
                "- {}: {} (reason: {})\n",
                name, dec.decision.choice, dec.decision.reason
            ));
        }
        p.push('\n');
    }

    // Project-wide decisions
    if component != "project" {
        let project_decisions: Vec<_> = state
            .decisions
            .iter()
            .filter(|(_, d)| d.decision.component == "project")
            .collect();

        if !project_decisions.is_empty() {
            p.push_str("## Project-wide decisions (apply everywhere)\n");
            for (name, dec) in &project_decisions {
                p.push_str(&format!(
                    "- {}: {} (reason: {})\n",
                    name, dec.decision.choice, dec.decision.reason
                ));
            }
            p.push('\n');
        }
    }

    // Mode-specific instructions
    if revisit {
        p.push_str(
            "## Mode: Revisit\n\
             Challenge each existing decision. Ask if the reasoning still holds \
             and if better alternatives exist. For changed decisions, output the \
             new decision JSON with a \"supersedes\" field naming the decision \
             being replaced (e.g. \"supersedes\": \"auth-token-format\"). \
             Skip decisions the user wants to keep.\n\n",
        );
    }

    p.push_str(
        "## Instructions\n\n\
         Ask ONE design question at a time. After the user answers, summarize \
         their decision as a JSON object on its own line:\n\n\
         {\"choice\": \"concise decision title\", \"reason\": \"the reasoning\", \
         \"alternatives\": [\"Option A — rejected: why\"]}\n\n\
         Include \"alternatives\" only when other options were discussed or \
         are worth noting. Omit the field when there are none.\n\n\
         Then continue with the next question. Cover key technical choices, \
         patterns, constraints, and integration points. Reference existing \
         decisions and connections for consistency.\n\n\
         When all important design aspects are covered, output DESIGN_COMPLETE \
         on its own line.\n",
    );

    p
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{Component, ComponentFile, Project, ProjectFile};
    use std::collections::BTreeMap;

    fn test_state() -> store::ProjectState {
        let mut components = BTreeMap::new();
        components.insert(
            "auth".into(),
            ComponentFile {
                component: Component {
                    name: "auth".into(),
                    description: "Authentication service".into(),
                    connects_to: vec!["database".into()],
                },
            },
        );

        let project = ProjectFile {
            trurl_version: "0.1.0".into(),
            project: Project {
                name: "test-project".into(),
                description: String::new(),
            },
        };

        store::ProjectState {
            project,
            components,
            decisions: BTreeMap::new(),
        }
    }

    #[test]
    fn includes_component_context() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("auth"));
        assert!(prompt.contains("database"));
        assert!(prompt.contains("Authentication service"));
    }

    #[test]
    fn includes_instructions() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("DESIGN_COMPLETE"));
        assert!(prompt.contains("\"choice\""));
        assert!(prompt.contains("\"alternatives\""));
    }

    #[test]
    fn revisit_mode() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, true);
        assert!(prompt.contains("Revisit"));
        assert!(prompt.contains("Challenge"));
        assert!(prompt.contains("\"supersedes\""));
    }

    #[test]
    fn project_wide() {
        let state = test_state();
        let prompt = build_system_prompt("project", &state, false);
        assert!(prompt.contains("test-project"));
    }
}
