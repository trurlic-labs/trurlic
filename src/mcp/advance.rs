//! Deterministic workflow state machine for component readiness.
//!
//! The `advance` tool computes workflow state from the knowledge graph and
//! returns the single productive next action. Read-only, stateless, and
//! idempotent — no writes, no locks, no LLM calls.
//!
//! Five states — `unregistered`, `undecided`, `incomplete`, `stale`, `ready`
//! — are pure functions of the current graph contents. The graph IS the state.

use chrono::Utc;
use serde_json::Value;

use crate::store::ProjectState;
use crate::store::schema::DecisionFile;

use super::prompts;

// ── Constants ──────────────────────────────────────────────────────────────

/// Decisions older than this trigger `stale` state (days).
/// Shared with `context.rs` for consistent staleness detection.
pub(crate) const STALENESS_THRESHOLD_DAYS: i64 = 90;

/// Maximum concerns listed in `action.focus` for `incomplete` state.
/// Top concerns by priority (array order in `CONCERNS`) are selected.
const CONCERN_FOCUS_LIMIT: usize = 3;

// ── Types ──────────────────────────────────────────────────────────────────

/// Workflow states derived from graph data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Component does not exist in the graph.
    Unregistered,
    /// Component exists but has zero decisions.
    Undecided,
    /// Decisions exist but uncovered concern areas outnumber covered ones.
    Incomplete,
    /// Coverage is adequate but one or more decisions exceed the staleness threshold.
    Stale,
    /// Coverage is adequate and all decisions are fresh.
    Ready,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unregistered => "unregistered",
            Self::Undecided => "undecided",
            Self::Incomplete => "incomplete",
            Self::Stale => "stale",
            Self::Ready => "ready",
        }
    }
}

/// Caller intent determines state machine behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    /// Full readiness check — routes through design until coverage is adequate.
    Implement,
    /// Study existing decisions regardless of coverage.
    Learn,
    /// Challenge decisions for drift.
    Review,
}

impl Intent {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "implement" => Ok(Self::Implement),
            "learn" => Ok(Self::Learn),
            "review" => Ok(Self::Review),
            _ => Err(format!(
                "invalid intent `{s}` — expected: implement, learn, review"
            )),
        }
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Compute the workflow state for a component and return the next action.
///
/// Read-only. No writes. No locks. No LLM calls. Computes state from the
/// in-memory graph on every call — no session tracking, no persistent
/// workflow state.
pub(crate) fn advance(
    state: &ProjectState,
    component: &str,
    intent_str: Option<&str>,
    task: Option<&str>,
) -> Result<Value, String> {
    let intent = match intent_str {
        Some(s) => Intent::parse(s)?,
        None => Intent::Implement,
    };

    // Project scope: simplified two-state machine.
    if component == "project" {
        return Ok(advance_project(state, intent, task));
    }

    // Unregistered: component not in graph. Same for all intents —
    // you cannot learn, review, or implement something that doesn't exist.
    if !state.components.contains_key(component) {
        let suggested = crate::store::slugify(component);
        return Ok(response(
            component,
            State::Unregistered,
            false,
            Value::Null,
            serde_json::json!({
                "tool": "add_component",
                "args": { "name": suggested },
                "instruction": format!(
                    "Component `{component}` is not registered. \
                     Confirm the name and description with the user, \
                     then call add_component."
                ),
            }),
        ));
    }

    // ── Compute full state (all intents see the same assessment) ───────

    let graph = &state.graph;
    let decisions = graph.decisions_for(component);
    let project_rules = graph.project_decisions();

    // Concern coverage: project rules + component decisions.
    let all_decs: Vec<&DecisionFile> = project_rules
        .iter()
        .chain(decisions.iter())
        .map(|(_, d)| *d)
        .collect();
    let (covered, uncovered) = prompts::compute_concern_coverage(&all_decs);

    // Staleness detection.
    let now = Utc::now();
    let stale: Vec<Value> = decisions
        .iter()
        .filter_map(|(name, d)| {
            let age_days = now.signed_duration_since(d.decision.created).num_days();
            (age_days >= STALENESS_THRESHOLD_DAYS).then(|| {
                serde_json::json!({
                    "name": name.as_ref(),
                    "created": d.decision.created.to_rfc3339(),
                    "age_days": age_days,
                })
            })
        })
        .collect();

    let patterns_count = graph.patterns_for(component).len();

    // Evaluation order matters: undecided → incomplete → stale → ready.
    // Coverage is checked before staleness — design completeness takes
    // priority over recency. An incomplete component produces silent AI
    // choices; stale decisions at worst produce outdated constraints.
    let computed = if decisions.is_empty() {
        State::Undecided
    } else if uncovered.len() > covered.len() {
        State::Incomplete
    } else if !stale.is_empty() {
        State::Stale
    } else {
        State::Ready
    };

    let assessment = serde_json::json!({
        "decisions": decisions.len(),
        "concerns_covered": covered,
        "concerns_uncovered": uncovered,
        "stale_decisions": &stale,
        "patterns": patterns_count,
    });

    // Intent determines action and ready flag.
    let decision_count = decisions.len();
    let has_decisions = !decisions.is_empty();

    let (display_state, ready, action) = match intent {
        Intent::Implement => implement_action(
            component,
            computed,
            task,
            decision_count,
            &uncovered,
            &stale,
        ),
        Intent::Learn => learn_action(component, computed, task, has_decisions),
        Intent::Review => review_action(component, computed, task, has_decisions),
    };

    Ok(response(
        component,
        display_state,
        ready,
        assessment,
        action,
    ))
}

