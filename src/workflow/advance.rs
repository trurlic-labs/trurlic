//! Deterministic step-deduction state machine for workflow orchestration.
//!
//! The `advance` function computes the next workflow step from the knowledge
//! graph and returns a single focused action. Read-only, stateless, and
//! idempotent — no writes, no locks, no LLM calls.
//!
//! Called N times with the same graph state, it returns the same `(step, prompt)`
//! every time. The function is a pure projection from graph contents to
//! workflow step.

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use crate::store::ProjectState;
use crate::store::schema::DecisionFile;

use super::concerns;
use super::{CONCERN_FOCUS_LIMIT, STALENESS_THRESHOLD_DAYS, Step, TaskType};

// ── Observability ─────────────────────────────────────────────────────────

/// Runtime debug flag. Checked once on first call (zero-cost after init).
/// Enable via `TRURLIC_DEBUG=1` environment variable.
fn is_debug() -> bool {
    static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEBUG.get_or_init(|| std::env::var_os("TRURLIC_DEBUG").is_some())
}

// ── Public API ────────────────────────────────────────────────────────────

/// Compute the next workflow step for a component and return a structured
/// action.
///
/// Read-only. No writes. No locks. No LLM calls. Computes step from the
/// in-memory graph on every call — no session tracking, no persistent
/// workflow state.
pub fn advance(
    state: &ProjectState,
    component: &str,
    task_type: Option<TaskType>,
    task: Option<&str>,
    completed_steps: &[&str],
) -> Result<Value, String> {
    // Project scope: simplified state machine.
    if component == "project" {
        return Ok(advance_project(state, task_type, task, completed_steps));
    }

    // Unregistered: component not in graph.
    if !state.components.contains_key(component) {
        let suggested = crate::store::slugify(component);
        return Ok(build_response(
            component,
            TaskType::NewComponent,
            &Step::Register,
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

    // ── Compute state from graph ──────────────────────────────────────

    let graph = state.graph();
    let decisions = graph.decisions_for(component);
    let project_rules = graph.project_decisions();

    let all_decs: Vec<&DecisionFile> = project_rules
        .iter()
        .chain(decisions.iter())
        .map(|(_, d)| *d)
        .collect();
    let (covered, uncovered) = concerns::compute_concern_coverage(&all_decs);

    let now = Utc::now();
    let stale: Vec<StaleDec> = decisions
        .iter()
        .filter_map(|(name, d)| {
            let age_days = now.signed_duration_since(d.decision.created).num_days();
            (age_days >= STALENESS_THRESHOLD_DAYS).then(|| StaleDec {
                name: Arc::clone(name),
                created: d.decision.created.to_rfc3339(),
                age_days,
            })
        })
        .collect();

    let patterns = graph.patterns_for(component);

    // ── Infer task type if not provided ───────────────────────────────

    let task_type = match task_type {
        Some(tt) => tt,
        None => match infer_task_type(&decisions, task, &covered, &uncovered, &stale) {
            Some(tt) => tt,
            None => {
                // Fully designed, no task → Ready.
                return Ok(ready_response(
                    component, &decisions, &covered, &uncovered, &stale, &patterns, task,
                ));
            }
        },
    };

    // ── Deduce step ───────────────────────────────────────────────────

    let step = deduce_step(
        task_type,
        &decisions,
        &covered,
        &uncovered,
        &stale,
        &patterns,
        graph,
        component,
        task,
        completed_steps,
    );

    if is_debug() {
        eprintln!(
            "trurlic: advance {component} → {} (type={}, decisions={}, coverage={}/{}, stale={}, patterns={})",
            step.as_str(),
            task_type.as_str(),
            decisions.len(),
            covered.len(),
            covered.len() + uncovered.len(),
            stale.len(),
            patterns.len(),
        );
    }

    let ready = matches!(step, Step::Ready);
    let assessment = build_assessment(&decisions, &covered, &uncovered, &stale, &patterns);
    let action = step_action(component, &step, task);

    Ok(build_response(
        component, task_type, &step, ready, assessment, action,
    ))
}

// ── Task type inference ───────────────────────────────────────────────────

/// Infer the task type from graph state when not explicitly provided.
///
/// Returns `None` when the component is fully designed and no task is
/// specified — the caller should return `Ready` directly.
fn infer_task_type(
    decisions: &[(&Arc<str>, &DecisionFile)],
    task: Option<&str>,
    covered: &[&str],
    uncovered: &[&str],
    stale: &[StaleDec],
) -> Option<TaskType> {
    if decisions.is_empty() {
        return if task.is_some() {
            Some(TaskType::NewComponent)
        } else {
            Some(TaskType::Learn)
        };
    }

    // Decisions exist — check health.
    if uncovered.len() > covered.len() {
        return Some(TaskType::Harden);
    }
    if !stale.is_empty() {
        return Some(TaskType::Review);
    }

    // Healthy and covered. If there's a task, assume Feature; otherwise
    // the component is ready and no workflow is needed.
    if task.is_some() {
        Some(TaskType::Feature)
    } else {
        None // → Ready
    }
}

// ── Step deduction ────────────────────────────────────────────────────────

/// Deduce the next step from graph state for a given task type.
///
/// Each task type defines a sequence of steps. The state machine walks
/// the sequence by checking postconditions. Some postconditions are
/// heuristic (e.g. `PatternDetection` — checked via pattern count).
/// The machine errs on the side of returning `Ready` rather than looping.
#[allow(clippy::too_many_arguments)]
fn deduce_step(
    task_type: TaskType,
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    stale: &[StaleDec],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    graph: &crate::store::graph::InMemoryGraph,
    component: &str,
    task: Option<&str>,
    completed: &[&str],
) -> Step {
    match task_type {
        TaskType::NewComponent => {
            deduce_new_component(decisions, covered, uncovered, patterns, completed)
        }
        TaskType::Feature => {
            deduce_feature(decisions, covered, uncovered, patterns, task, completed)
        }
        TaskType::Fix => deduce_fix(decisions, uncovered, graph, component, task, completed),
        TaskType::Learn => deduce_learn(decisions, patterns),
        TaskType::Review => {
            deduce_review(decisions, stale, covered, uncovered, patterns, completed)
        }
        TaskType::Harden => deduce_harden(uncovered, patterns, completed),
        TaskType::Bootstrap => deduce_bootstrap_component(decisions, patterns, component),
    }
}

/// NewComponent: Register → DefineScope → CoverConcerns → PatternDetection → SummaryGate → Ready
fn deduce_new_component(
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    completed: &[&str],
) -> Step {
    if !has_scope_decision(decisions) {
        return Step::DefineScope;
    }
    if uncovered.len() > covered.len() {
        return Step::CoverConcerns {
            focus: top_n(uncovered, CONCERN_FOCUS_LIMIT),
        };
    }
    if patterns.is_empty() {
        return Step::PatternDetection;
    }
    if !completed.contains(&"summary_gate") {
        return Step::SummaryGate;
    }
    Step::Ready
}

/// Feature: VerifyConstraints → CoverConcerns(focused) → PatternDetection → Ready
///
/// VerifyConstraints: ensures existing decisions still hold for the new
/// feature. Has no verifiable graph postcondition — uses `completed_steps`
/// for progression. Often produces `update_decision` calls that change the
/// graph naturally.
///
/// CoverConcerns: when a task is provided, filter uncovered concerns by
/// keyword relevance to the task description (e.g. "add caching" matches
/// "Performance constraints" via the "cache" keyword). If no task or no
/// keyword matches, fall back to the majority threshold (uncovered > covered).
///
/// PatternDetection: after concerns are covered, look for patterns across
/// decisions. Has a verifiable postcondition (patterns recorded).
fn deduce_feature(
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    task: Option<&str>,
    completed: &[&str],
) -> Step {
    // VerifyConstraints: existing decisions need checking against the task.
    // Skipped only when no decisions exist (nothing to verify) or when
    // the caller signals the step was already completed.
    if !decisions.is_empty() && !completed.contains(&"verify_constraints") {
        return Step::VerifyConstraints;
    }

    // CoverConcerns: focused on task-relevant gaps.
    if let Some(t) = task {
        let relevant = task_relevant_concerns(uncovered, t);
        if !relevant.is_empty() {
            return Step::CoverConcerns {
                focus: top_n(&relevant, CONCERN_FOCUS_LIMIT),
            };
        }
    }

    // Fallback: majority threshold.
    if uncovered.len() > covered.len() {
        return Step::CoverConcerns {
            focus: top_n(uncovered, CONCERN_FOCUS_LIMIT),
        };
    }

    // PatternDetection: after concerns are covered.
    if patterns.is_empty() && !decisions.is_empty() && !completed.contains(&"pattern_detection") {
        return Step::PatternDetection;
    }

    Step::Ready
}

/// Filter uncovered concern names to those whose keywords overlap with
/// words in the task description. O(concerns × keywords × task_words),
/// all bounded by small constants — no allocations beyond the result vec.
fn task_relevant_concerns<'a>(uncovered: &[&'a str], task: &str) -> Vec<&'a str> {
    let task_words: Vec<String> = task
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .collect();

    if task_words.is_empty() {
        return Vec::new();
    }

    uncovered
        .iter()
        .filter(|concern_name| {
            concerns::CONCERNS
                .iter()
                .find(|(name, _)| name == *concern_name)
                .is_some_and(|(_, keywords)| {
                    keywords
                        .iter()
                        .any(|kw| task_words.iter().any(|tw| tw == kw))
                })
        })
        .copied()
        .collect()
}

