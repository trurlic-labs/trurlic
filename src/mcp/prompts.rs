use serde_json::Value;

use crate::store::ProjectState;
use crate::store::graph::InMemoryGraph;
use crate::store::schema::DecisionFile;

use super::context;

// ── Concern tracking ────────────────────────────────────────────────────────

/// Architectural concern areas and keywords for matching against decision
/// content. Used to track design session progress: decisions matching a
/// concern's keywords mark it as covered, so the agent focuses on gaps.
const CONCERNS: &[(&str, &[&str])] = &[
    (
        "Data format & serialization",
        &[
            "format",
            "toml",
            "json",
            "yaml",
            "serialize",
            "deserialize",
            "schema",
            "encoding",
            "parse",
            "marshal",
            "protobuf",
        ],
    ),
    (
        "Error handling & failure modes",
        &[
            "error", "errors", "fail", "failure", "panic", "result", "recovery", "retry",
            "graceful", "crash", "fault", "fallback",
        ],
    ),
    (
        "Concurrency & locking",
        &[
            "lock",
            "locking",
            "concurrent",
            "concurrency",
            "mutex",
            "rwlock",
            "atomic",
            "thread",
            "async",
            "parallel",
            "race",
            "deadlock",
            "flock",
        ],
    ),
    (
        "Integrity & validation",
        &[
            "hash",
            "hashing",
            "validate",
            "validation",
            "integrity",
            "verify",
            "blake3",
            "sha256",
            "checksum",
            "corrupt",
            "consistency",
        ],
    ),
    (
        "Performance constraints",
        &[
            "performance",
            "latency",
            "throughput",
            "cache",
            "caching",
            "memory",
            "speed",
            "budget",
            "target",
            "millisecond",
            "benchmark",
            "optimize",
        ],
    ),
    (
        "External interfaces & APIs",
        &[
            "api",
            "interface",
            "endpoint",
            "protocol",
            "http",
            "rpc",
            "mcp",
            "rest",
            "grpc",
            "websocket",
            "boundary",
            "stdio",
        ],
    ),
    (
        "Security boundaries",
        &[
            "security",
            "auth",
            "authentication",
            "authorization",
            "token",
            "permission",
            "trust",
            "encrypt",
            "encryption",
            "secret",
            "credential",
            "tls",
            "certificate",
            "zeroize",
        ],
    ),
    (
        "Storage & persistence",
        &[
            "storage",
            "file",
            "disk",
            "persist",
            "persistence",
            "write",
            "read",
            "database",
            "redis",
            "save",
            "load",
            "filesystem",
        ],
    ),
    (
        "Dependencies & coupling",
        &[
            "dependency",
            "dependencies",
            "crate",
            "library",
            "coupling",
            "vendor",
            "package",
            "module",
        ],
    ),
    (
        "Migration & versioning",
        &[
            "migration",
            "migrate",
            "version",
            "versioning",
            "upgrade",
            "backward",
            "compatibility",
            "breaking",
        ],
    ),
];

/// Check if a decision's content matches any keyword for a concern area.
/// Uses word-boundary matching to avoid substring false positives.
fn decision_covers_concern(dec: &DecisionFile, keywords: &[&str]) -> bool {
    let text = format!(
        "{} {} {}",
        dec.decision.choice,
        dec.decision.reason,
        dec.decision.tags.join(" "),
    );
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .collect();
    keywords.iter().any(|kw| words.iter().any(|w| w == kw))
}

