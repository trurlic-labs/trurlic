//! Deterministic step-deduction state machine for workflow orchestration.
//!
//! The `advance` function computes the next workflow step from the knowledge
//! graph and returns a single focused action. Read-only, stateless, and
//! idempotent — no writes, no locks, no LLM calls, no clock reads.
//!
//! Called N times with the same `(graph state, now)` inputs, it returns the
//! same `(step, prompt)` every time. The wall clock is injected by the caller
//! as `now`, never read internally, so `advance` is a pure projection from its
//! inputs to a workflow step — deterministic and testable with a pinned clock.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::store::ProjectState;
use crate::store::limits::MIN_STEP_EVIDENCE_BYTES;
use crate::store::schema::DecisionFile;

use super::action::{
    ReadyParams, StaleDec, agent_unreviewed_count, build_assessment, build_response,
    ready_response, step_action, step_prompt_action, top_n, with_agent_review_hint,
};
use super::concerns;
use super::{CONCERN_FOCUS_LIMIT, Mode, STALENESS_THRESHOLD_DAYS, Step, TaskType};

// ── Public API ────────────────────────────────────────────────────────────

/// Compute the next workflow step for a component and return a structured
/// action.
///
/// Read-only. No writes. No locks. No LLM calls. Computes the step from the
/// in-memory graph and the injected `now` on every call — no session tracking,
/// no persistent workflow state. `now` is supplied by the caller (the impure
/// boundary) so this function never reads the clock and stays a pure,
/// deterministic projection of its inputs.
pub fn advance(
    state: &ProjectState,
    component: &str,
    task_type: Option<TaskType>,
    task: Option<&str>,
    mode: Option<Mode>,
    step_evidence: &BTreeMap<&str, &str>,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    // ── Mode gate ────────────────────────────────────────────────────
    let mode = match mode {
        Some(m) => m,
        None => {
            return Ok(serde_json::json!({
                "requires_mode": true,
                "step": Value::Null,
                "ready": false,
                "modes": {
                    "agent": "AI analyzes code and makes all decisions \
                              autonomously. Decisions recorded with \
                              attribution=agent for human review.",
                    "interactive": "User participates in design through \
                                    guided discussion. Each decision \
                                    requires user input.",
                },
            }));
        }
    };

    // ── Validate mode × task_type (if explicit) ──────────────────────
    if let Some(tt) = task_type {
        super::validate_mode_task(mode, tt)?;
    }

    // ── Evidence validation (interactive only) ───────────────────────
    if mode == Mode::Interactive {
        for (step_name, evidence) in step_evidence {
            if let Some(true) = Step::is_gated_name(step_name)
                && evidence.len() < MIN_STEP_EVIDENCE_BYTES
            {
                return Err(format!(
                    "step `{step_name}` is gated and requires evidence \
                         of at least {MIN_STEP_EVIDENCE_BYTES} bytes, \
                         but got {} bytes",
                    evidence.len()
                ));
            }
        }
    }

    let completed: Vec<&str> = step_evidence.keys().copied().collect();

    if component == "project" {
        return advance_project(state, task_type, task, mode, &completed);
    }

    if !state.components.contains_key(component) {
        let suggested = crate::store::slugify(component);
        return Ok(build_response(
            component,
            TaskType::NewComponent,
            &Step::Register,
            false,
            mode,
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

    let stale: Vec<StaleDec> = decisions
        .iter()
        .filter_map(|(name, d)| {
            let touched = d.decision.last_touched();
            let age_days = now.signed_duration_since(touched).num_days();
            (age_days >= STALENESS_THRESHOLD_DAYS).then(|| StaleDec {
                name: Arc::clone(name),
                created: d.decision.created.to_rfc3339(),
                last_touched: touched.to_rfc3339(),
                age_days,
            })
        })
        .collect();

    let patterns = graph.patterns_for(component);

    // ── Infer task type if not provided ───────────────────────────────

    let task_type = match task_type {
        Some(tt) => tt,
        None => match infer_task_type(&decisions, task, &covered, &uncovered, &stale) {
            Some(tt) => {
                // Validate mode × inferred task_type.
                super::validate_mode_task(mode, tt)?;
                tt
            }
            None => {
                // Fully designed, no task → Ready.
                return Ok(ready_response(ReadyParams {
                    component,
                    decisions: &decisions,
                    covered: &covered,
                    uncovered: &uncovered,
                    stale: &stale,
                    patterns: &patterns,
                    task,
                    mode,
                }));
            }
        },
    };

    // ── Deduce step ───────────────────────────────────────────────────

    let ctx = DeduceContext {
        decisions: &decisions,
        covered: &covered,
        uncovered: &uncovered,
        stale: &stale,
        patterns: &patterns,
        graph,
        component,
        task,
        completed: &completed,
        mode,
    };
    let step = deduce_step(task_type, &ctx);

    let ready = matches!(step, Step::Ready);
    let assessment = build_assessment(&decisions, &covered, &uncovered, &stale, &patterns);
    let action = step_action(component, &step, task, mode);

    let response = build_response(component, task_type, &step, ready, mode, assessment, action);
    if ready {
        Ok(with_agent_review_hint(
            response,
            agent_unreviewed_count(&decisions),
        ))
    } else {
        Ok(response)
    }
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

/// All state needed to deduce the next step for a component.
struct DeduceContext<'a> {
    decisions: &'a [(&'a Arc<str>, &'a DecisionFile)],
    covered: &'a [&'a str],
    uncovered: &'a [&'a str],
    stale: &'a [StaleDec],
    patterns: &'a [(&'a Arc<str>, &'a crate::store::schema::PatternFile)],
    graph: &'a crate::store::graph::InMemoryGraph,
    component: &'a str,
    task: Option<&'a str>,
    completed: &'a [&'a str],
    mode: Mode,
}