/// Fix: VerifyConstraints → [CoverConcerns] → ImpactCheck → Ready
///
/// VerifyConstraints ensures the fix doesn't break existing decisions.
/// Skipped when no decisions exist (nothing to verify).
///
/// CoverConcerns: conditional — fires only when the component has zero
/// decisions AND the task description matches uncovered concern keywords.
/// A bug fix in an undesigned concern area should not proceed without at
/// least one recorded decision constraining it.
///
/// ImpactCheck: cross-component effects. Fires regardless of decision
/// count — a fix on an undesigned component with connections still needs
/// cross-component checking. Previously skipped when decisions were empty;
/// now reachable.
///
/// VerifyConstraints and ImpactCheck lack verifiable graph postconditions —
/// `completed_steps` handles progression.
fn deduce_fix(
    decisions: &[(&Arc<str>, &DecisionFile)],
    uncovered: &[&str],
    graph: &crate::store::graph::InMemoryGraph,
    component: &str,
    task: Option<&str>,
    completed: &[&str],
) -> Step {
    // VerifyConstraints: existing decisions need checking against the fix.
    if !decisions.is_empty() && !completed.contains(&"verify_constraints") {
        return Step::VerifyConstraints;
    }

    // CoverConcerns: when no decisions exist and the task touches an
    // uncovered concern area, force a focused design step before coding.
    // This prevents unguarded fixes in concern areas that have never been
    // designed (e.g. fixing a concurrency bug with zero concurrency
    // decisions on record).
    if decisions.is_empty()
        && let Some(t) = task
    {
        let relevant = task_relevant_concerns(uncovered, t);
        if !relevant.is_empty() {
            return Step::CoverConcerns {
                focus: top_n(&relevant, CONCERN_FOCUS_LIMIT),
            };
        }
    }

    // ImpactCheck: cross-component effects.
    let has_connections =
        !graph.connects_to(component).is_empty() || !graph.connects_from(component).is_empty();
    if has_connections && !completed.contains(&"impact_check") {
        return Step::ImpactCheck;
    }

    Step::Ready
}

/// Learn: AnalyzeCode → WalkDecisions → PatternDetection → Ready
///
/// AnalyzeCode postcondition: decisions recorded (verifiable).
/// WalkDecisions postcondition: heuristic (patterns serve as proxy —
/// if patterns exist, walkthrough + pattern detection are complete).
fn deduce_learn(
    decisions: &[(&Arc<str>, &DecisionFile)],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
) -> Step {
    if decisions.is_empty() {
        return Step::AnalyzeCode;
    }
    if patterns.is_empty() {
        return Step::WalkDecisions;
    }
    Step::Ready
}

/// Review: WalkDecisions → DriftCheck → CoverageAudit → PatternDetection → Ready
///
/// WalkDecisions: interactive walkthrough of all decisions, challenging
/// each against current source code. No verifiable graph postcondition,
/// but typically produces updated decisions and/or patterns. Uses
/// `completed_steps` for progression.
///
/// DriftCheck: systematic verification for stale decisions — agent
/// updates timestamps by calling `update_decision`, advancing the
/// state machine past this step naturally.
///
/// CoverageAudit: surfaces coverage gaps. Agent may record intentional-
/// gap decisions (graph change) or note gaps for future work.
fn deduce_review(
    decisions: &[(&Arc<str>, &DecisionFile)],
    stale: &[StaleDec],
    covered: &[&str],
    uncovered: &[&str],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    completed: &[&str],
) -> Step {
    // WalkDecisions: review starts with interactive walkthrough.
    if !decisions.is_empty() && !completed.contains(&"walk_decisions") {
        return Step::WalkDecisions;
    }
    // DriftCheck: stale decisions need verification against current code.
    if !stale.is_empty() {
        return Step::DriftCheck;
    }
    // CoverageAudit: check for coverage gaps.
    if uncovered.len() > covered.len() {
        return Step::CoverageAudit;
    }
    // PatternDetection: find patterns across fresh decisions.
    if patterns.is_empty() && !completed.contains(&"pattern_detection") {
        return Step::PatternDetection;
    }
    Step::Ready
}