// ── Project scope ──────────────────────────────────────────────────────────

/// Simplified state machine for project-wide rules.
///
/// No concern coverage — project decisions are cross-cutting principles
/// that don't map to the 10 technical concern areas. No staleness check.
/// Two states: `undecided` (0 decisions) or `ready` (≥1).
fn advance_project(state: &ProjectState, intent: Intent, task: Option<&str>) -> Value {
    let graph = &state.graph;
    let decisions = graph.project_decisions();
    let has_decisions = !decisions.is_empty();

    let computed = if has_decisions {
        State::Ready
    } else {
        State::Undecided
    };

    let assessment = serde_json::json!({
        "decisions": decisions.len(),
        "concerns_covered": Value::Array(vec![]),
        "concerns_uncovered": Value::Array(vec![]),
        "stale_decisions": Value::Array(vec![]),
        "patterns": state.patterns.len(),
    });

    let (display_state, ready, action) = match intent {
        Intent::Implement => match computed {
            State::Ready => (
                State::Ready,
                true,
                serde_json::json!({
                    "tool": "get_context",
                    "args": { "component": "project", "task": task },
                    "instruction": "Project rules are established. \
                                    Call get_context for the brief.",
                }),
            ),
            _ => (
                State::Undecided,
                false,
                design_action(
                    "project",
                    "full",
                    task,
                    None,
                    "No project rules recorded. Run a full design session \
                     to establish cross-cutting principles.",
                ),
            ),
        },
        Intent::Learn => {
            let instruction = if has_decisions {
                "Present each project rule for understanding."
            } else {
                "No project rules recorded. The learn session should \
                 explore what principles guide this project."
            };
            (
                computed,
                false,
                design_action("project", "learn", task, None, instruction),
            )
        }
        Intent::Review => {
            if has_decisions {
                (
                    computed,
                    false,
                    design_action(
                        "project",
                        "review",
                        task,
                        None,
                        "Review project rules for drift.",
                    ),
                )
            } else {
                (
                    State::Undecided,
                    false,
                    design_action(
                        "project",
                        "full",
                        task,
                        None,
                        "No project rules to review. Run a full design session.",
                    ),
                )
            }
        }
    };

    response("project", display_state, ready, assessment, action)
}

// ── Intent action builders ─────────────────────────────────────────────────