/// Deduce the next step from graph state for a given task type.
///
/// Each task type defines a sequence of steps. The state machine walks
/// the sequence by checking postconditions. Some postconditions are
/// heuristic (e.g. `PatternDetection` — checked via pattern count).
/// The machine errs on the side of returning `Ready` rather than looping.
fn deduce_step(task_type: TaskType, ctx: &DeduceContext<'_>) -> Step {
    match task_type {
        TaskType::NewComponent => deduce_new_component(
            ctx.decisions,
            ctx.covered,
            ctx.uncovered,
            ctx.patterns,
            ctx.completed,
            ctx.mode,
        ),
        TaskType::Feature => deduce_feature(
            ctx.decisions,
            ctx.covered,
            ctx.uncovered,
            ctx.patterns,
            ctx.task,
            ctx.completed,
            ctx.mode,
        ),
        TaskType::Fix => deduce_fix(
            ctx.decisions,
            ctx.uncovered,
            ctx.graph,
            ctx.component,
            ctx.task,
            ctx.completed,
        ),
        TaskType::Learn => deduce_learn(ctx.decisions, ctx.patterns, ctx.completed),
        TaskType::Review => deduce_review(
            ctx.decisions,
            ctx.stale,
            ctx.covered,
            ctx.uncovered,
            ctx.patterns,
            ctx.completed,
            ctx.mode,
        ),
        TaskType::Harden => deduce_harden(ctx.uncovered, ctx.patterns, ctx.completed),
        TaskType::Bootstrap => {
            deduce_bootstrap_component(ctx.decisions, ctx.patterns, ctx.component)
        }
    }
}

/// NewComponent: Register → DefineScope → CoverConcerns → PatternDetection → [DesignCheck] → Ready
fn deduce_new_component(
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    completed: &[&str],
    mode: Mode,
) -> Step {
    if !has_scope_decision(decisions) {
        return Step::DefineScope;
    }
    if uncovered.len() > covered.len() && !completed.contains(&"cover_concerns") {
        return Step::CoverConcerns {
            focus: top_n(uncovered, CONCERN_FOCUS_LIMIT),
        };
    }
    if patterns.is_empty() && !completed.contains(&"pattern_detection") {
        return Step::PatternDetection;
    }
    if mode == Mode::Interactive
        && !completed.contains(&"design_check")
        && !completed.contains(&"summary_gate")
    {
        return Step::DesignCheck;
    }
    Step::Ready
}