/// Harden: CoverageAudit → CoverConcerns(gaps) → PatternDetection → Ready
///
/// CoverageAudit is the entry step: present the gap landscape to the user
/// so they can decide which gaps are intentional before filling real ones.
/// No verifiable graph postcondition — uses `completed_steps`.
///
/// CoverConcerns fills the real gaps with recorded decisions (verifiable).
/// PatternDetection is the final pass.
fn deduce_harden(
    uncovered: &[&str],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    completed: &[&str],
) -> Step {
    // CoverageAudit: entry step, assess gaps before filling.
    if !uncovered.is_empty() && !completed.contains(&"coverage_audit") {
        return Step::CoverageAudit;
    }
    // CoverConcerns: fill the real gaps.
    if !uncovered.is_empty() {
        return Step::CoverConcerns {
            focus: top_n(uncovered, CONCERN_FOCUS_LIMIT),
        };
    }
    // PatternDetection: final pass.
    if patterns.is_empty() && !completed.contains(&"pattern_detection") {
        return Step::PatternDetection;
    }
    Step::Ready
}

/// Bootstrap (component): ExtractDecisions → PatternDetection → Ready
///
/// Autonomous extraction for a single component. The agent reads source
/// code and records decisions without interactive dialogue. Unlike
/// `deduce_learn`, the step prompts omit the Socratic interaction
/// protocol.
fn deduce_bootstrap_component(
    decisions: &[(&Arc<str>, &DecisionFile)],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    component: &str,
) -> Step {
    if decisions.is_empty() {
        return Step::ExtractDecisions {
            component: component.into(),
        };
    }
    if patterns.is_empty() {
        return Step::PatternDetection;
    }
    Step::Ready
}

// ── Project scope ─────────────────────────────────────────────────────────

/// Simplified state machine for project-wide rules.
///
/// No concern coverage — project decisions are cross-cutting principles
/// that don't map to the 10 technical concern areas. Patterns serve as
/// the progression signal: once decisions are recorded AND patterns are
/// identified, the project is ready.
fn advance_project(
    state: &ProjectState,
    task_type: Option<TaskType>,
    task: Option<&str>,
    completed_steps: &[&str],
) -> Value {
    let graph = state.graph();
    let decisions = graph.project_decisions();
    let has_decisions = !decisions.is_empty();
    let has_patterns = !state.patterns.is_empty();

    let task_type = task_type.unwrap_or(if has_decisions {
        if task.is_some() {
            TaskType::Feature
        } else if has_patterns {
            // Decisions + patterns → fully learned. Default to ready.
            TaskType::Feature // Feature with no uncovered → Ready
        } else {
            TaskType::Learn // Has decisions but no patterns → still learning
        }
    } else {
        TaskType::NewComponent
    });

    let assessment = serde_json::json!({
        "decisions": decisions.len(),
        "concerns_covered": Value::Array(vec![]),
        "concerns_uncovered": Value::Array(vec![]),
        "stale_decisions": Value::Array(vec![]),
        "patterns": state.patterns.len(),
    });

    let ready_action = serde_json::json!({
        "tool": "get_context",
        "args": { "component": "project", "task": task },
        "instruction": "Project rules are established. \
                        Call get_context for the brief.",
    });

    let (step, ready, action) = match task_type {
        TaskType::NewComponent | TaskType::Harden => {
            if has_decisions {
                (Step::Ready, true, ready_action)
            } else {
                (
                    Step::DefineScope,
                    false,
                    step_prompt_action(
                        "project",
                        "define_scope",
                        task,
                        "No project rules recorded. Run a design session \
                         to establish cross-cutting principles.",
                    ),
                )
            }
        }
        TaskType::Feature | TaskType::Fix => {
            if has_decisions {
                (Step::Ready, true, ready_action)
            } else {
                (
                    Step::DefineScope,
                    false,
                    step_prompt_action(
                        "project",
                        "define_scope",
                        task,
                        "No project rules recorded. Establish cross-cutting \
                         principles before proceeding.",
                    ),
                )
            }
        }
        TaskType::Learn => {
            if !has_decisions {
                (
                    Step::WalkDecisions,
                    false,
                    step_prompt_action(
                        "project",
                        "walk_decisions",
                        task,
                        "No project rules recorded. The learn session should \
                         explore what principles guide this project.",
                    ),
                )
            } else if !has_patterns && !completed_steps.contains(&"walk_decisions") {
                (
                    Step::WalkDecisions,
                    false,
                    step_prompt_action(
                        "project",
                        "walk_decisions",
                        task,
                        "Present each project rule for understanding. \
                         After all are walked, identify patterns.",
                    ),
                )
            } else {
                (Step::Ready, true, ready_action)
            }
        }
        TaskType::Review => {
            if !has_decisions {
                (
                    Step::DefineScope,
                    false,
                    step_prompt_action(
                        "project",
                        "define_scope",
                        task,
                        "No project rules to review. Run a design session.",
                    ),
                )
            } else if !has_patterns && !completed_steps.contains(&"drift_check") {
                (
                    Step::DriftCheck,
                    false,
                    step_prompt_action(
                        "project",
                        "drift_check",
                        task,
                        "Review project rules for drift. After review, \
                         identify patterns across decisions.",
                    ),
                )
            } else {
                (Step::Ready, true, ready_action)
            }
        }
        TaskType::Bootstrap => deduce_bootstrap_project(state, task, completed_steps),
    };

    if is_debug() {
        eprintln!(
            "trurlic: project → {} (type={}, decisions={}, patterns={})",
            step.as_str(),
            task_type.as_str(),
            decisions.len(),
            state.patterns.len(),
        );
    }

    build_response("project", task_type, &step, ready, assessment, action)
}

// ── Bootstrap (project scope) ─────────────────────────────────────────────