/// Implement intent: full five-state machine.
fn implement_action(
    component: &str,
    computed: State,
    task: Option<&str>,
    decision_count: usize,
    uncovered: &[&str],
    stale: &[Value],
) -> (State, bool, Value) {
    match computed {
        State::Undecided => (
            State::Undecided,
            false,
            design_action(
                component,
                "full",
                task,
                None,
                "No decisions recorded. Run a full design session.",
            ),
        ),
        State::Incomplete => {
            let focus: Vec<&str> = uncovered
                .iter()
                .take(CONCERN_FOCUS_LIMIT)
                .copied()
                .collect();
            let instruction = format!(
                "{decision_count} decision(s) recorded but {} concern areas \
                 unexplored. Run a quick design session focusing on the \
                 uncovered areas.",
                uncovered.len(),
            );
            (
                State::Incomplete,
                false,
                design_action(component, "quick", task, Some(&focus), &instruction),
            )
        }
        State::Stale => {
            let stale_count = stale.len();
            (
                State::Stale,
                false,
                serde_json::json!({
                    "tool": "get_design_prompt",
                    "args": {
                        "component": component,
                        "mode": "review",
                        "task": task,
                    },
                    "stale_decisions": stale,
                    "instruction": format!(
                        "Coverage is adequate but {stale_count} decision(s) \
                         are older than {STALENESS_THRESHOLD_DAYS} days. \
                         Run a review session.",
                    ),
                }),
            )
        }
        State::Ready => (
            State::Ready,
            true,
            serde_json::json!({
                "tool": "get_context",
                "args": {
                    "component": component,
                    "task": task,
                },
                "instruction": "Component is designed and ready for \
                                implementation. Call get_context for the \
                                authoritative brief.",
            }),
        ),
        State::Unregistered => unreachable!("handled before state computation"),
    }
}

/// Learn intent: always routes to get_design_prompt(mode="learn").
/// No readiness gate — learning is always allowed.
fn learn_action(
    component: &str,
    computed: State,
    task: Option<&str>,
    has_decisions: bool,
) -> (State, bool, Value) {
    let instruction = if has_decisions {
        "Present each decision for understanding, then probe for \
         implicit decisions not yet captured."
    } else {
        "No decisions recorded. The learn session should probe for \
         implicit decisions embedded in the code."
    };
    (
        computed,
        false,
        design_action(component, "learn", task, None, instruction),
    )
}

/// Review intent: challenge existing decisions for drift.
fn review_action(
    component: &str,
    computed: State,
    task: Option<&str>,
    has_decisions: bool,
) -> (State, bool, Value) {
    if has_decisions {
        (
            computed,
            false,
            design_action(
                component,
                "review",
                task,
                None,
                "Review existing decisions for drift or staleness.",
            ),
        )
    } else {
        // Nothing to review — start fresh.
        (
            State::Undecided,
            false,
            design_action(
                component,
                "full",
                task,
                None,
                "No decisions to review. Run a full design session.",
            ),
        )
    }
}

// ── Response builders ──────────────────────────────────────────────────────

fn response(component: &str, state: State, ready: bool, assessment: Value, action: Value) -> Value {
    serde_json::json!({
        "component": component,
        "state": state.as_str(),
        "ready": ready,
        "assessment": assessment,
        "action": action,
    })
}

