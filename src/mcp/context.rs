use std::collections::BTreeSet;

use serde_json::Value;

use crate::store::schema::EdgeKind;
use crate::store::{DecisionFile, ProjectState};

// ── get_context ──────────────────────────────────────────────────────────

/// Assemble a tailored spec for a component: its decisions, project-wide
/// rules, related decisions from connected components, and a pre-assembled
/// authoritative brief.
pub(crate) fn get_context(
    state: &ProjectState,
    component: &str,
    task_description: Option<&str>,
) -> Result<Value, String> {
    if component == "project" {
        return Ok(project_context(state, task_description));
    }

    let comp = state
        .components
        .get(component)
        .ok_or_else(|| format!("component `{component}` does not exist"))?;

    // Forward connections: this component connects to...
    let connects_to: Vec<&str> = state
        .graph_index
        .edges
        .iter()
        .filter(|e| e.from == component && e.kind == EdgeKind::ConnectsTo)
        .map(|e| e.to.as_str())
        .collect();

    // Reverse connections: who connects TO this component.
    let connects_from: Vec<&str> = state
        .graph_index
        .edges
        .iter()
        .filter(|e| e.to == component && e.kind == EdgeKind::ConnectsTo)
        .map(|e| e.from.as_str())
        .collect();

    let component_decisions: Vec<(&String, &DecisionFile)> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == component)
        .collect();

    let project_decisions: Vec<(&String, &DecisionFile)> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == "project")
        .collect();

    // Related: decisions from directly connected components (both directions).
    let connected: BTreeSet<&str> = connects_to
        .iter()
        .chain(connects_from.iter())
        .copied()
        .collect();

    let related_decisions: Vec<(&String, &DecisionFile)> = state
        .decisions
        .iter()
        .filter(|(_, d)| connected.contains(d.decision.component.as_str()))
        .collect();

    let brief = build_brief(
        component,
        task_description,
        &component_decisions,
        &project_decisions,
        &related_decisions,
    );

    let status = if component_decisions.is_empty() && project_decisions.is_empty() {
        "not_covered"
    } else {
        "covered"
    };

    Ok(serde_json::json!({
        "component": {
            "name": comp.component.name,
            "description": comp.component.description,
            "connects_to": connects_to,
            "connects_from": connects_from,
        },
        "decisions": decision_list(&component_decisions),
        "project_rules": project_decisions.iter()
            .map(|(_, d)| &d.decision.choice)
            .collect::<Vec<_>>(),
        "related_decisions": related_decisions.iter()
            .map(|(_, d)| format!(
                "{}: {} ({})",
                d.decision.component, d.decision.choice, d.decision.reason
            ))
            .collect::<Vec<_>>(),
        "brief": brief,
        "status": status,
    }))
}

fn project_context(state: &ProjectState, task_description: Option<&str>) -> Value {
    let project_decisions: Vec<(&String, &DecisionFile)> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == "project")
        .collect();

    let mut brief = String::with_capacity(256);
    if let Some(task) = task_description {
        brief.push_str(&format!("TASK: {task}\n\n"));
    }
    if project_decisions.is_empty() {
        brief.push_str("No project-wide decisions recorded yet.\n");
    } else {
        brief.push_str("PROJECT-WIDE RULES:\n");
        for (_, d) in &project_decisions {
            brief.push_str(&format!("- {}\n", d.decision.choice));
        }
    }
    brief.push_str("\nWHEN UNCERTAIN:\n");
    brief.push_str("STOP. Run `trurl design project` first.\n");

    let status = if project_decisions.is_empty() {
        "not_covered"
    } else {
        "covered"
    };

    serde_json::json!({
        "component": {
            "name": "project",
            "description": state.project.project.description,
        },
        "decisions": decision_list(&project_decisions),
        "project_rules": project_decisions.iter()
            .map(|(_, d)| &d.decision.choice).collect::<Vec<_>>(),
        "related_decisions": [],
        "brief": brief,
        "status": status,
    })
}

// ── build_brief ──────────────────────────────────────────────────────────

