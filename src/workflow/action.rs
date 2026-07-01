//! Step-to-action mapping and response builders for the workflow engine.
//!
//! Converts deduced [`Step`] values into concrete tool-call actions
//! (JSON payloads) that agents execute. Separated from the advance
//! dispatcher for navigability — the action surface is pure data
//! transformation with no graph inspection.

use std::sync::Arc;

use serde_json::Value;

use crate::store::schema::{DecisionFile, PatternFile};

use super::{Mode, Step, TaskType};

// ── Step → action mapping ─────────────────────────────────────────────────

/// Map a deduced step to a concrete tool action the agent should execute.
pub(super) fn step_action(component: &str, step: &Step, task: Option<&str>, mode: Mode) -> Value {
    match step {
        Step::Register => serde_json::json!({
            "tool": "add_component",
            "args": { "name": component },
            "instruction": "Component is not registered. Confirm the name \
                            and description, then call add_component.",
        }),

        Step::DefineScope => step_prompt_action(
            component,
            "define_scope",
            task,
            mode,
            match mode {
                Mode::Agent => "Read source code and determine this component's \
                     responsibilities and boundaries. Record scope decisions \
                     with tags: [\"scope\"] and attribution=\"agent\".",
                Mode::Interactive => "Define what the component is and isn't responsible for. \
                     Record each answer as a decision with tags: [\"scope\"].",
            },
        ),

        Step::AnalyzeCode => step_prompt_action(
            component,
            "analyze_code",
            task,
            mode,
            match mode {
                Mode::Agent => "Read every source file in this component. Identify \
                     all architectural decisions and record each immediately \
                     with attribution=\"agent\".",
                Mode::Interactive => "Read every source file in this component. Build a numbered \
                     list of all architectural decisions you identify. Present \
                     the list, then walk through each one.",
            },
        ),

        Step::CoverConcerns { focus } => {
            let instruction = match mode {
                Mode::Agent => format!(
                    "Cover uncovered concern areas: {}. For each, determine \
                     the decision the code has made and record with \
                     attribution=\"agent\".",
                    focus.join(", "),
                ),
                Mode::Interactive => format!(
                    "Cover uncovered concern areas: {}. For each, present \
                     2-3 viable options with trade-offs, ask the user to \
                     choose, and record with matching tags.",
                    focus.join(", "),
                ),
            };
            let mut action = step_prompt_action(
                component,
                "cover_concerns",
                task,
                mode,
                &instruction,
            );
            action["focus"] = serde_json::json!(focus);
            action
        }

        Step::WalkDecisions => step_prompt_action(
            component,
            "walk_decisions",
            task,
            mode,
            match mode {
                Mode::Agent => "Verify each recorded decision against the current \
                     source code. Update any that have drifted. Record \
                     unrecorded decisions with attribution=\"agent\".",
                Mode::Interactive => "Walk through each recorded decision with the user. Present \
                     one per message. After each, STOP and wait for the user's \
                     response. Then identify patterns across decisions.",
            },
        ),

        Step::VerifyConstraints => step_prompt_action(
            component,
            "verify_constraints",
            task,
            mode,
            match mode {
                Mode::Agent => "Verify each existing constraint against the source code. \
                     Check if the current task conflicts with any constraint. \
                     Update any that have drifted.",
                Mode::Interactive => "Present each existing constraint that the task may affect. \
                     For each, ask: \"Does your change respect this constraint, \
                     violate it, or require changing it?\" STOP and wait. If \
                     any constraint needs changing, call update_decision. Also \
                     check whether this change impacts connected components.",
            },
        ),

        Step::ImpactCheck => step_prompt_action(
            component,
            "impact_check",
            task,
            mode,
            match mode {
                Mode::Agent => "Read the interface code for connected components and \
                     determine whether the current task affects them.",
                Mode::Interactive => "Check whether this change impacts connected components. \
                     Review the architecture brief for cross-component effects.",
            },
        ),

        Step::PatternDetection => step_prompt_action(
            component,
            "pattern_detection",
            task,
            mode,
            match mode {
                Mode::Agent => "Review all recorded decisions. For groups of 2+ that \
                     reinforce the same invariant or form a defense-in-depth \
                     chain, call record_pattern with attribution=\"agent\".",
                Mode::Interactive => "Review all recorded decisions for this component and project \
                     rules. Look for groups of 2+ decisions that reinforce the \
                     same invariant, form a defense-in-depth chain, or share a \
                     common constraint. For each candidate, ask the user to \
                     confirm, then call record_pattern.",
            },
        ),

        Step::SummaryGate => step_prompt_action(
            component,
            "summary_gate",
            task,
            mode,
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
            mode,
            match mode {
                Mode::Agent => "Compare each recorded decision against the current source \
                     code. Supersede any that have drifted. Proceed autonomously.",
                Mode::Interactive => "Compare each recorded decision against the current source \
                     code. Flag any that have drifted from the implementation. \
                     For drifted decisions, call update_decision(supersede).",
            },
        ),

        Step::CoverageAudit => step_prompt_action(
            component,
            "coverage_audit",
            task,
            mode,
            match mode {
                Mode::Agent => "Audit concern coverage. Read source code for each gap \
                     and report which are real vs intentional.",
                Mode::Interactive => "Audit concern coverage. The assessment shows which areas \
                     lack decisions. For each gap, determine whether the \
                     component needs a decision there or if the gap is \
                     intentional.",
            },
        ),

        Step::UserExplains => step_prompt_action(
            component,
            "user_explains",
            task,
            mode,
            "Ask the user to describe this component's architecture from \
             memory. Do not show decisions or code first. Compare their \
             answer against recorded decisions afterward.",
        ),

        Step::ScanProject => step_prompt_action(
            "project",
            "scan_project",
            task,
            mode,
            "Read the project structure, identify major components, \
             and register them with add_component and add_connection.",
        ),

        Step::ProjectRules => step_prompt_action(
            "project",
            "project_rules",
            task,
            mode,
            "Identify cross-cutting project-level decisions and \
             record them with component='project'.",
        ),

        Step::ExtractDecisions { component: target } => step_prompt_action(
            target,
            "extract_decisions",
            task,
            mode,
            &format!(
                "Read every source file in [{target}] and record \
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

pub(super) fn build_response(
    component: &str,
    task_type: TaskType,
    step: &Step,
    ready: bool,
    mode: Mode,
    assessment: Value,
    action: Value,
) -> Value {
    let requires_user_input = match mode {
        Mode::Agent => false,
        Mode::Interactive => step.is_gated(),
    };
    serde_json::json!({
        "component": component,
        "task_type": task_type.as_str(),
        "step": step.as_str(),
        "ready": ready,
        "mode": mode.as_str(),
        "requires_user_input": requires_user_input,
        "assessment": assessment,
        "action": action,
    })
}

pub(super) fn build_assessment(
    decisions: &[(&Arc<str>, &DecisionFile)],
    covered: &[&str],
    uncovered: &[&str],
    stale: &[StaleDec],
    patterns: &[(&Arc<str>, &PatternFile)],
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

/// Bundled graph-health data for building a ready response.
pub(super) struct ReadyParams<'a> {
    pub(super) component: &'a str,
    pub(super) decisions: &'a [(&'a Arc<str>, &'a DecisionFile)],
    pub(super) covered: &'a [&'a str],
    pub(super) uncovered: &'a [&'a str],
    pub(super) stale: &'a [StaleDec],
    pub(super) patterns: &'a [(&'a Arc<str>, &'a PatternFile)],
    pub(super) task: Option<&'a str>,
    pub(super) mode: Mode,
}

pub(super) fn ready_response(p: ReadyParams<'_>) -> Value {
    let display_type = if p.decisions.is_empty() {
        TaskType::NewComponent
    } else {
        TaskType::Feature
    };
    let assessment = build_assessment(p.decisions, p.covered, p.uncovered, p.stale, p.patterns);
    build_response(
        p.component,
        display_type,
        &Step::Ready,
        true,
        p.mode,
        assessment,
        serde_json::json!({
            "tool": "get_context",
            "args": { "component": p.component, "task": p.task },
            "instruction": "Component is designed and ready for \
                            implementation. Call get_context for the \
                            authoritative brief.",
        }),
    )
}

/// Build a `get_step_prompt` action.
pub(super) fn step_prompt_action(
    component: &str,
    step: &str,
    task: Option<&str>,
    mode: Mode,
    instruction: &str,
) -> Value {
    serde_json::json!({
        "tool": "get_step_prompt",
        "args": {
            "component": component,
            "step": step,
            "task": task,
            "mode": mode.as_str(),
        },
        "instruction": instruction,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Select the top N concerns by priority (array order).
pub(super) fn top_n(concerns: &[&str], n: usize) -> Vec<String> {
    concerns.iter().take(n).map(|s| (*s).to_string()).collect()
}

pub(super) struct StaleDec {
    pub(super) name: Arc<str>,
    pub(super) created: String,
    pub(super) age_days: i64,
}