/// Autonomous project-wide bootstrap.
///
/// Sequences through four phases, each with a graph-verifiable
/// postcondition. The `extract_decisions` phase cycles through
/// components: each `advance` call returns the first component
/// (alphabetical, via `BTreeMap` iteration order) with zero decisions,
/// ensuring deterministic, idempotent behaviour.
///
/// Returns `(Step, ready, action)` for assembly into the project
/// advance response.
fn deduce_bootstrap_project(
    state: &ProjectState,
    task: Option<&str>,
    completed: &[&str],
) -> (Step, bool, Value) {
    let graph = state.graph();

    // Phase 1: no components registered → scan the project.
    if state.components.is_empty() {
        return (
            Step::ScanProject,
            false,
            step_prompt_action(
                "project",
                "scan_project",
                task,
                "Read the project structure, identify major components, \
                 and register them with add_component and add_connection.",
            ),
        );
    }

    // Phase 2: find the first component with zero decisions.
    // BTreeMap iterates in sorted order → deterministic selection.
    for name in state.components.keys() {
        if graph.decisions_for(name).is_empty() {
            let mut action = step_prompt_action(
                name,
                "extract_decisions",
                task,
                &format!(
                    "Read every source file in [{name}] and record \
                     architectural decisions autonomously."
                ),
            );
            action["target_component"] = serde_json::json!(name);
            return (
                Step::ExtractDecisions {
                    component: name.clone(),
                },
                false,
                action,
            );
        }
    }

    // Phase 3: no project-level rules → record them.
    if graph.project_decisions().is_empty() && !completed.contains(&"project_rules") {
        return (
            Step::ProjectRules,
            false,
            step_prompt_action(
                "project",
                "project_rules",
                task,
                "Identify cross-cutting project-level decisions and \
                 record them with component='project'.",
            ),
        );
    }

    // Phase 4: no patterns → detect them.
    if state.patterns.is_empty() && !completed.contains(&"pattern_detection") {
        return (
            Step::PatternDetection,
            false,
            step_prompt_action(
                "project",
                "pattern_detection",
                task,
                "Review all recorded decisions across components. \
                 Identify patterns and call record_pattern.",
            ),
        );
    }

    // All phases complete.
    (
        Step::Ready,
        true,
        serde_json::json!({
            "tool": "get_context",
            "args": { "component": "project", "task": task },
            "instruction": "Bootstrap complete. Call get_context for \
                            the architectural brief.",
        }),
    )
}

// ── Step → action mapping ─────────────────────────────────────────────────

/// Map a deduced step to a concrete tool action the agent should execute.
fn step_action(component: &str, step: &Step, task: Option<&str>) -> Value {
    match step {
        Step::Register => unreachable!("handled before step deduction"),

        Step::DefineScope => step_prompt_action(
            component,
            "define_scope",
            task,
            "Define what the component is and isn't responsible for. \
             Record each answer as a decision with tags: [\"scope\"].",
        ),

        Step::AnalyzeCode => step_prompt_action(
            component,
            "analyze_code",
            task,
            "Read every source file in this component. Build a numbered \
             list of all architectural decisions you identify. Present \
             the list, then walk through each one.",
        ),

        Step::CoverConcerns { focus } => {
            let mut action = step_prompt_action(
                component,
                "cover_concerns",
                task,
                &format!(
                    "Cover uncovered concern areas: {}. For each, present \
                     2-3 viable options with trade-offs, ask the user to \
                     choose, and record with matching tags.",
                    focus.join(", "),
                ),
            );
            action["focus"] = serde_json::json!(focus);
            action
        }

        Step::WalkDecisions => step_prompt_action(
            component,
            "walk_decisions",
            task,
            "Walk through each recorded decision with the user. Present \
             one per message. After each, STOP and wait for the user's \
             response. Then identify patterns across decisions.",
        ),

        Step::VerifyConstraints => step_prompt_action(
            component,
            "verify_constraints",
            task,
            "Present each existing constraint that the task may affect. \
             For each, ask: \"Does your change respect this constraint, \
             violate it, or require changing it?\" STOP and wait. If \
             any constraint needs changing, call update_decision. Also \
             check whether this change impacts connected components.",
        ),

        Step::ImpactCheck => step_prompt_action(
            component,
            "impact_check",
            task,
            "Check whether this change impacts connected components. \
             Review the architecture brief for cross-component effects.",
        ),

        Step::PatternDetection => step_prompt_action(
            component,
            "pattern_detection",
            task,
            "Review all recorded decisions for this component and project \
             rules. Look for groups of 2+ decisions that reinforce the \
             same invariant, form a defense-in-depth chain, or share a \
             common constraint. For each candidate, ask the user to \
             confirm, then call record_pattern.",
        ),

        Step::SummaryGate => step_prompt_action(
            component,
            "summary_gate",
            task,
            "Ask the user: \"Without looking at the list, describe in \
             3-5 sentences the constraints any code touching this \
             component must respect.\" Do NOT help, hint, or break it \
             into sub-questions. If the user cannot produce a coherent \
             summary, revisit the decisions they couldn't explain.",
        ),

        Step::DriftCheck => step_prompt_action(
            component,
            "drift_check",
            task,
            "Compare each recorded decision against the current source \
             code. Flag any that have drifted from the implementation. \
             For drifted decisions, call update_decision(supersede).",
        ),

        Step::CoverageAudit => step_prompt_action(
            component,
            "coverage_audit",
            task,
            "Audit concern coverage. The assessment shows which areas \
             lack decisions. For each gap, determine whether the \
             component needs a decision there or if the gap is \
             intentional.",
        ),

        // Bootstrap steps are handled in advance_project, never reached
        // from the per-component path. Present for match exhaustiveness.
        Step::ScanProject | Step::ProjectRules => {
            unreachable!("project-scope bootstrap steps handled in advance_project")
        }

        Step::ExtractDecisions { .. } => step_prompt_action(
            component,
            "extract_decisions",
            task,
            &format!(
                "Read every source file in [{component}] and record \
                 architectural decisions autonomously."
            ),
        ),

        Step::Ready => serde_json::json!({
            "tool": "get_context",
            "args": { "component": component, "task": task },
            "instruction": "Component is designed and ready for \
                            implementation. Call get_context for the \
                            authoritative brief.",
        }),
    }
}

// ── Response builders ─────────────────────────────────────────────────────

fn build_response(
    component: &str,
    task_type: TaskType,
    step: &Step,
    ready: bool,
    assessment: Value,
    action: Value,
) -> Value {
    serde_json::json!({
        "component": component,
        "task_type": task_type.as_str(),
        "step": step.as_str(),
        "ready": ready,
        "assessment": assessment,
        "action": action,
    })
}