/// Analyze which concern areas are covered by existing decisions.
/// Returns formatted prompt text showing covered (with decision names)
/// and uncovered areas for the agent to explore.
fn concern_status(decisions: &[&DecisionFile]) -> String {
    let mut covered: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut uncovered: Vec<&str> = Vec::new();

    for &(concern_name, keywords) in CONCERNS {
        let matching: Vec<&str> = decisions
            .iter()
            .filter(|d| decision_covers_concern(d, keywords))
            .map(|d| d.decision.choice.as_str())
            .collect();

        if matching.is_empty() {
            uncovered.push(concern_name);
        } else {
            covered.push((concern_name, matching));
        }
    }

    let mut out = String::new();

    if !covered.is_empty() {
        out.push_str("COVERED (decisions exist — do not re-ask):\n");
        for (name, choices) in &covered {
            out.push_str(&format!("  ✓ {name}: \"{}\"\n", choices.join("\", \"")));
        }
        out.push('\n');
    }

    if !uncovered.is_empty() {
        out.push_str("UNCOVERED (systematically ask about each):\n");
        for name in &uncovered {
            out.push_str(&format!("  □ {name}\n"));
        }
        out.push('\n');
    }

    out
}

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
    let mut out = String::with_capacity(2048);

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

    // Dynamic concern tracking — shows the agent what's already covered
    // and what needs exploration.
    let all_decs: Vec<&DecisionFile> = project_rules
        .iter()
        .chain(existing.iter())
        .map(|(_, d)| *d)
        .collect();
    out.push_str(&concern_status(&all_decs));

    out.push_str(
        "PHASE 1 — SCOPE:\n\
         Ask the user:\n\
         - What is this component responsible for?\n\
         - What is explicitly NOT its responsibility?\n\
         - What components does it interact with?\n\
         After each answer, call record_decision with tag \"scope\".\n\n\
         PHASE 2 — TECHNICAL CHOICES:\n\
         Work through each UNCOVERED concern above. For each:\n\
         - Present 2-3 viable options with trade-offs\n\
         - Ask the user to choose\n\
         - Call record_decision\n\
         If the component has domain-specific concerns not listed above,\n\
         ask about those too.\n\n\
         COMPREHENSION GATE (after each decision):\n\
         State one concrete, testable implication:\n\
           \"This means that when [specific scenario], the system will \
         [specific behavior].\"\n\
         Ask: \"Is that correct, or am I missing something?\"\n\
         Wait for the user's response. If they correct you, adjust the decision.\n\
         Do not move to the next concern until the gate is satisfied.\n\
         Decisions without verified implications are incomplete.\n\n\
         DEPTH CHECK — after every 2-3 decisions:\n\
         \"Are there other architectural choices about this component we \
         haven't captured?\"\n\
         Continue until the user says done. Three decisions for a complex \
         component almost certainly means important choices are still implicit.\n\n\
         PHASE 3 — PATTERN DETECTION:\n\
         After each decision, scan ALL decisions (this component + project-wide)\n\
         for shared themes:\n\
         - Same technology chosen across different concerns\n\
           (e.g., Redis for rate limiting AND session cache → \
         \"All state in Redis\")\n\
         - Same strategy applied repeatedly\n\
           (e.g., fail-closed in auth AND writes → \
         \"Fail-closed everywhere\")\n\
         - Same constraint or boundary repeated\n\
           (e.g., <100ms on multiple operations → \
         \"Sub-100ms budget\")\n\
         If you identify a theme spanning 2+ decisions, present it:\n\
           \"These decisions share a pattern: [description]. Record it?\"\n\
         If confirmed, call record_pattern with the constituent decision names.\n\n\
         PHASE 4 — SUMMARY CHECKPOINT:\n\
         Ask: \"Before we implement — summarize this design. What are the \
         constraints the implementation must respect?\"\n\
         Do NOT proceed to implementation until the user demonstrates \
         understanding of the design they just created.\n",
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
    let project_rules = graph.project_decisions();

    if !existing.is_empty() || !project_rules.is_empty() {
        out.push_str("ACTIVE CONSTRAINTS (the user must confirm each applies):\n");
        for (name, d) in &project_rules {
            out.push_str(&format!(
                "- [project] {}: {} ({})\n",
                name, d.decision.choice, d.decision.reason
            ));
        }
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
        "STEP 1 — CONFIRM EXISTING CONSTRAINTS:\n\
         Present the constraints above and ask:\n\
         \"This task touches this component. Here are the existing constraints.\n\
         Do all of these still apply, or does something need to change?\"\n\
         Wait for the user to confirm or flag changes.\n\
         If changes → call update_decision (amend or supersede) for each.\n\n\
         STEP 2 — CHECK FOR NEW DECISIONS:\n\
         Ask: \"Does this task introduce any new architectural choice not\n\
         covered by the constraints above?\"\n\
         If NO → call get_context for the implementation brief and proceed.\n\
         If YES → ask 1-3 targeted questions, call record_decision for each.\n\n\
         COMPREHENSION GATE (only if any decision was changed or added):\n\
         State one concrete implication:\n\
           \"This means that when [specific scenario], the system will \
         [specific behavior].\"\n\
         Ask: \"Is that correct?\"\n",
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
        out.push_str(
            "No decisions recorded for this component yet.\n\n\
             This component exists in the codebase but has no architectural decisions\n\
             captured. Probe for implicit decisions that are embedded in the code:\n\n\
             Ask the user about each of these (skip clearly irrelevant ones):\n\
             - What data format does this component use, and why?\n\
             - How does it handle errors and failure modes?\n\
             - What concurrency or locking strategy does it use?\n\
             - What integrity or validation rules does it enforce?\n\
             - What are its performance constraints?\n\
             - What are its external interfaces?\n\
             - What security boundaries does it respect?\n\
             - What are its key dependencies, and why those?\n\n\
             For each answer that reveals an architectural choice, call record_decision.\n\
             After each decision, ask \"What else?\" until the user says they're done.\n\
             Learning IS capturing — these are decisions that already exist in the code\n\
             but haven't been made explicit.\n",
        );
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
         update_decision with mode=\"supersede\".\n\n\
         AFTER ALL DECISIONS REVIEWED:\n\
         Ask: \"Are there architectural decisions about this component that exist\n\
         in the code but haven't been captured here? Common areas: error handling,\n\
         concurrency, data formats, performance constraints, security boundaries.\"\n\
         For each new decision identified, call record_decision.\n\n\
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
    fn full_mode_has_concern_checklist() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Full).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        // Auth has one decision (JWT with DPoP) — some concerns should be
        // COVERED and others UNCOVERED.
        assert!(
            instructions.contains("COVERED") || instructions.contains("UNCOVERED"),
            "should have dynamic concern status"
        );
        assert!(instructions.contains("COMPREHENSION GATE"));
        assert!(instructions.contains("DEPTH CHECK"));
        assert!(instructions.contains("PATTERN DETECTION"));
    }

    #[test]
    fn full_mode_concern_status_reflects_existing_decisions() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Full).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        // "JWT with DPoP binding" should trigger Security boundaries concern.
        // "Stateless, no session store" has no direct keyword match for other concerns.
        // Project rule "Result<T, AppError>" should trigger Error handling concern.
        assert!(
            instructions.contains("COVERED"),
            "should show covered concerns: {instructions}"
        );
        assert!(
            instructions.contains("UNCOVERED"),
            "should show uncovered concerns for remaining areas: {instructions}"
        );
    }

    #[test]
    fn full_mode_empty_component_all_uncovered() {
        let state = test_state();
        // database has no decisions in this fixture.
        let result = build_design_prompt(&state, "database", None, DesignMode::Full).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        // Project-wide "Result<T, AppError>" covers Error handling.
        // Everything else should be UNCOVERED.
        assert!(
            instructions.contains("UNCOVERED"),
            "mostly-empty component should have many uncovered concerns"
        );
    }

    #[test]
    fn quick_mode_compact() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Quick).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        assert!(instructions.contains("quick"));
        assert!(instructions.contains("ACTIVE CONSTRAINTS"));
        assert!(instructions.contains("JWT with DPoP"));
        assert!(instructions.contains("CONFIRM EXISTING"));
        assert!(instructions.contains("CHECK FOR NEW"));
        assert!(!instructions.contains("PHASE 1"));
        assert_eq!(result["token_budget"], "compact");
    }

    #[test]
    fn quick_mode_includes_project_rules() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Quick).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();

        assert!(
            instructions.contains("[project]"),
            "quick mode should show project rules for confirmation"
        );
        assert!(instructions.contains("Result<T, AppError>"));
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
        assert!(
            instructions.contains("Probe for implicit decisions"),
            "empty learn mode should guide the agent to probe for unrecorded decisions"
        );
    }

    #[test]
    fn learn_mode_probes_for_missing_decisions() {
        let state = test_state();
        let result = build_design_prompt(&state, "auth", None, DesignMode::Learn).unwrap();
        let instructions = result["system_instructions"].as_str().unwrap();
        assert!(
            instructions.contains("AFTER ALL DECISIONS REVIEWED"),
            "learn mode should ask about unrecorded decisions after reviewing existing ones"
        );
    }

    // ── concern tracking ───────────────────────────────────────────────

    #[test]
    fn concern_matching_security_keywords() {
        let dec = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT with DPoP binding".into(),
                reason: "Token security via proof-of-possession".into(),
                alternatives: vec![],
                tags: vec!["security".into()],
                created: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            },
        };
        // "security" and "token" are Security concern keywords.
        let security_kw = CONCERNS
            .iter()
            .find(|(name, _)| *name == "Security boundaries")
            .map(|(_, kw)| *kw)
            .unwrap();
        assert!(decision_covers_concern(&dec, security_kw));
    }

    #[test]
    fn concern_matching_no_false_positives() {
        let dec = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT tokens".into(),
                reason: "Stateless".into(),
                alternatives: vec![],
                tags: vec![],
                created: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            },
        };
        // "JWT tokens" / "Stateless" should NOT match concurrency concern.
        let concurrency_kw = CONCERNS
            .iter()
            .find(|(name, _)| *name == "Concurrency & locking")
            .map(|(_, kw)| *kw)
            .unwrap();
        assert!(!decision_covers_concern(&dec, concurrency_kw));
    }

    #[test]
    fn concern_status_shows_covered_and_uncovered() {
        let dec = DecisionFile {
            decision: Decision {
                component: "store".into(),
                choice: "BLAKE3 content hashing".into(),
                reason: "Fast integrity verification".into(),
                alternatives: vec![],
                tags: vec![],
                created: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            },
        };
        let output = concern_status(&[&dec]);
        assert!(output.contains("COVERED"), "should have covered section");
        assert!(
            output.contains("Integrity"),
            "BLAKE3/hashing should match Integrity concern"
        );
        assert!(
            output.contains("UNCOVERED"),
            "should have uncovered section"
        );
        assert!(
            output.contains("Concurrency"),
            "Concurrency should be uncovered"
        );
    }
}
