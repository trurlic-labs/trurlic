use serde_json::Value;

use crate::store::ProjectState;
use crate::store::graph::InMemoryGraph;

use super::context;

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesignMode {
    Full,
    Quick,
    Learn,
    Review,
}

impl DesignMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "full" => Ok(Self::Full),
            "quick" => Ok(Self::Quick),
            "learn" => Ok(Self::Learn),
            "review" => Ok(Self::Review),
            _ => Err(format!(
                "invalid mode `{s}` — expected: full, quick, learn, review"
            )),
        }
    }

    fn token_budget(self) -> &'static str {
        match self {
            Self::Full => "thorough",
            Self::Quick => "compact",
            Self::Learn => "standard",
            Self::Review => "standard",
        }
    }
}

// ── Public API ──────────────────────────────────────────────────────────────

pub(crate) fn build_design_prompt(
    state: &ProjectState,
    component: &str,
    task: Option<&str>,
    mode: DesignMode,
) -> Result<Value, String> {
    let graph = &state.graph;

    if component != "project" && !state.components.contains_key(component) {
        return Err(format!("component `{component}` does not exist"));
    }

    let ctx = context::get_context(state, component, task)?;

    let instructions = match mode {
        DesignMode::Full => build_full_instructions(graph, component, task),
        DesignMode::Quick => build_quick_instructions(graph, component, task),
        DesignMode::Learn => build_learn_instructions(graph, component),
        DesignMode::Review => build_review_instructions(graph, component),
    };

    Ok(serde_json::json!({
        "system_instructions": instructions,
        "context": ctx,
        "token_budget": mode.token_budget(),
    }))
}

// ── Full mode ───────────────────────────────────────────────────────────────

fn build_full_instructions(graph: &InMemoryGraph, component: &str, task: Option<&str>) -> String {
    let mut out = String::with_capacity(1024);

    out.push_str(&format!(
        "You are running a Trurl design session for component [{component}].\n\n"
    ));

    // Existing constraints.
    let project_rules = graph.project_decisions();
    let existing = graph.decisions_for(component);
    if !project_rules.is_empty() || !existing.is_empty() {
        out.push_str("EXISTING CONSTRAINTS (do not re-discuss):\n");
        for (_, d) in &project_rules {
            out.push_str(&format!("- {} (project rule)\n", d.decision.choice));
        }
        for (_, d) in &existing {
            out.push_str(&format!(
                "- {} ({})\n",
                d.decision.choice, d.decision.reason
            ));
        }
        out.push('\n');
    }

    // Connected components for context.
    let connects_to = graph.connects_to(component);
    let connects_from = graph.connects_from(component);
    if !connects_to.is_empty() || !connects_from.is_empty() {
        out.push_str("COMPONENT GRAPH:\n");
        for c in &connects_to {
            out.push_str(&format!("- {component} → {c}\n"));
        }
        for c in &connects_from {
            out.push_str(&format!("- {c} → {component}\n"));
        }
        out.push('\n');
    }

    if let Some(t) = task {
        out.push_str(&format!("TASK CONTEXT: {t}\n\n"));
    }

    out.push_str(
        "PHASE 1 — SCOPE:\n\
         Ask the user:\n\
         - What is this component responsible for?\n\
         - What is explicitly NOT its responsibility?\n\
         - What components does it interact with?\n\
         After each answer, call record_decision with tag \"scope\".\n\n\
         PHASE 2 — TECHNICAL CHOICES:\n\
         For each non-obvious decision this component needs:\n\
         - Present 2-3 viable options with trade-offs\n\
         - Ask the user to choose\n\
         - Call record_decision\n\
         - Then ask: \"In your own words, what does this mean for [connected component]?\"\n\
         If the user's answer is thin, explain the implication and ask again.\n\n\
         PHASE 3 — PATTERN RECOGNITION:\n\
         After 3+ decisions, check if they form a pattern with existing decisions.\n\
         If yes, present: \"These decisions together mean [X]. Record as a pattern?\"\n\
         If confirmed, call record_pattern.\n\n\
         PHASE 4 — SUMMARY CHECKPOINT:\n\
         Ask: \"Before we implement — summarize this design. What are the constraints?\"\n\
         Do NOT proceed to implementation until the user demonstrates understanding.\n",
    );

    out
}