/// Build a `get_design_prompt` action object with optional focus.
fn design_action(
    component: &str,
    mode: &str,
    task: Option<&str>,
    focus: Option<&[&str]>,
    instruction: &str,
) -> Value {
    let mut action = serde_json::json!({
        "tool": "get_design_prompt",
        "args": {
            "component": component,
            "mode": mode,
            "task": task,
        },
        "instruction": instruction,
    });
    if let Some(f) = focus {
        action["focus"] = serde_json::json!(f);
    }
    action
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    // ── Test fixtures ──────────────────────────────────────────────────

    /// Build a ProjectState from component and decision descriptors.
    /// Automatically wires BelongsTo edges for each decision.
    fn build_state(
        components: &[(&str, &str)],
        decisions: &[(&str, DecisionFile)],
    ) -> ProjectState {
        let mut comp_map = BTreeMap::new();
        for &(name, desc) in components {
            comp_map.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: desc.into(),
                    },
                },
            );
        }

        let mut dec_map = BTreeMap::new();
        let mut nodes: Vec<NodeEntry> = components
            .iter()
            .map(|&(name, _)| NodeEntry {
                name: name.into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: String::new(),
            })
            .collect();
        let mut edges = Vec::new();

        for &(name, ref dec) in decisions {
            dec_map.insert(name.into(), dec.clone());
            nodes.push(NodeEntry {
                name: name.into(),
                kind: NodeKind::Decision,
                tags: dec.decision.tags.clone(),
                hash: String::new(),
            });
            edges.push(EdgeEntry {
                from: name.into(),
                to: dec.decision.component.clone(),
                kind: EdgeKind::BelongsTo,
            });
        }

        ProjectState::new(
            ProjectFile {
                trurl_version: FORMAT_VERSION.into(),
                project: Project {
                    name: "test".into(),
                    description: String::new(),
                },
            },
            comp_map,
            dec_map,
            BTreeMap::new(),
            GraphIndex {
                version: 1,
                rebuilt: Utc::now(),
                nodes,
                edges,
            },
        )
    }

    fn fresh_decision(component: &str, choice: &str, reason: &str, tags: &[&str]) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: choice.into(),
                reason: reason.into(),
                alternatives: vec![],
                tags: tags.iter().map(|t| (*t).into()).collect(),
                created: Utc::now(),
            },
        }
    }

    fn stale_decision(component: &str, choice: &str, reason: &str, tags: &[&str]) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: choice.into(),
                reason: reason.into(),
                alternatives: vec![],
                tags: tags.iter().map(|t| (*t).into()).collect(),
                created: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            },
        }
    }

    /// Six decisions covering 6 of 10 concerns → adequate coverage.
    fn well_covered_decisions(component: &str, fresh: bool) -> Vec<(&'static str, DecisionFile)> {
        let make = if fresh {
            fresh_decision
        } else {
            stale_decision
        };
        vec![
            (
                "d-security",
                make(component, "Auth tokens", "Token security", &["security"]),
            ),
            (
                "d-errors",
                make(component, "Fail-closed", "Error recovery", &["error"]),
            ),
            (
                "d-locking",
                make(
                    component,
                    "RwLock for state",
                    "Concurrent access",
                    &["lock"],
                ),
            ),
            (
                "d-integrity",
                make(component, "BLAKE3 hash validation", "Integrity check", &[]),
            ),
            (
                "d-perf",
                make(
                    component,
                    "In-memory cache",
                    "Performance target",
                    &["cache"],
                ),
            ),
            (
                "d-api",
                make(component, "REST API protocol", "External interface", &[]),
            ),
        ]
    }

    // ── Implement intent ───────────────────────────────────────────────

    #[test]
    fn implement_unregistered() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "rate-limiter", None, None).unwrap();

        assert_eq!(result["state"], "unregistered");
        assert_eq!(result["ready"], false);
        assert!(result["assessment"].is_null());
        assert_eq!(result["action"]["tool"], "add_component");
        assert_eq!(result["action"]["args"]["name"], "rate-limiter");
    }

    #[test]
    fn implement_unregistered_suggests_kebab() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "Rate Limiter", None, None).unwrap();

        assert_eq!(result["state"], "unregistered");
        assert_eq!(result["action"]["args"]["name"], "rate-limiter");
    }

    #[test]
    fn implement_undecided() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", None, None).unwrap();

        assert_eq!(result["state"], "undecided");
        assert_eq!(result["ready"], false);
        assert_eq!(result["assessment"]["decisions"], 0);
        assert_eq!(result["action"]["tool"], "get_design_prompt");
        assert_eq!(result["action"]["args"]["mode"], "full");
    }

    #[test]
    fn implement_incomplete() {
        // 1 decision covering 1 concern → 1 covered, 9 uncovered → incomplete.
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Human-readable", &["format"]),
            )],
        );
        let result = advance(&state, "store", None, None).unwrap();

        assert_eq!(result["state"], "incomplete");
        assert_eq!(result["ready"], false);
        assert_eq!(result["assessment"]["decisions"], 1);
        assert_eq!(result["action"]["tool"], "get_design_prompt");
        assert_eq!(result["action"]["args"]["mode"], "quick");

        // Focus present and limited to CONCERN_FOCUS_LIMIT.
        let focus = result["action"]["focus"].as_array().unwrap();
        assert_eq!(focus.len(), CONCERN_FOCUS_LIMIT);
    }

    #[test]
    fn implement_incomplete_focus_uses_priority_order() {
        // With 1 decision covering Data format (priority 8), the top
        // uncovered concerns by priority should be Security (1), Error (2),
        // Concurrency (3).
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Schema encoding", &[]),
            )],
        );
        let result = advance(&state, "store", None, None).unwrap();
        let focus = result["action"]["focus"].as_array().unwrap();

        assert_eq!(focus[0], "Security boundaries");
        assert_eq!(focus[1], "Error handling & failure modes");
        assert_eq!(focus[2], "Concurrency & locking");
    }

    #[test]
    fn implement_stale() {
        let decisions = well_covered_decisions("store", false);
        let state = build_state(&[("store", "Data store")], &decisions);
        let result = advance(&state, "store", None, None).unwrap();

        assert_eq!(result["state"], "stale");
        assert_eq!(result["ready"], false);
        assert_eq!(result["action"]["tool"], "get_design_prompt");
        assert_eq!(result["action"]["args"]["mode"], "review");

        let stale = result["action"]["stale_decisions"].as_array().unwrap();
        assert_eq!(stale.len(), decisions.len());
    }

    #[test]
    fn implement_ready() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(&state, "store", None, None).unwrap();

        assert_eq!(result["state"], "ready");
        assert_eq!(result["ready"], true);
        assert_eq!(result["action"]["tool"], "get_context");
        assert_eq!(result["action"]["args"]["component"], "store");
    }

    #[test]
    fn evaluation_order_coverage_before_staleness() {
        // 1 stale decision → both incomplete (9 > 1) AND stale.
        // Spec: coverage checked first → incomplete wins.
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                stale_decision("store", "TOML format", "Schema encoding", &[]),
            )],
        );
        let result = advance(&state, "store", None, None).unwrap();

        assert_eq!(result["state"], "incomplete");
        // Stale decisions still visible in assessment for transparency.
        let stale = result["assessment"]["stale_decisions"].as_array().unwrap();
        assert_eq!(stale.len(), 1);
    }

    // ── Project scope ──────────────────────────────────────────────────

    #[test]
    fn project_undecided() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "project", None, None).unwrap();

        assert_eq!(result["state"], "undecided");
        assert_eq!(result["ready"], false);
        assert_eq!(result["action"]["tool"], "get_design_prompt");
        assert_eq!(result["action"]["args"]["mode"], "full");
        // No concern coverage for project scope.
        assert!(
            result["assessment"]["concerns_covered"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            result["assessment"]["concerns_uncovered"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn project_ready() {
        let state = build_state(
            &[],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety first", &[]),
            )],
        );
        let result = advance(&state, "project", None, None).unwrap();

        assert_eq!(result["state"], "ready");
        assert_eq!(result["ready"], true);
        assert_eq!(result["action"]["tool"], "get_context");
        assert_eq!(result["assessment"]["decisions"], 1);
    }

    // ── Learn intent ───────────────────────────────────────────────────

    #[test]
    fn learn_always_routes_to_learn_mode() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        // Even a ready component → learn mode, not get_context.
        let result = advance(&state, "store", Some("learn"), None).unwrap();

        assert_eq!(result["action"]["tool"], "get_design_prompt");
        assert_eq!(result["action"]["args"]["mode"], "learn");
        assert_eq!(result["ready"], false);
        // Assessment still shows full health data.
        assert!(result["assessment"]["decisions"].as_u64().unwrap() > 0);
    }

    #[test]
    fn learn_empty_probes_implicit() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", Some("learn"), None).unwrap();

        assert_eq!(result["action"]["args"]["mode"], "learn");
        let instruction = result["action"]["instruction"].as_str().unwrap();
        assert!(
            instruction.contains("probe") || instruction.contains("implicit"),
            "empty learn should mention probing for implicit decisions"
        );
    }

    #[test]
    fn learn_unregistered_returns_unregistered() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "ghost", Some("learn"), None).unwrap();

        assert_eq!(result["state"], "unregistered");
        assert_eq!(result["action"]["tool"], "add_component");
    }

    // ── Review intent ──────────────────────────────────────────────────

    #[test]
    fn review_with_decisions_routes_to_review() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let result = advance(&state, "store", Some("review"), None).unwrap();

        assert_eq!(result["action"]["tool"], "get_design_prompt");
        assert_eq!(result["action"]["args"]["mode"], "review");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn review_empty_falls_to_full() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", Some("review"), None).unwrap();

        assert_eq!(result["state"], "undecided");
        assert_eq!(result["action"]["args"]["mode"], "full");
    }

    // ── Intent parsing ─────────────────────────────────────────────────

    #[test]
    fn invalid_intent_rejected() {
        let state = build_state(&[("store", "Data store")], &[]);
        let err = advance(&state, "store", Some("deploy"), None).unwrap_err();
        assert!(err.contains("invalid intent"));
    }

    #[test]
    fn default_intent_is_implement() {
        let state = build_state(&[("store", "Data store")], &[]);
        let with_none = advance(&state, "store", None, None).unwrap();
        let with_implement = advance(&state, "store", Some("implement"), None).unwrap();

        assert_eq!(with_none["state"], with_implement["state"]);
        assert_eq!(
            with_none["action"]["tool"],
            with_implement["action"]["tool"]
        );
    }

    // ── Task passthrough ───────────────────────────────────────────────

    #[test]
    fn task_passed_through_to_action() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", None, Some("add caching")).unwrap();

        assert_eq!(result["action"]["args"]["task"], "add caching");
    }

    #[test]
    fn task_passed_through_in_ready_state() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(&state, "store", None, Some("fix bug")).unwrap();

        assert_eq!(result["state"], "ready");
        assert_eq!(result["action"]["args"]["task"], "fix bug");
    }

    // ── Ready flag semantics ───────────────────────────────────────────

    #[test]
    fn ready_true_only_for_implement_ready() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );

        // Implement → ready=true.
        let r = advance(&state, "store", Some("implement"), None).unwrap();
        assert_eq!(r["ready"], true);

        // Learn → ready=false even when state is ready.
        let r = advance(&state, "store", Some("learn"), None).unwrap();
        assert_eq!(r["ready"], false);

        // Review → ready=false even when state is ready.
        let r = advance(&state, "store", Some("review"), None).unwrap();
        assert_eq!(r["ready"], false);
    }

    // ── Response shape ─────────────────────────────────────────────────

    #[test]
    fn response_has_all_required_fields() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let result = advance(&state, "store", None, None).unwrap();

        assert!(result.get("component").is_some());
        assert!(result.get("state").is_some());
        assert!(result.get("ready").is_some());
        assert!(result.get("assessment").is_some());
        assert!(result.get("action").is_some());
        assert!(result["action"].get("tool").is_some());
        assert!(result["action"].get("args").is_some());
        assert!(result["action"].get("instruction").is_some());
    }

    #[test]
    fn focus_only_present_in_incomplete() {
        // Incomplete → has focus.
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let r = advance(&state, "store", None, None).unwrap();
        assert_eq!(r["state"], "incomplete");
        assert!(r["action"].get("focus").is_some());

        // Ready → no focus.
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let r = advance(&state, "store", None, None).unwrap();
        assert_eq!(r["state"], "ready");
        assert!(r["action"].get("focus").is_none());
    }

    // ── Idempotency ────────────────────────────────────────────────────

    #[test]
    fn advance_is_idempotent() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let a = advance(&state, "store", None, None).unwrap();
        let b = advance(&state, "store", None, None).unwrap();
        assert_eq!(a, b);
    }

    // ── Coverage boundary ──────────────────────────────────────────────

    #[test]
    fn coverage_boundary_equal_is_not_incomplete() {
        // 5 covered, 5 uncovered → 5 is not > 5 → not incomplete.
        let state = build_state(
            &[("store", "Data store")],
            &[
                (
                    "d-security",
                    fresh_decision("store", "Auth tokens", "Security", &["security"]),
                ),
                (
                    "d-errors",
                    fresh_decision("store", "Fail-closed", "Error recovery", &["error"]),
                ),
                (
                    "d-locking",
                    fresh_decision("store", "Mutex for state", "Locking strategy", &["lock"]),
                ),
                (
                    "d-integrity",
                    fresh_decision("store", "BLAKE3 hash", "Integrity check", &[]),
                ),
                (
                    "d-perf",
                    fresh_decision("store", "In-memory cache", "Performance", &["cache"]),
                ),
            ],
        );
        let result = advance(&state, "store", None, None).unwrap();

        // 5 covered, 5 uncovered → not incomplete.
        assert_ne!(result["state"], "incomplete");
    }
}
