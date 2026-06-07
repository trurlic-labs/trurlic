use std::sync::Arc;

use serde_json::Value;

use crate::store::graph::{Direction, InMemoryGraph};
use crate::store::schema::EdgeKind;
use crate::store::{DecisionFile, PatternFile, ProjectState};

// ── get_context ──────────────────────────────────────────────────────────

/// Assemble a tailored spec for a component: its decisions, project-wide
/// rules, related decisions from connected components, applicable patterns,
/// and a pre-assembled authoritative brief.
pub(crate) fn get_context(
    state: &ProjectState,
    component: &str,
    task_description: Option<&str>,
) -> Result<Value, String> {
    let graph = state.build_graph();

    if component == "project" {
        return Ok(project_context(state, &graph, task_description));
    }

    let comp = state
        .components
        .get(component)
        .ok_or_else(|| format!("component `{component}` does not exist"))?;

    let connects_to: Vec<String> = graph
        .connects_to(component)
        .iter()
        .map(|a| a.to_string())
        .collect();
    let connects_from: Vec<String> = graph
        .connects_from(component)
        .iter()
        .map(|a| a.to_string())
        .collect();

    let component_decisions = graph.decisions_for(component);
    let project_decisions = graph.project_decisions();
    let related_decisions = graph.related_decisions(component);
    let patterns = graph.patterns_for(component);

    let brief = build_brief(
        component,
        task_description,
        &component_decisions,
        &project_decisions,
        &related_decisions,
        &patterns,
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
        "decisions": format_decisions(&component_decisions),
        "project_rules": project_decisions.iter()
            .map(|(_, d)| &d.decision.choice)
            .collect::<Vec<_>>(),
        "patterns": format_patterns(&patterns),
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

fn project_context(
    state: &ProjectState,
    graph: &InMemoryGraph,
    task_description: Option<&str>,
) -> Value {
    let project_decisions = graph.project_decisions();

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
        "decisions": format_decisions(&project_decisions),
        "project_rules": project_decisions.iter()
            .map(|(_, d)| &d.decision.choice).collect::<Vec<_>>(),
        "patterns": [],
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
    component_decisions: &[(&Arc<str>, &DecisionFile)],
    project_decisions: &[(&Arc<str>, &DecisionFile)],
    related_decisions: &[(&Arc<str>, &DecisionFile)],
    patterns: &[(&Arc<str>, &PatternFile)],
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

    if !patterns.is_empty() {
        brief.push_str("PATTERNS:\n");
        for (name, p) in patterns {
            brief.push_str(&format!("- {}: {}\n", name.as_ref(), p.pattern.description));
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
///
/// Enhanced matching: keywords against decision content + node tags.
/// Pattern membership (via MemberOf edges) boosts ranking.
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

    let graph = state.build_graph();

    struct Match<'a> {
        score: usize,
        in_pattern: bool,
        name: &'a str,
        dec: &'a DecisionFile,
    }

    let mut matches: Vec<Match<'_>> = Vec::new();

    for (name, dec) in &state.decisions {
        let haystack = format!(
            "{} {} {}",
            dec.decision.choice, dec.decision.reason, dec.decision.component
        );
        let decision_words = extract_words(&haystack);

        let keyword_hits = query_words
            .iter()
            .filter(|qw| decision_words.iter().any(|dw| dw == *qw))
            .count();

        // Tag hits (weighted 2×) from graph node metadata.
        let tag_hits = graph
            .node_meta(name)
            .map(|m| {
                query_words
                    .iter()
                    .filter(|qw| m.tags.iter().any(|t| t.as_ref() == qw.as_str()))
                    .count()
            })
            .unwrap_or(0);

        let score = keyword_hits + tag_hits * 2;
        if score == 0 {
            continue;
        }

        let in_pattern = graph
            .edges_involving(name)
            .iter()
            .any(|(_, e, d)| e.kind == EdgeKind::MemberOf && *d == Direction::Reverse);

        matches.push(Match {
            score,
            in_pattern,
            name,
            dec,
        });
    }

    // Pattern members first, then by score descending.
    matches.sort_by(|a, b| b.in_pattern.cmp(&a.in_pattern).then(b.score.cmp(&a.score)));

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
            "decisions": matches.iter().map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "component": m.dec.decision.component,
                    "choice": m.dec.decision.choice,
                    "reason": m.dec.decision.reason,
                })
            }).collect::<Vec<_>>(),
        })
    }
}

// ── get_architecture ─────────────────────────────────────────────────────

pub(crate) fn get_architecture(state: &ProjectState) -> Value {
    let graph = state.build_graph();

    let components: Vec<Value> = state
        .components
        .iter()
        .map(|(name, comp)| {
            let decision_count = graph.decisions_for(name).len();
            let connects_to: Vec<String> = graph
                .connects_to(name)
                .iter()
                .map(|a| a.to_string())
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

    let project_decisions: Vec<Value> = graph
        .project_decisions()
        .iter()
        .map(|(name, d)| {
            serde_json::json!({
                "name": name.as_ref(),
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

fn format_decisions(decisions: &[(&Arc<str>, &DecisionFile)]) -> Vec<Value> {
    decisions
        .iter()
        .map(|(name, d)| {
            serde_json::json!({
                "name": name.as_ref(),
                "choice": d.decision.choice,
                "reason": d.decision.reason,
            })
        })
        .collect()
}

fn format_patterns(patterns: &[(&Arc<str>, &PatternFile)]) -> Vec<Value> {
    patterns
        .iter()
        .map(|(name, p)| {
            serde_json::json!({
                "name": name.as_ref(),
                "description": p.pattern.description,
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
                    tags: vec!["auth".into(), "security".into()],
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
                    tags: vec!["storage".into()],
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
                    from: "rate-limiter".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
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
        let result = get_context(&state, "auth", None).unwrap();

        let related = result["related_decisions"].as_array().unwrap();
        assert!(!related.is_empty());
        let text = related[0].as_str().unwrap();
        assert!(text.contains("database"));
        assert!(text.contains("connection pool"));
    }

    #[test]
    fn get_context_includes_patterns_field() {
        let state = test_state();
        let result = get_context(&state, "auth", None).unwrap();
        // Empty patterns for this fixture, but field must exist.
        let patterns = result["patterns"].as_array().unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn get_context_not_covered_when_no_decisions() {
        let mut state = test_state();
        state.decisions.clear();
        // Clear edges that reference decisions
        state
            .graph_index
            .edges
            .retain(|e| e.kind == EdgeKind::ConnectsTo);
        state
            .graph_index
            .nodes
            .retain(|n| n.kind == NodeKind::Component);
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

    #[test]
    fn check_pattern_boosts_tag_matches() {
        let state = test_state();
        // "security" is a tag on use-jwt. Should boost its ranking.
        let result = check_pattern(&state, "security authentication tokens");
        assert_eq!(result["status"], "covered");
        let decisions = result["decisions"].as_array().unwrap();
        assert_eq!(decisions[0]["name"], "use-jwt");
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