// ── Quick mode ──────────────────────────────────────────────────────────────

fn build_quick_instructions(graph: &InMemoryGraph, component: &str, task: Option<&str>) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!(
        "You are running a quick Trurl design check for [{component}].\n\n"
    ));

    let existing = graph.decisions_for(component);
    if !existing.is_empty() {
        out.push_str("EXISTING DECISIONS:\n");
        for (name, d) in &existing {
            out.push_str(&format!(
                "- {}: {} ({})\n",
                name, d.decision.choice, d.decision.reason
            ));
        }
        out.push('\n');
    }

    if let Some(t) = task {
        out.push_str(&format!("TASK: {t}\n\n"));
    }

    out.push_str(
        "Ask the user: \"Does this task introduce any new pattern or \
         architectural choice not covered above?\"\n\n\
         If NO → proceed with existing constraints.\n\
         If YES → ask 1-3 targeted questions, call record_decision for each, \
         then proceed.\n\n\
         Comprehension gate (only if a new decision was recorded):\n\
         \"What does this decision mean for the connected components?\"\n",
    );

    out
}

// ── Learn mode ──────────────────────────────────────────────────────────────

fn build_learn_instructions(graph: &InMemoryGraph, component: &str) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!(
        "You are running a Trurl learn session for [{component}].\n\
         The goal is understanding, not implementation.\n\n"
    ));

    let decisions = graph.decisions_for(component);
    let patterns = graph.patterns_for(component);

    if decisions.is_empty() {
        out.push_str("No decisions recorded for this component yet.\n");
        return out;
    }

    out.push_str("Present each decision one by one. For each:\n\n");

    for (name, d) in &decisions {
        out.push_str(&format!("DECISION: {} — {}\n", name, d.decision.choice));
        out.push_str(&format!("  Reason: {}\n", d.decision.reason));
        if !d.decision.alternatives.is_empty() {
            out.push_str("  Alternatives considered:\n");
            for alt in &d.decision.alternatives {
                out.push_str(&format!("    - {alt}\n"));
            }
        }
        out.push_str(&format!(
            "  → Ask: \"Why was `{}` chosen over the alternatives?\"\n",
            d.decision.choice
        ));
        out.push_str("  → Ask: \"What would need to change if we reversed this?\"\n\n");
    }

    if !patterns.is_empty() {
        out.push_str("PATTERNS:\n");
        for (name, p) in &patterns {
            out.push_str(&format!("- {}: {}\n", name, p.pattern.description));
        }
        out.push('\n');
    }

    out.push_str(
        "If the user says \"actually, this should be different\", call \
         update_decision with mode=\"supersede\".\n\
         Learning IS the output. Do not proceed to implementation.\n",
    );

    out
}

// ── Review mode ─────────────────────────────────────────────────────────────

fn build_review_instructions(graph: &InMemoryGraph, component: &str) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!(
        "You are running a Trurl design review for [{component}].\n\
         Challenge each decision. Look for drift and staleness.\n\n"
    ));

    let mut decisions: Vec<_> = graph.decisions_for(component);
    // Sort oldest-first by created timestamp.
    decisions.sort_by_key(|(_, d)| d.decision.created);

    if decisions.is_empty() {
        out.push_str("No decisions to review for this component.\n");
        return out;
    }

    out.push_str("Review each decision oldest-first:\n\n");

    for (name, d) in &decisions {
        out.push_str(&format!(
            "DECISION: {} (created {})\n  {}: {}\n",
            name,
            d.decision.created.format("%Y-%m-%d"),
            d.decision.choice,
            d.decision.reason,
        ));
        out.push_str("  → Ask: \"Does this still hold?\"\n");
        out.push_str("  If YES → move on.\n");
        out.push_str(
            "  If NO → run a mini design conversation, then call \
             update_decision with mode=\"supersede\".\n\n",
        );
    }

    out.push_str(
        "After all decisions reviewed, call validate_consistency \
         to check graph health.\n",
    );

    out
}