/// Feature: VerifyConstraints → CoverConcerns(focused) → PatternDetection → DesignCheck → Ready
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
///
/// DesignCheck: practical comprehension check — user demonstrates
/// understanding through a context-appropriate question.
fn deduce_feature(
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    task: Option<&str>,
    completed: &[&str],
    mode: Mode,
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

    // DesignCheck: comprehension check before ready (interactive only).
    if mode == Mode::Interactive
        && !completed.contains(&"design_check")
        && !completed.contains(&"summary_gate")
    {
        return Step::DesignCheck;
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

/// Learn: WarmUp → AnalyzeCode → WalkDecisions → DesignCheck → Ready
///
/// WarmUp: practical question that reveals the user's mental model.
/// Postcondition: step evidence (not graph-verifiable).
///
/// AnalyzeCode postcondition: decisions recorded (verifiable).
///
/// WalkDecisions postcondition: heuristic (patterns serve as proxy —
/// if patterns exist, walkthrough is complete).
///
/// DesignCheck: comprehension check — user summarizes understanding.
fn deduce_learn(
    decisions: &[(&Arc<str>, &DecisionFile)],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    completed: &[&str],
) -> Step {
    if !completed.contains(&"warm_up") && !completed.contains(&"user_explains") {
        return Step::WarmUp;
    }
    if decisions.is_empty() {
        return Step::AnalyzeCode;
    }
    if patterns.is_empty() {
        return Step::WalkDecisions;
    }
    if !completed.contains(&"design_check") && !completed.contains(&"summary_gate") {
        return Step::DesignCheck;
    }
    Step::Ready
}

/// Review: WalkDecisions → DriftCheck → CoverageAudit → PatternDetection → DesignCheck → Ready
///
/// WalkDecisions: interactive walkthrough of all decisions, challenging
/// each against current source code. No verifiable graph postcondition,
/// but typically produces updated decisions and/or patterns. Uses
/// `completed_steps` for progression.
///
/// DriftCheck: systematic verification for stale decisions. Staleness
/// is computed from `Decision::last_touched()` — the latest revision
/// timestamp if revised, else `created`. A decision clears staleness
/// either (a) by being revised via `update_decision`, which refreshes
/// `last_touched`, or (b) by the caller reporting `drift_check` in
/// `step_evidence` when decisions were confirmed unchanged (legitimate
/// outcome). Gated by `completed_steps` to prevent infinite loops when
/// stale decisions are confirmed without editing.
///
/// CoverageAudit: surfaces coverage gaps. Agent may record intentional-
/// gap decisions (graph change) or note gaps for future work.
///
/// DesignCheck: comprehension check — user summarizes review findings.
fn deduce_review(
    decisions: &[(&Arc<str>, &DecisionFile)],
    stale: &[StaleDec],
    covered: &[&str],
    uncovered: &[&str],
    patterns: &[(&Arc<str>, &crate::store::schema::PatternFile)],
    completed: &[&str],
    mode: Mode,
) -> Step {
    if !decisions.is_empty() && !completed.contains(&"walk_decisions") {
        return Step::WalkDecisions;
    }
    // Gated by completed_steps to prevent infinite loops when stale
    // decisions are confirmed unchanged (legitimate review outcome).
    if !stale.is_empty() && !completed.contains(&"drift_check") {
        return Step::DriftCheck;
    }
    if uncovered.len() > covered.len() {
        return Step::CoverageAudit;
    }
    if patterns.is_empty() && !completed.contains(&"pattern_detection") {
        return Step::PatternDetection;
    }
    if mode == Mode::Interactive
        && !completed.contains(&"design_check")
        && !completed.contains(&"summary_gate")
    {
        return Step::DesignCheck;
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
    if !uncovered.is_empty() && !completed.contains(&"coverage_audit") {
        return Step::CoverageAudit;
    }
    // Gated by completed_steps to prevent infinite loops when recorded
    // decisions don't match concern keywords.
    if !uncovered.is_empty() && !completed.contains(&"cover_concerns") {
        return Step::CoverConcerns {
            focus: top_n(uncovered, CONCERN_FOCUS_LIMIT),
        };
    }
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
    mode: Mode,
    completed_steps: &[&str],
) -> Result<Value, String> {
    let graph = state.graph();
    let decisions = graph.project_decisions();
    let has_decisions = !decisions.is_empty();
    let has_patterns = !graph.patterns_for("project").is_empty();

    let task_type = match task_type {
        Some(tt) => tt,
        None => {
            let inferred = if has_decisions {
                if task.is_some() || has_patterns {
                    TaskType::Feature
                } else {
                    TaskType::Learn
                }
            } else {
                TaskType::NewComponent
            };
            super::validate_mode_task(mode, inferred)?;
            inferred
        }
    };

    let project_patterns = graph.patterns_for("project");
    let assessment = serde_json::json!({
        "decisions": decisions.len(),
        "concerns_covered": Value::Array(vec![]),
        "concerns_uncovered": Value::Array(vec![]),
        "stale_decisions": Value::Array(vec![]),
        "patterns": project_patterns.len(),
    });

    let ready_action = || {
        serde_json::json!({
            "tool": "get_context",
            "args": { "component": "project", "task": task },
            "instruction": "Project rules are established. \
                            Call get_context for the brief.",
        })
    };

    let (step, ready, action) = match task_type {
        TaskType::NewComponent | TaskType::Harden => {
            if has_decisions {
                (Step::Ready, true, ready_action())
            } else {
                (
                    Step::DefineScope,
                    false,
                    step_prompt_action(
                        "project",
                        "define_scope",
                        task,
                        mode,
                        "No project rules recorded. Run a design session \
                         to establish cross-cutting principles.",
                    ),
                )
            }
        }
        TaskType::Feature | TaskType::Fix => {
            if has_decisions {
                (Step::Ready, true, ready_action())
            } else {
                (
                    Step::DefineScope,
                    false,
                    step_prompt_action(
                        "project",
                        "define_scope",
                        task,
                        mode,
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
                        mode,
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
                        mode,
                        "Present each project rule for understanding. \
                         After all are walked, identify patterns.",
                    ),
                )
            } else {
                (Step::Ready, true, ready_action())
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
                        mode,
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
                        mode,
                        "Review project rules for drift. After review, \
                         identify patterns across decisions.",
                    ),
                )
            } else {
                (Step::Ready, true, ready_action())
            }
        }
        TaskType::Bootstrap => deduce_bootstrap_project(state, task, mode, completed_steps),
    };

    let response = build_response("project", task_type, &step, ready, mode, assessment, action);
    if ready {
        // Match component-level ready: surface unreviewed agent-authored project
        // rules so the schema is uniform and project rules aren't silently exempt
        // from review.
        Ok(with_agent_review_hint(
            response,
            agent_unreviewed_count(&decisions),
        ))
    } else {
        Ok(response)
    }
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
    mode: Mode,
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
                mode,
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
                mode,
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
                mode,
                "Identify cross-cutting project-level decisions and \
                 record them with component='project'.",
            ),
        );
    }

    // Phase 4: no project-scoped patterns → detect them.
    if graph.patterns_for("project").is_empty() && !completed.contains(&"pattern_detection") {
        return (
            Step::PatternDetection,
            false,
            step_prompt_action(
                "project",
                "pattern_detection",
                task,
                mode,
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

// ── Helpers ───────────────────────────────────────────────────────────────

/// Check whether any decision carries a "scope" tag.
fn has_scope_decision(decisions: &[(&Arc<str>, &DecisionFile)]) -> bool {
    decisions
        .iter()
        .any(|(_, d)| d.decision.tags.iter().any(|t| t == "scope"))
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    const SUFFICIENT_EVIDENCE: &str =
        "User confirmed this step was completed thoroughly and correctly";

    fn evidence<'a>(steps: &[&'a str]) -> BTreeMap<&'a str, &'static str> {
        steps
            .iter()
            .copied()
            .map(|s| (s, SUFFICIENT_EVIDENCE))
            .collect()
    }

    // ── Fixtures ──────────────────────────────────────────────────────

    fn build_state(
        components: &[(&str, &str)],
        decisions: &[(&str, DecisionFile)],
    ) -> ProjectState {
        let mut comp_map = BTreeMap::new();
        for &(name, desc) in components {
            comp_map.insert(
                name.into(),
                Arc::new(ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: desc.into(),
                    },
                }),
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
            dec_map.insert(name.into(), Arc::new(dec.clone()));
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
                Arc::new(PatternFile {
                    pattern: Pattern {
                        name: name.into(),
                        description: desc.into(),
                    },
                }),
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
                attribution: Attribution::User,
                created: Utc::now(),
                code_refs: vec![],
                history: vec![],
            },
        }
    }

    fn agent_decision(component: &str, choice: &str, reason: &str, tags: &[&str]) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: choice.into(),
                reason: reason.into(),
                alternatives: vec![],
                tags: tags.iter().map(|t| (*t).into()).collect(),
                attribution: Attribution::Agent,
                created: Utc::now(),
                code_refs: vec![],
                history: vec![],
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
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
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
        let result = advance(
            &state,
            "rate-limiter",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "register");
        assert_eq!(result["task_type"], "new_component");
        assert_eq!(result["ready"], false);
        assert_eq!(result["action"]["tool"], "add_component");
        assert_eq!(result["action"]["args"]["name"], "rate-limiter");
    }

    #[test]
    fn unregistered_suggests_kebab() {
        let state = build_state(&[], &[]);
        let result = advance(
            &state,
            "Rate Limiter",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "register");
        assert_eq!(result["action"]["args"]["name"], "rate-limiter");
    }

    // ── TaskType inference ────────────────────────────────────────────

    #[test]
    fn infer_learn_when_empty_no_task() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["task_type"], "learn");
        assert_eq!(result["step"], "warm_up");
    }

    #[test]
    fn infer_new_component_when_empty_with_task() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            None,
            Some("build data layer"),
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["task_type"], "harden");
        assert_eq!(result["step"], "coverage_audit");
    }

    #[test]
    fn infer_review_when_stale() {
        let decisions = well_covered_decisions("store", false);
        let state = build_state(&[("store", "Data store")], &decisions);
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["task_type"], "review");
        assert_eq!(result["step"], "walk_decisions");
    }

    #[test]
    fn staleness_is_driven_by_the_injected_clock_and_is_deterministic() {
        // Decisions created 2024-01-01, well covered. Whether they read as stale
        // is a function of the `now` the caller injects — advance never reads the
        // wall clock — so the same graph yields different steps under different
        // clocks and identical results under the same clock.
        let decisions = well_covered_decisions("store", false);
        let state = build_state(&[("store", "Data store")], &decisions);
        let call = |now| {
            advance(
                &state,
                "store",
                None,
                None,
                Some(Mode::Interactive),
                &BTreeMap::new(),
                now,
            )
            .unwrap()
        };

        // Two weeks after creation: nothing is stale → no drift-driven Review.
        let early = call(Utc.with_ymd_and_hms(2024, 1, 15, 0, 0, 0).unwrap());
        assert_ne!(early["task_type"], "review");

        // A year later the same graph crosses the staleness threshold → Review.
        let late_now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let late = call(late_now);
        assert_eq!(late["task_type"], "review");

        // Same (graph, now) in → identical response out: advance is pure.
        assert_eq!(late, call(late_now));
    }

    #[test]
    fn infer_feature_when_healthy_with_task() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(
            &state,
            "store",
            None,
            Some("add caching"),
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["task_type"], "feature");
    }

    #[test]
    fn infer_ready_when_healthy_no_task() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Agent-review count on ready ────────────────────────────────────

    #[test]
    fn ready_response_reports_agent_review_count() {
        let mut decs = well_covered_decisions("store", true);
        decs.push((
            "d-agent",
            agent_decision("store", "Auto-detected cache", "Inferred", &[]),
        ));
        let state = build_state(&[("store", "Data store")], &decs);
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["ready"], true);
        assert_eq!(result["agent_decisions_unreviewed"], 1);
        assert!(
            result["hint"].as_str().unwrap().contains("pending review"),
            "hint should prompt review: {}",
            result["hint"]
        );
    }

    #[test]
    fn ready_response_hint_null_when_all_reviewed() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["ready"], true);
        assert_eq!(result["agent_decisions_unreviewed"], 0);
        assert!(result["hint"].is_null());
    }

    #[test]
    fn feature_ready_carries_agent_review_count() {
        let mut decs = well_covered_decisions("store", true);
        decs.push((
            "d-agent",
            agent_decision("store", "Auto-detected cache", "Inferred", &[]),
        ));
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &decs,
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &evidence(&["verify_constraints", "design_check"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["agent_decisions_unreviewed"], 1);
    }

    #[test]
    fn project_ready_carries_agent_review_count() {
        // A ready project response must carry the same review hint as a
        // component's, so agent-authored project rules aren't silently exempt.
        let decs = vec![(
            "d-agent",
            agent_decision("project", "Deny unsafe code project-wide", "Inferred", &[]),
        )];
        let state = build_state(&[], &decs);
        let result = advance(
            &state,
            "project",
            Some(TaskType::Harden),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["ready"], true);
        assert_eq!(result["agent_decisions_unreviewed"], 1);
    }

    // ── NewComponent step sequence ────────────────────────────────────

    #[test]
    fn new_component_starts_with_define_scope() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn new_component_with_patterns_moves_to_design_check() {
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
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "design_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn new_component_after_design_check_is_ready() {
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
            Some(Mode::Interactive),
            &evidence(&["design_check"]),
            chrono::Utc::now(),
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "verify_constraints");

        // After verification: PatternDetection (no patterns yet).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn feature_reaches_design_check_after_pattern_detection() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &evidence(&["verify_constraints", "pattern_detection"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "design_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn feature_design_check_blocks_ready() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        // Without design_check evidence → stays at design_check.
        let r1 = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(r1["step"], "design_check");

        // Same call again → still design_check (idempotent).
        let r2 = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(r2["step"], "design_check");
    }

    #[test]
    fn feature_fully_covered_with_patterns_reaches_design_check() {
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
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "design_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn feature_fully_covered_with_patterns_is_ready_after_design_check() {
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
            Some(Mode::Interactive),
            &evidence(&["verify_constraints", "design_check"]),
            chrono::Utc::now(),
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "verify_constraints");

        // After verification: ImpactCheck (connected component).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "impact_check");

        // After impact check: Ready.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            Some(Mode::Interactive),
            &evidence(&["verify_constraints", "impact_check"]),
            chrono::Utc::now(),
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
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "ready");
    }

    #[test]
    fn fix_empty_component_is_ready() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
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
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Fix),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
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
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        let step_name = result["step"].as_str().unwrap();
        assert_eq!(step_name, "cover_concerns");

        let prompt = crate::workflow::steps::build_step_prompt(
            &state,
            "store",
            step_name,
            Some("fix error handling crash"),
            Some("fix"),
            Mode::Interactive,
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
    fn learn_starts_with_warm_up() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "warm_up");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn learn_warm_up_gates_analyze_code() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &evidence(&["warm_up"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "analyze_code");
    }

    #[test]
    fn learn_accepts_user_explains_alias_as_evidence() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &evidence(&["user_explains"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "analyze_code");
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &evidence(&["warm_up"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "walk_decisions");
    }

    #[test]
    fn learn_reaches_design_check() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &evidence(&["warm_up"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "design_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn learn_with_patterns_is_ready_after_design_check() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &evidence(&["warm_up", "design_check"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn learn_accepts_summary_gate_alias_as_evidence() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &evidence(&["warm_up", "summary_gate"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Review step sequence ──────────────────────────────────────────

    #[test]
    fn review_starts_with_walk_decisions() {
        let decisions = well_covered_decisions("store", false);
        let state = build_state(&[("store", "Data store")], &decisions);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            Some(Mode::Interactive),
            &evidence(&["walk_decisions"]),
            chrono::Utc::now(),
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "walk_decisions");

        // After walk: CoverageAudit (gaps, no stale).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions"]),
            chrono::Utc::now(),
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "walk_decisions");

        // After walk: PatternDetection (no stale, no gaps).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn review_reaches_design_check() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        // With completed walk: DesignCheck (no stale, no gaps, patterns exist).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "design_check");
        assert_eq!(result["ready"], false);
    }

    #[test]
    fn review_fully_healthy_is_ready_after_design_check() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions", "design_check"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn review_stale_reaches_drift_check_before_design_check() {
        // Stale decisions: walk_decisions → drift_check → (stale cleared) →
        // pattern_detection or design_check. Here, stale decisions exist,
        // but after walk and with patterns, hits design_check.
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &well_covered_decisions("store", false),
            &[("p1", "Integrity chain", &["store"])],
        );
        // walk_decisions done, but stale → drift_check first.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "drift_check");
    }

    // ── T01: DriftCheck regression tests ────────────────────────────

    #[test]
    fn review_revised_decision_not_stale_skips_drift_check() {
        // Decision created 200+ days ago but revised 5 days ago.
        // last_touched is the revision date → NOT stale → no DriftCheck.
        let old_created = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let recent_revision = Utc.with_ymd_and_hms(2025, 6, 25, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 30, 0, 0, 0).unwrap();

        let mut decs: Vec<(&str, DecisionFile)> = vec![(
            "d-security",
            DecisionFile {
                decision: Decision {
                    component: "store".into(),
                    choice: "Auth tokens revised".into(),
                    reason: "Token security updated".into(),
                    alternatives: vec![],
                    tags: vec!["security".into()],
                    attribution: Attribution::User,
                    created: old_created,
                    code_refs: vec![],
                    history: vec![HistoryEntry {
                        choice: "Auth tokens".into(),
                        reason: "Token security".into(),
                        changed_at: recent_revision,
                    }],
                },
            },
        )];
        // Add remaining decisions also created recently (relative to `now`)
        // so they are not stale. Use dates within 90 days of `now`.
        let recent = Utc.with_ymd_and_hms(2025, 5, 1, 0, 0, 0).unwrap();
        for (name, choice, reason, tags) in [
            ("d-errors", "Fail-closed", "Error recovery", &["error"][..]),
            (
                "d-locking",
                "RwLock for state",
                "Concurrent access",
                &["lock"][..],
            ),
            (
                "d-integrity",
                "BLAKE3 hash validation",
                "Integrity check",
                &[][..],
            ),
            (
                "d-perf",
                "In-memory cache",
                "Performance target",
                &["cache"][..],
            ),
            ("d-api", "REST API protocol", "External interface", &[][..]),
        ] {
            decs.push((
                name,
                DecisionFile {
                    decision: Decision {
                        component: "store".into(),
                        choice: choice.into(),
                        reason: reason.into(),
                        alternatives: vec![],
                        tags: tags.iter().map(|t| (*t).into()).collect(),
                        attribution: Attribution::User,
                        created: recent,
                        code_refs: vec![],
                        history: vec![],
                    },
                },
            ));
        }

        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &decs,
            &[("p1", "Integrity chain", &["store"])],
        );

        // walk_decisions completed → no stale decisions (revised clears it)
        // → should skip DriftCheck entirely.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions"]),
            now,
        )
        .unwrap();

        assert_ne!(
            result["step"], "drift_check",
            "revised decision should NOT be stale; should skip DriftCheck"
        );
    }

    #[test]
    fn review_untouched_stale_decision_returns_drift_check() {
        // Decision created 200+ days ago, never revised → stale → DriftCheck.
        let now = Utc.with_ymd_and_hms(2025, 6, 30, 0, 0, 0).unwrap();
        let decisions = well_covered_decisions("store", false); // created 2024-01-01
        let state = build_state(&[("store", "Data store")], &decisions);

        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions"]),
            now,
        )
        .unwrap();

        assert_eq!(result["step"], "drift_check");
    }

    #[test]
    fn review_drift_check_completed_progresses_past_it() {
        // Untouched stale decisions exist, but drift_check is in completed
        // → should NOT return DriftCheck again (completed gate).
        let now = Utc.with_ymd_and_hms(2025, 6, 30, 0, 0, 0).unwrap();
        let decisions = well_covered_decisions("store", false); // stale
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &decisions,
            &[("p1", "Integrity chain", &["store"])],
        );

        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &evidence(&["walk_decisions", "drift_check"]),
            now,
        )
        .unwrap();

        assert_ne!(
            result["step"], "drift_check",
            "drift_check in completed_steps must prevent infinite DriftCheck loop"
        );
    }

    #[test]
    fn review_staleness_deterministic_with_revised_decisions() {
        // Same inputs + same `now` → identical output. Advance stays pure
        // even with history-based last_touched computation.
        let old_created = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let recent_revision = Utc.with_ymd_and_hms(2025, 6, 25, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 30, 0, 0, 0).unwrap();

        let decs = vec![(
            "d1",
            DecisionFile {
                decision: Decision {
                    component: "store".into(),
                    choice: "Current".into(),
                    reason: "Updated reason".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: old_created,
                    code_refs: vec![],
                    history: vec![HistoryEntry {
                        choice: "Original".into(),
                        reason: "Original reason".into(),
                        changed_at: recent_revision,
                    }],
                },
            },
        )];
        let state = build_state(&[("store", "Data store")], &decs);

        let call = |n| {
            advance(
                &state,
                "store",
                Some(TaskType::Review),
                None,
                Some(Mode::Interactive),
                &evidence(&["walk_decisions"]),
                n,
            )
            .unwrap()
        };

        assert_eq!(
            call(now),
            call(now),
            "same (graph, now) must give identical results"
        );
    }

    #[test]
    fn review_stale_assessment_includes_last_touched() {
        // When decisions ARE stale, the assessment should report last_touched.
        let now = Utc.with_ymd_and_hms(2025, 6, 30, 0, 0, 0).unwrap();
        let decisions = well_covered_decisions("store", false); // created 2024-01-01
        let state = build_state(&[("store", "Data store")], &decisions);

        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            now,
        )
        .unwrap();

        let stale_decs = result["assessment"]["stale_decisions"]
            .as_array()
            .expect("stale_decisions must be an array");
        assert!(!stale_decs.is_empty());
        for sd in stale_decs {
            assert!(
                sd.get("last_touched").is_some(),
                "stale decision should include last_touched: {sd}"
            );
            assert!(
                sd.get("created").is_some(),
                "stale decision should still include created: {sd}"
            );
        }
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Harden),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            Some(Mode::Interactive),
            &evidence(&["coverage_audit"]),
            chrono::Utc::now(),
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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Harden),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn harden_fully_done_is_ready() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &fully_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Harden),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    // ── Project scope ─────────────────────────────────────────────────

    #[test]
    fn project_empty_defines_scope() {
        let state = build_state(&[], &[]);
        let result = advance(
            &state,
            "project",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            None,
            Some("add auth"),
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            &[("p1", "Security posture", &["auth", "project"])],
        );
        let result = advance(
            &state,
            "project",
            Some(TaskType::Learn),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            &[("p1", "Security posture", &["auth", "project"])],
        );
        let result = advance(
            &state,
            "project",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            &[("p1", "Security posture", &["auth", "project"])],
        );
        let result = advance(
            &state,
            "project",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert!(result.get("component").is_some());
        assert!(result.get("task_type").is_some());
        assert!(result.get("step").is_some());
        assert!(result.get("ready").is_some());
        assert!(result.get("requires_user_input").is_some());
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
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
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
        let a = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        let b = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn advance_with_explicit_type_is_idempotent() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let a = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        let b = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
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
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
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
        let result = advance(
            &state,
            "store",
            None,
            Some("add caching"),
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["action"]["args"]["task"], "add caching");
    }

    #[test]
    fn task_in_ready_state() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(
            &state,
            "store",
            None,
            Some("fix bug"),
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["task_type"], "bootstrap");
        assert_eq!(result["step"], "scan_project");
        assert_eq!(result["ready"], false);
        assert_eq!(result["action"]["args"]["step"], "scan_project");
    }

    #[test]
    fn bootstrap_with_components_extracts_first_undecided() {
        let state = build_state(&[("auth", "Auth"), ("store", "Data store")], &[]);
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            &[("p1", "Security posture", &["auth", "project"])],
        );
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
    }

    #[test]
    fn bootstrap_is_idempotent() {
        let state = build_state(&[("auth", "Auth"), ("store", "Data store")], &[]);
        let a = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        let b = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn bootstrap_at_component_level_extracts_decisions() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
        let result = advance(
            &state,
            "store",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

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
            Some(Mode::Agent),
            &evidence(&["project_rules"]),
            chrono::Utc::now(),
        )
        .unwrap();

        // project_rules completed → moves to pattern_detection.
        assert_eq!(result["step"], "pattern_detection");
    }

    #[test]
    fn bootstrap_pipeline_unaffected() {
        // Full bootstrap pipeline: scan → extract → project_rules →
        // pattern_detection → ready. No design_check, no warm_up.
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
            &[("p1", "Security posture", &["auth", "project"])],
        );
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "ready");
        assert_eq!(result["ready"], true);
        // Bootstrap never returns design_check or warm_up.
    }

    // ── Step evidence validation ──────────────────────────────────

    #[test]
    fn gated_step_without_evidence_returns_error() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let mut ev = BTreeMap::new();
        ev.insert("verify_constraints", "");
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &ev,
            chrono::Utc::now(),
        );
        let err = result.unwrap_err();
        assert!(err.contains("gated"), "error should mention gated: {err}");
        assert!(
            err.contains("0 bytes"),
            "error should mention byte count: {err}"
        );
    }

    #[test]
    fn gated_step_with_short_evidence_returns_error() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let mut ev = BTreeMap::new();
        ev.insert("verify_constraints", "ok");
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Interactive),
            &ev,
            chrono::Utc::now(),
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("2 bytes"),
            "error should mention actual byte count: {err}"
        );
    }

    #[test]
    fn gated_step_with_sufficient_evidence_progresses() {
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
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_ne!(result["step"], "verify_constraints");
    }

    #[test]
    fn ungated_step_with_empty_evidence_progresses() {
        let state = build_state(
            &[("auth", "Auth")],
            &[(
                "d1",
                fresh_decision("auth", "JWT tokens", "Stateless auth", &[]),
            )],
        );
        let mut ev = BTreeMap::new();
        ev.insert("project_rules", "");
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &ev,
            chrono::Utc::now(),
        )
        .unwrap();
        assert_ne!(result["step"], "project_rules");
    }

    #[test]
    fn unknown_step_in_evidence_ignored() {
        let state = build_state(&[("store", "Data store")], &[]);
        let mut ev = BTreeMap::new();
        ev.insert("nonexistent", "some evidence text that is long enough");
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &ev,
            chrono::Utc::now(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn requires_user_input_true_for_gated_steps() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "define_scope");
        assert_eq!(result["requires_user_input"], true);
    }

    #[test]
    fn requires_user_input_false_for_ready() {
        let state = build_state(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
        );
        let result = advance(
            &state,
            "store",
            None,
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "ready");
        assert_eq!(result["requires_user_input"], false);
    }

    // ── Feature task-relevant concerns ────────────────────────────────

    #[test]
    fn feature_with_task_covers_relevant_concerns() {
        // Setup: component with one decision (so VerifyConstraints applies),
        // most concerns uncovered, and a task whose keywords match
        // "Performance constraints" via the "cache"/"caching" keyword.
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );

        // First call: VerifyConstraints (decisions exist).
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            Some("add caching layer"),
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "verify_constraints");

        // After verification: CoverConcerns with task-relevant focus.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            Some("add caching layer"),
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "cover_concerns");
        assert_eq!(result["ready"], false);
        let focus = result["action"]["focus"].as_array().unwrap();
        assert!(
            focus
                .iter()
                .any(|f| f.as_str().unwrap().contains("Performance")),
            "focus should include Performance for caching-related task: {focus:?}"
        );
    }

    #[test]
    fn feature_with_task_irrelevant_to_concerns_uses_majority_threshold() {
        // Task words that don't match any concern keywords → fall back to
        // majority threshold (uncovered > covered).
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );

        // After verification: task "add widget support" has no concern
        // keyword matches, but uncovered > covered → CoverConcerns with
        // top N uncovered by priority.
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            Some("add widget support"),
            Some(Mode::Interactive),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["step"], "cover_concerns");
        let focus = result["action"]["focus"].as_array().unwrap();
        // Should be priority-ordered: Security first.
        assert_eq!(
            focus[0].as_str().unwrap(),
            "Security boundaries",
            "fallback focus should follow priority order: {focus:?}"
        );
    }

    // ── Mode gate ────────────────────────────────────────────────────

    #[test]
    fn advance_without_mode_returns_requires_mode() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            None,
            None,
            None,
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();

        assert_eq!(result["requires_mode"], true);
        assert!(result["step"].is_null());
        assert_eq!(result["ready"], false);
        assert!(result["modes"]["agent"].is_string());
        assert!(result["modes"]["interactive"].is_string());
    }

    #[test]
    fn advance_agent_learn_rejected() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("interactive"), "error: {err}");
    }

    #[test]
    fn advance_interactive_bootstrap_rejected() {
        let state = build_state(&[], &[]);
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("agent"), "error: {err}");
    }

    #[test]
    fn agent_mode_skips_evidence_validation() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        let mut ev = BTreeMap::new();
        ev.insert("verify_constraints", "");
        let result = advance(
            &state,
            "store",
            Some(TaskType::Feature),
            None,
            Some(Mode::Agent),
            &ev,
            chrono::Utc::now(),
        );
        assert!(result.is_ok(), "agent mode should skip evidence validation");
    }

    #[test]
    fn agent_mode_never_returns_design_check() {
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
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_ne!(result["step"], "design_check");
        assert_eq!(result["step"], "ready");
    }

    #[test]
    fn agent_mode_never_returns_warm_up() {
        // Learn is the only task type with WarmUp, and Learn
        // requires interactive mode. Verify the validation catches it.
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::Learn),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        );
        assert!(
            result.is_err(),
            "agent + learn should be rejected, preventing warm_up"
        );
    }

    #[test]
    fn agent_mode_requires_user_input_always_false() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["requires_user_input"], false);
        assert_eq!(result["mode"], "agent");
    }

    #[test]
    fn interactive_mode_requires_user_input_for_gated() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "define_scope");
        assert_eq!(result["requires_user_input"], true);
        assert_eq!(result["mode"], "interactive");
    }

    #[test]
    fn response_includes_mode_field() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["mode"], "agent");
    }

    #[test]
    fn agent_mode_new_component_skips_design_check_to_ready() {
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
        // Interactive would return design_check; agent skips to ready.
        let interactive = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(interactive["step"], "design_check");

        let agent = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(agent["step"], "ready");
    }

    #[test]
    fn agent_mode_review_skips_design_check() {
        let state = build_state_with_patterns(
            &[("store", "Data store")],
            &well_covered_decisions("store", true),
            &[("p1", "Integrity chain", &["store"])],
        );
        let result = advance(
            &state,
            "store",
            Some(TaskType::Review),
            None,
            Some(Mode::Agent),
            &evidence(&["walk_decisions"]),
            chrono::Utc::now(),
        )
        .unwrap();
        // Agent skips design_check → ready.
        assert_eq!(result["step"], "ready");
    }

    #[test]
    fn agent_mode_feature_skips_design_check() {
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
            Some(Mode::Agent),
            &evidence(&["verify_constraints"]),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(result["step"], "ready");
    }

    #[test]
    fn project_agent_learn_inferred_rejected() {
        let state = build_state(
            &[],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
        );
        let result = advance(
            &state,
            "project",
            None,
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        );
        assert!(
            result.is_err(),
            "project-level inferred Learn should be rejected in agent mode"
        );
        let err = result.unwrap_err();
        assert!(err.contains("interactive"), "error: {err}");
    }

    #[test]
    fn action_args_include_mode() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(
            result["action"]["args"]["mode"], "agent",
            "action args must include mode for get_step_prompt"
        );
    }

    #[test]
    fn action_args_include_mode_interactive() {
        let state = build_state(&[("store", "Data store")], &[]);
        let result = advance(
            &state,
            "store",
            Some(TaskType::NewComponent),
            None,
            Some(Mode::Interactive),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(
            result["action"]["args"]["mode"], "interactive",
            "action args must include mode for get_step_prompt"
        );
    }

    #[test]
    fn bootstrap_action_args_include_mode() {
        let state = build_state(&[], &[]);
        let result = advance(
            &state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &BTreeMap::new(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(
            result["action"]["args"]["mode"], "agent",
            "bootstrap action args must include mode"
        );
    }
}