fn build_assessment(
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    stale: &[StaleDec],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
) -> Value {
    let stale_json: Vec<Value> = stale
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name.as_ref(),
                "created": &s.created,
                "age_days": s.age_days,
            })
        })
        .collect();

    serde_json::json!({
        "decisions": decisions.len(),
        "concerns_covered": covered,
        "concerns_uncovered": uncovered,
        "stale_decisions": stale_json,
        "patterns": patterns.len(),
    })
}

fn ready_response(
    component: &str,
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    stale: &[StaleDec],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    task: Option<&str>,
) -> Value {
    // Inferred as Ready — pick the most useful task type for display.
    let display_type = if decisions.is_empty() {
        TaskType::NewComponent
    } else {
        TaskType::Feature
    };
    let assessment = build_assessment(decisions, covered, uncovered, stale, patterns);
    build_response(
        component,
        display_type,
        &Step::Ready,
        true,
        assessment,
        serde_json::json!({
            "tool": "get_context",
            "args": { "component": component, "task": task },
            "instruction": "Component is designed and ready for \
                            implementation. Call get_context for the \
                            authoritative brief.",
        }),
    )
}

/// Build a `get_step_prompt` action.
fn step_prompt_action(component: &str, step: &str, task: Option<&str>, instruction: &str) -> Value {
    serde_json::json!({
        "tool": "get_step_prompt",
        "args": {
            "component": component,
            "step": step,
            "task": task,
        },
        "instruction": instruction,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Check whether any decision carries a "scope" tag.
fn has_scope_decision(decisions: &[(&Arc<str>, &DecisionFile)]) -> bool {
    decisions
        .iter()
        .any(|(_, d)| d.decision.tags.iter().any(|t| t == "scope"))
}

/// Select the top N concerns by priority (array order).
fn top_n(concerns: &[&str], n: usize) -> Vec<String> {
    concerns.iter().take(n).map(|s| (*s).to_string()).collect()
}

/// Internal representation of a stale decision.
struct StaleDec {
    name: Arc<str>,
    created: String,
    age_days: i64,
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    // ── Fixtures ──────────────────────────────────────────────────────

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
                trurlic_version: FORMAT_VERSION.into(),
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

    fn build_state_with_patterns(
        components: &[(&str, &str)],
        decisions: &[(&str, DecisionFile)],
        patterns: &[(&str, &str, &[&str])], // (name, description, applies_to_components)
    ) -> ProjectState {
        let mut state = build_state(components, decisions);
        for &(name, desc, applies_to) in patterns {
            state.patterns.insert(
                name.into(),
                PatternFile {
                    pattern: Pattern {
                        name: name.into(),
                        description: desc.into(),
                    },
                },
            );
            state.graph_index.nodes.push(NodeEntry {
                name: name.into(),
                kind: NodeKind::Pattern,
                tags: vec![],
                hash: String::new(),
            });
            for comp in applies_to {
                state.graph_index.edges.push(EdgeEntry {
                    from: name.into(),
                    to: (*comp).into(),
                    kind: EdgeKind::AppliesTo,
                });
            }
        }
        state.rebuild_graph();
        state
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

    /// Ten decisions covering all 10 concerns → full coverage.
    fn fully_covered_decisions(component: &str, fresh: bool) -> Vec<(&'static str, DecisionFile)> {
        let mut decs = well_covered_decisions(component, fresh);
        let make = if fresh {
            fresh_decision
        } else {
            stale_decision
        };
        decs.extend([
            (
                "d-storage",
                make(
                    component,
                    "File-per-entity",
                    "Storage isolation",
                    &["storage"],
                ),
            ),
            (
                "d-format",
                make(
                    component,
                    "TOML serialization",
                    "Human-readable format",
                    &["format"],
                ),
            ),
            (
                "d-deps",
                make(
                    component,
                    "Minimal dependencies",
                    "Supply chain risk",
                    &["dependency"],
                ),
            ),
            (
                "d-migration",
                make(
                    component,
                    "Semver schema versions",
                    "Migration path",
                    &["version"],
                ),
            ),
        ]);
        decs
    }

    // ── Unregistered ──────────────────────────────────────────────────

    #[test]
    fn unregistered_returns_register_step() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "rate-limiter", None, None, &[]).unwrap();

        assert_eq!(result["step"], "register");
        assert_eq!(result["task_type"], "new_component");
        assert_eq!(result["ready"], false);
        assert_eq!(result["action"]["tool"], "add_component");
        assert_eq!(result["action"]["args"]["name"], "rate-limiter");
    }

    #[test]
    fn unregistered_suggests_kebab() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "Rate Limiter", None, None, &[]).unwrap();

        assert_eq!(result["step"], "register");
        assert_eq!(result["action"]["args"]["name"], "rate-limiter");
    }

    // ── TaskType inference ────────────────────────────────────────────

    #[test]
    fn infer_learn_when_empty_no_task() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", None, None, &[]).unwrap();

        assert_eq!(result["task_type"], "learn");
        assert_eq!(result["step"], "analyze_code");
    }

    #[test]
    fn infer_new_component_when_empty_with_task() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", None, Some("build data layer"), &[]).unwrap();

        assert_eq!(result["task_type"], "new_component");
        assert_eq!(result["step"], "define_scope");
    }

    #[test]
    fn infer_harden_when_undercovered() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let result = advance(&state, "store", None, None, &[]).unwrap();

        assert_eq!(result["task_type"], "harden");
        assert_eq!(result["step"], "coverage_audit");
    }

    #[test]
    fn infer_review_when_stale() {
        let decisions = well_covered_decisions("store", false);
        let state = build_state(&[("store", "Data store")], &decisions);
        let result = advance(&state, "store", None, None, &[]).unwrap();

        assert_eq!(result["task_type"], "review");
        assert_eq!(result["step"], "walk_decisions");
    }

    #[test]
    fn infer_feature_when_healthy_with_task() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(&state, "store", None, Some("add caching"), &[]).unwrap();

        assert_eq!(result["task_type"], "feature");
    }

    #[test]
    fn infer_ready_when_healthy_no_task() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(&state, "store", None, None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── NewComponent step sequence ────────────────────────────────────

    #[test]
    fn new_component_starts_with_define_scope() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", Some(TaskType::NewComponent), None, &[]).unwrap();

        assert_eq!(result["step"], "define_scope");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn new_component_scope_defined_moves_to_cover_concerns() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d-scope",
                fresh_decision("store", "Data layer", "Scope", &["scope"]),
            )],
        );
        let result = advance(&state, "store", Some(TaskType::NewComponent), None, &[]).unwrap();

        assert_eq!(result["step"], "cover_concerns");
        let focus = result["action"]["focus"].as_array().unwrap();
        assert_eq!(focus.len(), CONCERN_FOCUS_LIMIT);
    }

    #[test]
    fn new_component_covered_moves_to_pattern_detection() {
        let mut decs = well_covered_decisions("store", true);
        decs.push((
            "d-scope",
            fresh_decision("store", "Data layer", "Scope", &["scope"]),
        ));
        let state = build_state(&[("store", "Data store")], &decs);
        let result = advance(&state, "store", Some(TaskType::NewComponent), None, &[]).unwrap();

        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn new_component_with_patterns_moves_to_summary_gate() {
        let mut decs = well_covered_decisions("store", true);
        decs.push((
            "d-scope",
            fresh_decision("store", "Data layer", "Scope", &["scope"]),
        ));
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &decs,
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(&state, "store", Some(TaskType::NewComponent), None, &[]).unwrap();

        assert_eq!(result["step"], "summary_gate");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn new_component_after_summary_gate_is_ready() {
        let mut decs = well_covered_decisions("store", true);
        decs.push((
            "d-scope",
            fresh_decision("store", "Data layer", "Scope", &["scope"]),
        ));
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &decs,
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            &["summary_gate"],
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Feature step sequence ─────────────────────────────────────────

    #[test]
    fn feature_with_gaps_verifies_constraints_first() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let result = advance(&state, "store", Some(TaskType::Feature), None, &[]).unwrap();

        assert_eq!(result["step"], "verify_constraints");
    }

    #[test]
    fn feature_after_verify_covers_concerns() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            &["verify_constraints"],
        )
        .unwrap();

        assert_eq!(result["step"], "cover_concerns");
    }

    #[test]
    fn feature_fully_covered_verifies_then_detects_patterns() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        // First call: VerifyConstraints (decisions exist).
        let result = advance(&state, "store", Some(TaskType::Feature), None, &[]).unwrap();
        assert_eq!(result["step"], "verify_constraints");

        // After verification: PatternDetection (no patterns yet).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            &["verify_constraints"],
        )
        .unwrap();
        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn feature_fully_covered_with_patterns_is_ready() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            &["verify_constraints"],
        )
        .unwrap();
        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Fix step sequence ─────────────────────────────────────────────

    #[test]
    fn fix_with_decisions_verifies_constraints() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let result = advance(&state, "store", Some(TaskType::Fix), None, &[]).unwrap();

        assert_eq!(result["step"], "verify_constraints");
    }

    #[test]
    fn fix_with_connections_verifies_then_checks_impact() {
        let mut state = build_state(
            &[("store", "Data store"), ("api", "API layer")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        state.graph_index.edges.push(EdgeEntry {
            from: "api".into(),
            to: "store".into(),
            kind: EdgeKind::ConnectsTo,
        });
        state.rebuild_graph();

        // First: VerifyConstraints (always first for Fix).
        let result = advance(&state, "store", Some(TaskType::Fix), None, &[]).unwrap();
        assert_eq!(result["step"], "verify_constraints");

        // After verification: ImpactCheck (connected component).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            &["verify_constraints"],
        )
        .unwrap();
        assert_eq!(result["step"], "impact_check");

        // After impact check: Ready.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            &["verify_constraints", "impact_check"],
        )
        .unwrap();
        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn fix_isolated_skips_impact_check() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        // After verification on isolated component: straight to Ready.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            &["verify_constraints"],
        )
        .unwrap();
        assert_eq!(result["step"], "ready");
    }

    #[test]
    fn fix_empty_component_is_ready() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", Some(TaskType::Fix), None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
    }

    #[test]
    fn fix_empty_with_relevant_task_covers_concerns() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            Some("fix concurrent write corruption"),
            &[],
        )
        .unwrap();

        // "concurrent" matches Concurrency & locking keywords → CoverConcerns.
        assert_eq!(result["step"], "cover_concerns");
        assert_eq!(result["ready"], false);
        let focus = result["action"]["focus"].as_array().unwrap();
        assert!(
            focus
                .iter()
                .any(|f| f.as_str().unwrap().contains("Concurrency")),
            "focus should include Concurrency for concurrent-related fix: {focus:?}"
        );
    }

    #[test]
    fn fix_empty_with_irrelevant_task_is_ready() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            Some("fix typo in log message"),
            &[],
        )
        .unwrap();

        // "typo" and "log" match no concern keywords → skip CoverConcerns.
        // No connections → skip ImpactCheck → Ready.
        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn fix_empty_with_connections_checks_impact() {
        let mut state = build_state(&[("store", "Data store"), ("api", "API layer")], &[]);
        state.graph_index.edges.push(EdgeEntry {
            from: "api".into(),
            to: "store".into(),
            kind: EdgeKind::ConnectsTo,
        });
        state.rebuild_graph();

        // No decisions, no task → skip CoverConcerns; but has connections → ImpactCheck.
        let result = advance(&state, "store", Some(TaskType::Fix), None, &[]).unwrap();

        assert_eq!(result["step"], "impact_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn fix_empty_relevant_task_takes_priority_over_impact() {
        let mut state = build_state(&[("store", "Data store"), ("api", "API layer")], &[]);
        state.graph_index.edges.push(EdgeEntry {
            from: "api".into(),
            to: "store".into(),
            kind: EdgeKind::ConnectsTo,
        });
        state.rebuild_graph();

        // Empty, has connections AND a relevant task → CoverConcerns first.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            Some("fix encryption key rotation"),
            &[],
        )
        .unwrap();

        assert_eq!(result["step"], "cover_concerns");
        let focus = result["action"]["focus"].as_array().unwrap();
        assert!(
            focus
                .iter()
                .any(|f| f.as_str().unwrap().contains("Security")),
            "focus should include Security for encryption-related fix: {focus:?}"
        );
    }

    #[test]
    fn fix_empty_relevant_task_pipeline_contract() {
        // Pipeline contract: cover_concerns returned by the fix workflow
        // must be accepted by build_step_prompt.
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            Some("fix error handling crash"),
            &[],
        )
        .unwrap();

        let step_name = result["step"].as_str().unwrap();
        assert_eq!(step_name, "cover_concerns");

        let prompt = crate::workflow::steps::build_step_prompt(
            &state,
            "store",
            step_name,
            Some("fix error handling crash"),
        )
        .expect("build_step_prompt must accept cover_concerns from fix workflow");

        assert!(
            prompt.instructions.contains("source code"),
            "missing source code preamble"
        );
        assert!(
            !prompt.focus.is_empty(),
            "cover_concerns must have a focus list"
        );
    }

    // ── Learn step sequence ───────────────────────────────────────────

    #[test]
    fn learn_empty_starts_with_analyze_code() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", Some(TaskType::Learn), None, &[]).unwrap();

        assert_eq!(result["step"], "analyze_code");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn learn_with_decisions_walks_them() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        let result = advance(&state, "store", Some(TaskType::Learn), None, &[]).unwrap();

        assert_eq!(result["step"], "walk_decisions");
    }

    #[test]
    fn learn_with_patterns_is_ready() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(&state, "store", Some(TaskType::Learn), None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Review step sequence ──────────────────────────────────────────

    #[test]
    fn review_starts_with_walk_decisions() {
        let decisions = well_covered_decisions("store", false);
        let state = build_state(&[("store", "Data store")], &decisions);
        let result = advance(&state, "store", Some(TaskType::Review), None, &[]).unwrap();

        assert_eq!(result["step"], "walk_decisions");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn review_after_walk_checks_drift() {
        let decisions = well_covered_decisions("store", false);
        let state = build_state(&[("store", "Data store")], &decisions);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            &["walk_decisions"],
        )
        .unwrap();

        assert_eq!(result["step"], "drift_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn review_fresh_with_gaps_walks_then_audits() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        // First: WalkDecisions (decisions exist).
        let result = advance(&state, "store", Some(TaskType::Review), None, &[]).unwrap();
        assert_eq!(result["step"], "walk_decisions");

        // After walk: CoverageAudit (gaps, no stale).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            &["walk_decisions"],
        )
        .unwrap();
        assert_eq!(result["step"], "coverage_audit");
    }

    #[test]
    fn review_healthy_walks_then_detects_patterns() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        // First: WalkDecisions.
        let result = advance(&state, "store", Some(TaskType::Review), None, &[]).unwrap();
        assert_eq!(result["step"], "walk_decisions");

        // After walk: PatternDetection (no stale, no gaps).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            &["walk_decisions"],
        )
        .unwrap();
        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn review_fully_healthy_is_ready_after_walk() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        // With completed walk: Ready (no stale, no gaps, patterns exist).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            &["walk_decisions"],
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Harden step sequence ──────────────────────────────────────────

    #[test]
    fn harden_starts_with_coverage_audit() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        let result = advance(&state, "store", Some(TaskType::Harden), None, &[]).unwrap();

        assert_eq!(result["step"], "coverage_audit");
    }

    #[test]
    fn harden_after_audit_covers_concerns() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Harden),
            None,
            &["coverage_audit"],
        )
        .unwrap();

        assert_eq!(result["step"], "cover_concerns");
        let focus = result["action"]["focus"].as_array().unwrap();
        assert!(focus.len() <= CONCERN_FOCUS_LIMIT);
    }

    #[test]
    fn harden_covered_detects_patterns() {
        let state = build_state(
            &[("store", "Data store")],
            &fully_covered_decisions("store", true),
        );
        let result = advance(&state, "store", Some(TaskType::Harden), None, &[]).unwrap();

        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn harden_fully_done_is_ready() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &fully_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(&state, "store", Some(TaskType::Harden), None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Project scope ─────────────────────────────────────────────────

    #[test]
    fn project_empty_defines_scope() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "project", None, None, &[]).unwrap();

        assert_eq!(result["step"], "define_scope");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn project_with_decisions_is_ready() {
        let state = build_state(
            &[],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
        );
        let result = advance(&state, "project", None, Some("add auth"), &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn project_learn() {
        let state = build_state(
            &[],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
        );
        let result = advance(&state, "project", Some(TaskType::Learn), None, &[]).unwrap();

        assert_eq!(result["step"], "walk_decisions");
        assert_eq!(result["task_type"], "learn");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn project_learn_with_patterns_is_ready() {
        let state = build_state_with_patterns(
            &[("auth", "Auth")],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
            &[("p1", "Security posture", &["auth"])],
        );
        let result = advance(&state, "project", Some(TaskType::Learn), None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn project_review_walks_when_no_patterns() {
        let state = build_state(
            &[],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
        );
        let result = advance(&state, "project", Some(TaskType::Review), None, &[]).unwrap();

        assert_eq!(result["step"], "drift_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn project_review_with_patterns_is_ready() {
        let state = build_state_with_patterns(
            &[("auth", "Auth")],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
            &[("p1", "Security posture", &["auth"])],
        );
        let result = advance(&state, "project", Some(TaskType::Review), None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn project_inferred_learn_when_no_patterns() {
        // Decisions exist, no task, no patterns → should infer Learn (not Review).
        let state = build_state(
            &[],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
        );
        let result = advance(&state, "project", None, None, &[]).unwrap();

        assert_eq!(result["task_type"], "learn");
        assert_eq!(result["step"], "walk_decisions");
    }

    #[test]
    fn project_inferred_ready_when_complete() {
        // Decisions + patterns + no task → should be ready.
        let state = build_state_with_patterns(
            &[("auth", "Auth")],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
            &[("p1", "Security posture", &["auth"])],
        );
        let result = advance(&state, "project", None, None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Response shape ────────────────────────────────────────────────

    #[test]
    fn response_has_all_required_fields() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        let result = advance(&state, "store", None, None, &[]).unwrap();

        assert!(result.get("component").is_some());
        assert!(result.get("task_type").is_some());
        assert!(result.get("step").is_some());
        assert!(result.get("ready").is_some());
        assert!(result.get("assessment").is_some());
        assert!(result.get("action").is_some());
        assert!(result["action"].get("tool").is_some());
        assert!(result["action"].get("instruction").is_some());
    }

    #[test]
    fn assessment_has_all_fields() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        let result = advance(&state, "store", None, None, &[]).unwrap();
        let assessment = &result["assessment"];

        assert!(assessment.get("decisions").is_some());
        assert!(assessment.get("concerns_covered").is_some());
        assert!(assessment.get("concerns_uncovered").is_some());
        assert!(assessment.get("stale_decisions").is_some());
        assert!(assessment.get("patterns").is_some());
    }

    // ── Idempotency ───────────────────────────────────────────────────

    #[test]
    fn advance_is_idempotent() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        let a = advance(&state, "store", None, None, &[]).unwrap();
        let b = advance(&state, "store", None, None, &[]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn advance_with_explicit_type_is_idempotent() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let a = advance(&state, "store", Some(TaskType::Review), None, &[]).unwrap();
        let b = advance(&state, "store", Some(TaskType::Review), None, &[]).unwrap();
        assert_eq!(a, b);
    }

    // ── Coverage boundary ─────────────────────────────────────────────

    #[test]
    fn coverage_boundary_equal_is_not_undercovered() {
        // 5 covered, 5 uncovered → 5 is not > 5 → not harden.
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
                    fresh_decision("store", "Mutex", "Locking", &["lock"]),
                ),
                (
                    "d-integrity",
                    fresh_decision("store", "BLAKE3 hash", "Integrity", &[]),
                ),
                (
                    "d-perf",
                    fresh_decision("store", "In-memory cache", "Performance", &["cache"]),
                ),
            ],
        );
        let result = advance(&state, "store", None, None, &[]).unwrap();

        // 5 covered, 5 uncovered → not harden (5 is not > 5).
        assert_ne!(result["task_type"], "harden");
    }

    // ── Focus priority ────────────────────────────────────────────────

    #[test]
    fn focus_follows_priority_order() {
        let state = build_state(
            &[("store", "Data store")],
            &[
                (
                    "d-scope",
                    fresh_decision("store", "Data layer", "Scope", &["scope"]),
                ),
                (
                    "d1",
                    fresh_decision("store", "TOML format", "Schema encoding", &[]),
                ),
            ],
        );
        let result = advance(&state, "store", Some(TaskType::NewComponent), None, &[]).unwrap();
        let focus = result["action"]["focus"].as_array().unwrap();

        // Data format (priority 8) covered via "TOML"/"format"/"Schema".
        // Top 3 uncovered by priority:
        // Security (1), Error (2), Concurrency (3).
        assert_eq!(focus[0], "Security boundaries");
        assert_eq!(focus[1], "Error handling & failure modes");
        assert_eq!(focus[2], "Concurrency & locking");
    }

    // ── Task passthrough ──────────────────────────────────────────────

    #[test]
    fn task_appears_in_action_args() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", None, Some("add caching"), &[]).unwrap();

        assert_eq!(result["action"]["args"]["task"], "add caching");
    }

    #[test]
    fn task_in_ready_state() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(&state, "store", None, Some("fix bug"), &[]).unwrap();

        assert_eq!(result["action"]["args"]["task"], "fix bug");
    }

    // ── TaskType parsing ──────────────────────────────────────────────

    #[test]
    fn invalid_task_type_rejected() {
        let result = TaskType::parse("deploy");
        assert!(result.is_err());
    }

    #[test]
    fn all_task_types_parse() {
        for s in &[
            "new_component",
            "feature",
            "fix",
            "learn",
            "review",
            "harden",
            "bootstrap",
        ] {
            assert!(TaskType::parse(s).is_ok());
        }
    }

    #[test]
    fn task_type_round_trips() {
        for tt in &[
            TaskType::NewComponent,
            TaskType::Feature,
            TaskType::Fix,
            TaskType::Learn,
            TaskType::Review,
            TaskType::Harden,
            TaskType::Bootstrap,
        ] {
            assert_eq!(TaskType::parse(tt.as_str()).unwrap(), *tt);
        }
    }

    // ── Bootstrap step sequence ───────────────────────────────────────

    #[test]
    fn bootstrap_empty_project_starts_with_scan() {
        let state = build_state(&[], &[]);
        let result = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["task_type"], "bootstrap");
        assert_eq!(result["step"], "scan_project");
        assert_eq!(result["ready"], false);
        assert_eq!(result["action"]["args"]["step"], "scan_project");
    }

    #[test]
    fn bootstrap_with_components_extracts_first_undecided() {
        let state = build_state(&[("auth", "Auth"), ("store", "Data store")], &[]);
        let result = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["step"], "extract_decisions");
        assert_eq!(result["ready"], false);
        // BTreeMap order: "auth" < "store" → auth is first.
        assert_eq!(result["action"]["target_component"], "auth");
        assert_eq!(result["action"]["args"]["component"], "auth");
    }

    #[test]
    fn bootstrap_cycles_through_components() {
        // auth has decisions, store does not → extract_decisions targets store.
        let state = build_state(
            &[("auth", "Auth"), ("store", "Data store")],
            &[(
                "d1",
                fresh_decision("auth", "JWT tokens", "Stateless auth", &[]),
            )],
        );
        let result = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["step"], "extract_decisions");
        assert_eq!(result["action"]["target_component"], "store");
    }

    #[test]
    fn bootstrap_all_decided_moves_to_project_rules() {
        let state = build_state(
            &[("auth", "Auth")],
            &[(
                "d1",
                fresh_decision("auth", "JWT tokens", "Stateless auth", &[]),
            )],
        );
        let result = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["step"], "project_rules");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn bootstrap_with_project_rules_detects_patterns() {
        let state = build_state(
            &[("auth", "Auth")],
            &[
                (
                    "d1",
                    fresh_decision("auth", "JWT tokens", "Stateless auth", &[]),
                ),
                (
                    "rule-1",
                    fresh_decision("project", "Fail-closed", "Safety", &[]),
                ),
            ],
        );
        let result = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["step"], "pattern_detection");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn bootstrap_fully_populated_is_ready() {
        let state = build_state_with_patterns(
            &[("auth", "Auth")],
            &[
                (
                    "d1",
                    fresh_decision("auth", "JWT tokens", "Stateless auth", &[]),
                ),
                (
                    "rule-1",
                    fresh_decision("project", "Fail-closed", "Safety", &[]),
                ),
            ],
            &[("p1", "Security posture", &["auth"])],
        );
        let result = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn bootstrap_is_idempotent() {
        let state = build_state(&[("auth", "Auth"), ("store", "Data store")], &[]);
        let a = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();
        let b = advance(&state, "project", Some(TaskType::Bootstrap), None, &[]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn bootstrap_at_component_level_extracts_decisions() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(&state, "store", Some(TaskType::Bootstrap), None, &[]).unwrap();

        // Component-level bootstrap: autonomous extraction, not Learn.
        assert_eq!(result["task_type"], "bootstrap");
        assert_eq!(result["step"], "extract_decisions");
        assert_eq!(result["action"]["args"]["component"], "store");
    }

    #[test]
    fn bootstrap_component_with_decisions_detects_patterns() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let result = advance(&state, "store", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn bootstrap_component_fully_done_is_ready() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
            &[("p1", "Data integrity", &["store"])],
        );
        let result = advance(&state, "store", Some(TaskType::Bootstrap), None, &[]).unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn bootstrap_project_rules_skipped_via_completed_steps() {
        let state = build_state(
            &[("auth", "Auth")],
            &[(
                "d1",
                fresh_decision("auth", "JWT tokens", "Stateless auth", &[]),
            )],
        );
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            &["project_rules"],
        )
        .unwrap();

        // project_rules completed → moves to pattern_detection.
        assert_eq!(result["step"], "pattern_detection");
    }
}
