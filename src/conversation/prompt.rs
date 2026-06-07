use crate::store;

/// Build the system prompt from component context, existing decisions, and mode.
///
/// Uses [`InMemoryGraph`] for connection and pattern queries rather than
/// scanning raw edge entries — consistent with the MCP context assembly
/// path and O(1) per lookup after the one-time graph build.
pub(crate) fn build_system_prompt(
    component: &str,
    state: &store::ProjectState,
    revisit: bool,
) -> String {
    let graph = &state.graph;
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
        let connects_to: Vec<String> = graph
            .connects_to(component)
            .iter()
            .map(|a| a.to_string())
            .collect();
        if !connects_to.is_empty() {
            p.push_str(&format!("Connects to: {}\n", connects_to.join(", ")));
        }
        let connects_from: Vec<String> = graph
            .connects_from(component)
            .iter()
            .map(|a| a.to_string())
            .collect();
        if !connects_from.is_empty() {
            p.push_str(&format!("Connects from: {}\n", connects_from.join(", ")));
        }
        p.push('\n');
    } else if component == "project" {
        p.push_str(&format!("## Project: {}\n\n", state.project.project.name));
    }

    // Existing decisions for this component
    let comp_decisions = graph.decisions_for(component);
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

    // Applicable patterns
    let patterns = graph.patterns_for(component);
    if !patterns.is_empty() {
        p.push_str("## Applicable patterns\n");
        for (name, pat) in &patterns {
            p.push_str(&format!("- {}: {}\n", name, pat.pattern.description));
        }
        p.push('\n');
    }

    // Project-wide decisions
    if component != "project" {
        let project_decisions = graph.project_decisions();
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
             and if better alternatives exist. When a decision is revised, output \
             the new decision JSON. Skip decisions the user wants to keep.\n\n",
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
    use crate::store::schema::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    fn test_state() -> store::ProjectState {
        let mut components = BTreeMap::new();
        components.insert(
            "auth".into(),
            ComponentFile {
                component: Component {
                    name: "auth".into(),
                    description: "Authentication service".into(),
                },
            },
        );
        components.insert(
            "database".into(),
            ComponentFile {
                component: Component {
                    name: "database".into(),
                    description: "Database access layer".into(),
                },
            },
        );

        let project = ProjectFile {
            trurl_version: "0.2.0".into(),
            project: Project {
                name: "test-project".into(),
                description: String::new(),
            },
        };

        let graph_index = GraphIndex {
            version: 1,
            rebuilt: Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "auth".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "database".into(),
                    to: "auth".into(),
                    kind: EdgeKind::ConnectsTo,
                },
            ],
        };

        store::ProjectState::new(
            project,
            components,
            BTreeMap::new(),
            BTreeMap::new(),
            graph_index,
        )
    }

    #[test]
    fn includes_component_context() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("auth"));
        assert!(prompt.contains("Authentication service"));
    }

    #[test]
    fn includes_forward_connections() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("Connects to: database"));
    }

    #[test]
    fn includes_reverse_connections() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("Connects from: database"));
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
    }

    #[test]
    fn project_wide() {
        let state = test_state();
        let prompt = build_system_prompt("project", &state, false);
        assert!(prompt.contains("test-project"));
    }

    #[test]
    fn includes_applicable_patterns() {
        let mut state = test_state();

        state.patterns.insert(
            "stateless-auth".into(),
            PatternFile {
                pattern: Pattern {
                    name: "stateless-auth".into(),
                    description: "All auth is stateless via JWT".into(),
                },
            },
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "stateless-auth".into(),
            kind: NodeKind::Pattern,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "stateless-auth".into(),
            to: "auth".into(),
            kind: EdgeKind::AppliesTo,
        });

        state.rebuild_graph();

        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("## Applicable patterns"));
        assert!(prompt.contains("stateless-auth"));
        assert!(prompt.contains("All auth is stateless via JWT"));
    }

    #[test]
    fn includes_existing_decisions() {
        let mut state = test_state();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap();
        state.decisions.insert(
            "use-jwt".into(),
            DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP".into(),
                    reason: "Stateless".into(),
                    alternatives: vec![],
                    created: ts,
                },
            },
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "use-jwt".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "use-jwt".into(),
            to: "auth".into(),
            kind: EdgeKind::BelongsTo,
        });

        state.rebuild_graph();

        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("## Existing decisions"));
        assert!(prompt.contains("JWT with DPoP"));
        assert!(prompt.contains("Stateless"));
    }

    #[test]
    fn includes_project_wide_decisions() {
        let mut state = test_state();
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap();
        state.decisions.insert(
            "error-strategy".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "Result<T, AppError>".into(),
                    reason: "Consistent errors".into(),
                    alternatives: vec![],
                    created: ts,
                },
            },
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "error-strategy".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "error-strategy".into(),
            to: "project".into(),
            kind: EdgeKind::BelongsTo,
        });

        state.rebuild_graph();

        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("## Project-wide decisions"));
        assert!(prompt.contains("Result<T, AppError>"));
    }
}