/// Format the authoritative brief that coding agents consume directly.
fn build_brief(
    component: &str,
    task_description: Option<&str>,
    component_decisions: &[(&String, &DecisionFile)],
    project_decisions: &[(&String, &DecisionFile)],
    related_decisions: &[(&String, &DecisionFile)],
) -> String {
    let mut brief = String::with_capacity(512);

    if let Some(task) = task_description {
        brief.push_str(&format!("TASK: {task}\n\n"));
    }

    if !project_decisions.is_empty() {
        brief.push_str("RULES:\n");
        for (_, d) in project_decisions {
            brief.push_str(&format!("- {}\n", d.decision.choice));
        }
        brief.push('\n');
    }

    brief.push_str(&format!("COMPONENT: {component}\n"));
    if component_decisions.is_empty() {
        brief.push_str("- No decisions recorded yet.\n");
    } else {
        for (_, d) in component_decisions {
            brief.push_str(&format!(
                "- {} ({})\n",
                d.decision.choice, d.decision.reason
            ));
        }
    }
    brief.push('\n');

    if !related_decisions.is_empty() {
        brief.push_str("RELATED:\n");
        for (_, d) in related_decisions {
            brief.push_str(&format!(
                "- {}: {}\n",
                d.decision.component, d.decision.choice
            ));
        }
        brief.push('\n');
    }

    brief.push_str("WHEN UNCERTAIN:\n");
    brief.push_str(&format!(
        "STOP. This introduces a new pattern. Run `trurl design {component}` first.\n"
    ));

    brief
}

// ── check_pattern ────────────────────────────────────────────────────────

/// Check whether a pattern or approach is covered by existing decisions.
pub(crate) fn check_pattern(state: &ProjectState, description: &str) -> Value {
    let query_words = extract_words(description);
    if query_words.is_empty() {
        return serde_json::json!({
            "status": "not_covered",
            "message": "Description too short or vague. Provide more detail \
                        about the pattern to check.",
            "decisions": [],
        });
    }

    let mut matches: Vec<(usize, &str, &DecisionFile)> = Vec::new();

    for (name, dec) in &state.decisions {
        let haystack = format!(
            "{} {} {}",
            dec.decision.choice, dec.decision.reason, dec.decision.component
        );
        let decision_words = extract_words(&haystack);

        let overlap = query_words
            .iter()
            .filter(|qw| decision_words.iter().any(|dw| dw == *qw))
            .count();

        if overlap > 0 {
            matches.push((overlap, name.as_str(), dec));
        }
    }

    // Most relevant first.
    matches.sort_by(|a, b| b.0.cmp(&a.0));

    if matches.is_empty() {
        serde_json::json!({
            "status": "not_covered",
            "message": "No existing decisions cover this pattern. Suggest the \
                        developer run `trurl design <component>` to make \
                        architectural decisions before proceeding.",
            "decisions": [],
        })
    } else {
        serde_json::json!({
            "status": "covered",
            "message": "This pattern is addressed by existing decisions.",
            "decisions": matches.iter().map(|(_, name, d)| {
                serde_json::json!({
                    "name": name,
                    "component": d.decision.component,
                    "choice": d.decision.choice,
                    "reason": d.decision.reason,
                })
            }).collect::<Vec<_>>(),
        })
    }
}

// ── get_architecture ─────────────────────────────────────────────────────

pub(crate) fn get_architecture(state: &ProjectState) -> Value {
    let components: Vec<Value> = state
        .components
        .iter()
        .map(|(name, comp)| {
            let decision_count = state
                .decisions
                .values()
                .filter(|d| d.decision.component == *name)
                .count();

            let connects_to: Vec<&str> = state
                .graph_index
                .edges
                .iter()
                .filter(|e| e.from == *name && e.kind == EdgeKind::ConnectsTo)
                .map(|e| e.to.as_str())
                .collect();

            serde_json::json!({
                "name": name,
                "description": comp.component.description,
                "connects_to": connects_to,
                "decision_count": decision_count,
            })
        })
        .collect();

    let patterns: Vec<Value> = state
        .patterns
        .iter()
        .map(|(name, pat)| {
            serde_json::json!({
                "name": name,
                "description": pat.pattern.description,
            })
        })
        .collect();

    let project_decisions: Vec<Value> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == "project")
        .map(|(name, d)| {
            serde_json::json!({
                "name": name,
                "choice": d.decision.choice,
                "reason": d.decision.reason,
            })
        })
        .collect();

    serde_json::json!({
        "project": {
            "name": state.project.project.name,
            "description": state.project.project.description,
        },
        "components": components,
        "patterns": patterns,
        "project_decisions": project_decisions,
        "total_components": state.components.len(),
        "total_decisions": state.decisions.len(),
        "total_patterns": state.patterns.len(),
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn decision_list(decisions: &[(&String, &DecisionFile)]) -> Vec<Value> {
    decisions
        .iter()
        .map(|(name, d)| {
            serde_json::json!({
                "name": name,
                "choice": d.decision.choice,
                "reason": d.decision.reason,
            })
        })
        .collect()
}

fn extract_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !is_stop_word(w))
        .collect()
}

fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "the"
            | "and"
            | "for"
            | "are"
            | "but"
            | "not"
            | "you"
            | "all"
            | "can"
            | "had"
            | "her"
            | "was"
            | "one"
            | "our"
            | "out"
            | "has"
            | "have"
            | "been"
            | "were"
            | "being"
            | "will"
            | "would"
            | "could"
            | "should"
            | "must"
            | "shall"
            | "may"
            | "might"
            | "does"
            | "did"
            | "this"
            | "that"
            | "these"
            | "those"
            | "with"
            | "from"
            | "into"
            | "through"
            | "during"
            | "before"
            | "after"
            | "above"
            | "below"
            | "between"
            | "under"
            | "over"
            | "each"
            | "every"
            | "any"
            | "some"
            | "use"
            | "using"
            | "used"
            | "new"
            | "add"
            | "adding"
            | "added"
            | "about"
            | "which"
            | "when"
            | "where"
            | "what"
            | "how"
            | "why"
            | "also"
            | "just"
            | "than"
            | "then"
            | "them"
            | "they"
            | "their"
    )
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    fn test_state() -> ProjectState {
        let mut components = BTreeMap::new();
        components.insert(
            "auth".into(),
            ComponentFile {
                component: Component {
                    name: "auth".into(),
                    description: "Authentication and token management".into(),
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
        components.insert(
            "rate-limiter".into(),
            ComponentFile {
                component: Component {
                    name: "rate-limiter".into(),
                    description: "Request rate limiting".into(),
                },
            },
        );

        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap();
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "use-jwt".into(),
            DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP binding".into(),
                    reason: "Stateless, no session store needed".into(),
                    alternatives: vec!["Session cookies — rejected".into()],
                    created: ts,
                },
            },
        );
        decisions.insert(
            "error-strategy".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "ALL error handling MUST use Result<T, AppError>".into(),
                    reason: "Consistent error propagation".into(),
                    alternatives: vec![],
                    created: ts,
                },
            },
        );
        decisions.insert(
            "db-pool".into(),
            DecisionFile {
                decision: Decision {
                    component: "database".into(),
                    choice: "Shared connection pool via app state".into(),
                    reason: "Avoid per-request connection overhead".into(),
                    alternatives: vec![],
                    created: ts,
                },
            },
        );

        // Build graph index with nodes and edges.
        let graph_index = GraphIndex {
            version: 1,
            rebuilt: ts,
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
                NodeEntry {
                    name: "rate-limiter".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "use-jwt".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "error-strategy".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "db-pool".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: String::new(),
                },
            ],
            edges: vec![
                // ConnectsTo edges (previously in component.connects_to)
                EdgeEntry {
                    from: "auth".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "rate-limiter".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                // BelongsTo edges
                EdgeEntry {
                    from: "use-jwt".into(),
                    to: "auth".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "error-strategy".into(),
                    to: "project".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "db-pool".into(),
                    to: "database".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };

        ProjectState {
            project: ProjectFile {
                trurl_version: "0.2.0".into(),
                project: Project {
                    name: "test-project".into(),
                    description: "A test project".into(),
                },
            },
            components,
            decisions,
            patterns: BTreeMap::new(),
            graph_index,
        }
    }

    // ── get_context ─────────────────────────────────────────────────────

    #[test]
    fn get_context_returns_component_info() {
        let state = test_state();
        let result = get_context(&state, "auth", None).unwrap();

        assert_eq!(result["component"]["name"], "auth");
        assert_eq!(result["component"]["connects_to"][0], "database");
        assert_eq!(result["status"], "covered");
    }

    #[test]
    fn get_context_includes_reverse_connections() {
        let state = test_state();
        let result = get_context(&state, "database", None).unwrap();

        let connects_from = result["component"]["connects_from"].as_array().unwrap();
        let names: Vec<&str> = connects_from.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"auth"));
        assert!(names.contains(&"rate-limiter"));
    }

    #[test]
    fn get_context_includes_project_rules() {
        let state = test_state();
        let result = get_context(&state, "auth", None).unwrap();

        let rules = result["project_rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].as_str().unwrap().contains("Result<T, AppError>"));
    }

    #[test]
    fn get_context_includes_related_decisions() {
        let state = test_state();
        // auth connects_to database → database decisions are related.
        let result = get_context(&state, "auth", None).unwrap();

        let related = result["related_decisions"].as_array().unwrap();
        assert!(!related.is_empty());
        let text = related[0].as_str().unwrap();
        assert!(text.contains("database"));
        assert!(text.contains("connection pool"));
    }

    #[test]
    fn get_context_not_covered_when_no_decisions() {
        let mut state = test_state();
        state.decisions.clear();
        let result = get_context(&state, "auth", None).unwrap();

        assert_eq!(result["status"], "not_covered");
    }

    #[test]
    fn get_context_rejects_nonexistent_component() {
        let state = test_state();
        let err = get_context(&state, "nonexistent", None).unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn get_context_for_project() {
        let state = test_state();
        let result = get_context(&state, "project", None).unwrap();

        assert_eq!(result["component"]["name"], "project");
        assert_eq!(result["status"], "covered");
        let brief = result["brief"].as_str().unwrap();
        assert!(brief.contains("PROJECT-WIDE RULES"));
    }

    #[test]
    fn get_context_includes_task_in_brief() {
        let state = test_state();
        let result = get_context(&state, "auth", Some("implement token refresh")).unwrap();

        let brief = result["brief"].as_str().unwrap();
        assert!(brief.starts_with("TASK: implement token refresh\n"));
    }

    // ── build_brief ─────────────────────────────────────────────────────

    #[test]
    fn brief_has_when_uncertain() {
        let state = test_state();
        let result = get_context(&state, "auth", None).unwrap();
        let brief = result["brief"].as_str().unwrap();

        assert!(brief.contains("WHEN UNCERTAIN:"));
        assert!(brief.contains("STOP"));
        assert!(brief.contains("trurl design auth"));
    }

    #[test]
    fn brief_has_rules_section() {
        let state = test_state();
        let result = get_context(&state, "auth", None).unwrap();
        let brief = result["brief"].as_str().unwrap();

        assert!(brief.contains("RULES:\n"));
        assert!(brief.contains("Result<T, AppError>"));
    }

    // ── check_pattern ───────────────────────────────────────────────────

    #[test]
    fn check_pattern_finds_matching_decisions() {
        let state = test_state();
        let result = check_pattern(&state, "JWT token format for authentication");

        assert_eq!(result["status"], "covered");
        let decisions = result["decisions"].as_array().unwrap();
        assert!(!decisions.is_empty());
        assert!(decisions.iter().any(|d| d["name"] == "use-jwt"));
    }

    #[test]
    fn check_pattern_returns_not_covered() {
        let state = test_state();
        let result = check_pattern(&state, "WebSocket real-time notifications");

        assert_eq!(result["status"], "not_covered");
        let decisions = result["decisions"].as_array().unwrap();
        assert!(decisions.is_empty());
    }

    #[test]
    fn check_pattern_handles_empty_description() {
        let state = test_state();
        let result = check_pattern(&state, "");
        assert_eq!(result["status"], "not_covered");

        let result = check_pattern(&state, "a b");
        assert_eq!(result["status"], "not_covered");
    }

    #[test]
    fn check_pattern_case_insensitive() {
        let state = test_state();
        let result = check_pattern(&state, "REDIS CONNECTION POOL");
        assert_eq!(result["status"], "covered");
    }

    // ── get_architecture ────────────────────────────────────────────────

    #[test]
    fn get_architecture_returns_full_overview() {
        let state = test_state();
        let result = get_architecture(&state);

        assert_eq!(result["project"]["name"], "test-project");
        assert_eq!(result["total_components"], 3);
        assert_eq!(result["total_decisions"], 3);
        assert_eq!(result["total_patterns"], 0);

        let components = result["components"].as_array().unwrap();
        assert_eq!(components.len(), 3);

        let auth = components.iter().find(|c| c["name"] == "auth").unwrap();
        assert_eq!(auth["decision_count"], 1);
        let auth_connects = auth["connects_to"].as_array().unwrap();
        assert!(auth_connects.iter().any(|v| v == "database"));
    }

    // ── extract_words ───────────────────────────────────────────────────

    #[test]
    fn extract_words_filters_short_and_stop_words() {
        let words = extract_words("use the Redis for session storage");
        assert!(words.contains(&"redis".to_string()));
        assert!(words.contains(&"session".to_string()));
        assert!(words.contains(&"storage".to_string()));
        assert!(!words.contains(&"use".to_string()));
        assert!(!words.contains(&"the".to_string()));
        assert!(!words.contains(&"for".to_string()));
    }

    #[test]
    fn extract_words_lowercases() {
        let words = extract_words("JWT DPoP BINDING");
        assert!(words.contains(&"jwt".to_string()));
        assert!(words.contains(&"dpop".to_string()));
        assert!(words.contains(&"binding".to_string()));
    }
}