// ── Tests ───────────────────────────────────────────────────────────────────

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
                    description: "Authentication".into(),
                },
            },
        );
        components.insert(
            "database".into(),
            ComponentFile {
                component: Component {
                    name: "database".into(),
                    description: "Database layer".into(),
                },
            },
        );

        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "use-jwt".into(),
            DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP binding".into(),
                    reason: "Stateless, no session store".into(),
                    alternatives: vec!["Session cookies — rejected: server state".into()],
                    tags: vec![],
                    created: ts,
                },
            },
        );
        decisions.insert(
            "error-strategy".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "Result<T, AppError>".into(),
                    reason: "Consistent errors".into(),
                    alternatives: vec![],
                    tags: vec![],
                    created: Utc.with_ymd_and_hms(2025, 1, 10, 8, 0, 0).unwrap(),
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
            ],
            edges: vec![
                EdgeEntry {
                    from: "auth".into(),
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
            ],
        };

        ProjectState::new(
            ProjectFile {
                trurl_version: FORMAT_VERSION.into(),
                project: Project {
                    name: "test-project".into(),
                    description: String::new(),
                },
            },
            components,
            decisions,
            BTreeMap::new(),
            graph_index,
        )
    }

    #[test]
    fn design_mode_parse() {
        assert_eq!(DesignMode::parse("full").unwrap(), DesignMode::Full);
        assert_eq!(DesignMode::parse("quick").unwrap(), DesignMode::Quick);
        assert_eq!(DesignMode::parse("learn").unwrap(), DesignMode::Learn);
        assert_eq!(DesignMode::parse("review").unwrap(), DesignMode::Review);
        assert!(DesignMode::parse("bogus").is_err());
    }

    #[test]
    fn full_mode_has_all_phases() {
        let state = test_state();
        let result =
            build_design_prompt(&state, "auth", Some("add rate limiting"), DesignMode::Full)
                .unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        assert!(instructions.contains("PHASE 1"));
        assert!(instructions.contains("PHASE 2"));
        assert!(instructions.contains("PHASE 3"));
        assert!(instructions.contains("PHASE 4"));
        assert!(instructions.contains("EXISTING CONSTRAINTS"));
        assert!(instructions.contains("Result<T, AppError>"));
        assert!(instructions.contains("JWT with DPoP"));
        assert!(instructions.contains("auth → database"));
        assert!(instructions.contains("rate limiting"));
        assert_eq!(result["token_budget"], "thorough");
    }

    #[test]
    fn quick_mode_compact() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Quick).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        assert!(instructions.contains("quick"));
        assert!(instructions.contains("EXISTING DECISIONS"));
        assert!(instructions.contains("JWT with DPoP"));
        assert!(!instructions.contains("PHASE 1"));
        assert_eq!(result["token_budget"], "compact");
    }

    #[test]
    fn learn_mode_has_challenge_questions() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Learn).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        assert!(instructions.contains("learn session"));
        assert!(instructions.contains("JWT with DPoP"));
        assert!(instructions.contains("Why was"));
        assert!(instructions.contains("reversed this"));
        assert!(instructions.contains("Session cookies"));
        assert_eq!(result["token_budget"], "standard");
    }

    #[test]
    fn review_mode_oldest_first() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Review).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        assert!(instructions.contains("review"));
        assert!(instructions.contains("Does this still hold"));
        assert!(instructions.contains("validate_consistency"));
        assert_eq!(result["token_budget"], "standard");
    }

    #[test]
    fn context_included_in_output() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Full).unwrap();
        // Context has the same shape as get_context output.
        assert_eq!(result["context"]["component"]["name"], "auth");
        assert!(result["context"]["brief"].is_string());
    }

    #[test]
    fn rejects_nonexistent_component() {
        let state = test_state();
        let err = build_design_prompt(&state, "ghost", None, DesignMode::Full).unwrap_err();
        assert!(err.contains("ghost"));
    }

    #[test]
    fn project_works() {
        let state = test_state();
        let result = build_design_prompt(&state, "project", None, DesignMode::Full).unwrap();
        assert_eq!(result["context"]["component"]["name"], "project");
    }

    #[test]
    fn learn_mode_empty_component() {
        let state = test_state();
        let result = build_design_prompt(&state, "database", None, DesignMode::Learn).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();
        assert!(instructions.contains("No decisions recorded"));
    }
}
