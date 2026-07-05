//! Step-to-action mapping and response builders for the workflow engine.
//!
//! Converts deduced [`Step`] values into concrete tool-call actions
//! (JSON payloads) that agents execute. Separated from the advance
//! dispatcher for navigability — the action surface is pure data
//! transformation with no graph inspection.

use std::sync::Arc;

use serde_json::Value;

use crate::store::schema::{Attribution, DecisionFile, PatternFile};

use super::{Mode, Step, TaskType};

// ── Step → action mapping ─────────────────────────────────────────────────

/// Map a deduced step to a concrete tool action the agent should execute.
///
/// All instruction text comes from [`super::steps::summary`] — the single
/// source of truth for the short instruction that accompanies each action.
pub(super) fn step_action(component: &str, step: &Step, task: Option<&str>, mode: Mode) -> Value {
    let instruction = super::steps::summary(step, mode);
    let step_name = step.as_str();
    match step {
        Step::Register => serde_json::json!({
            "tool": "add_component",
            "args": { "name": component },
            "instruction": &*instruction,
        }),

        Step::CoverConcerns { focus } => {
            let mut action = step_prompt_action(component, step_name, task, mode, &instruction);
            action["focus"] = serde_json::json!(focus);
            action
        }

        Step::ScanProject | Step::ProjectRules => {
            step_prompt_action("project", step_name, task, mode, &instruction)
        }
        Step::ExtractDecisions { component: target } => {
            step_prompt_action(target, step_name, task, mode, &instruction)
        }

        Step::Ready => serde_json::json!({
            "tool": "get_context",
            "args": { "component": component, "task": task },
            "instruction": &*instruction,
        }),

        Step::DefineScope
        | Step::AnalyzeCode
        | Step::WalkDecisions
        | Step::VerifyConstraints
        | Step::ImpactCheck
        | Step::PatternDetection
        | Step::DesignCheck
        | Step::DriftCheck
        | Step::CoverageAudit
        | Step::WarmUp => step_prompt_action(component, step_name, task, mode, &instruction),
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
                "last_touched": &s.last_touched,
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
    let instruction = super::steps::summary(&Step::Ready, p.mode);
    let response = build_response(
        p.component,
        display_type,
        &Step::Ready,
        true,
        p.mode,
        assessment,
        serde_json::json!({
            "tool": "get_context",
            "args": { "component": p.component, "task": p.task },
            "instruction": &*instruction,
        }),
    );
    with_agent_review_hint(response, agent_unreviewed_count(p.decisions))
}

/// Count decisions still attributed to the agent — those recorded
/// autonomously and not yet confirmed by a human.
pub(super) fn agent_unreviewed_count(decisions: &[(&Arc<str>, &DecisionFile)]) -> usize {
    decisions
        .iter()
        .filter(|(_, d)| d.decision.attribution == Attribution::Agent)
        .count()
}

/// Annotate a ready response with the count of unreviewed agent decisions
/// and a prompt to review them. When every decision is human-confirmed the
/// hint is null, keeping the healthy path quiet.
pub(super) fn with_agent_review_hint(mut response: Value, count: usize) -> Value {
    response["agent_decisions_unreviewed"] = serde_json::json!(count);
    response["hint"] = if count > 0 {
        serde_json::json!(format!(
            "{count} agent decision{} pending review — ask the user to promote or revise before relying on them",
            if count == 1 { "" } else { "s" }
        ))
    } else {
        Value::Null
    };
    response
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
    pub(super) last_touched: String,
    pub(super) age_days: i64,
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::steps;

    #[test]
    fn step_action_instruction_matches_summary_gated() {
        let step = Step::VerifyConstraints;
        for mode in [Mode::Agent, Mode::Interactive] {
            let action = step_action("auth", &step, None, mode);
            let instruction = action["instruction"]
                .as_str()
                .expect("instruction must be a string");
            assert_eq!(
                instruction,
                steps::summary(&step, mode).as_ref(),
                "instruction mismatch for VerifyConstraints in {:?}",
                mode
            );
        }
    }

    #[test]
    fn step_action_instruction_matches_summary_ungated() {
        let step = Step::ScanProject;
        for mode in [Mode::Agent, Mode::Interactive] {
            let action = step_action("project", &step, None, mode);
            let instruction = action["instruction"]
                .as_str()
                .expect("instruction must be a string");
            assert_eq!(
                instruction,
                steps::summary(&step, mode).as_ref(),
                "instruction mismatch for ScanProject in {:?}",
                mode
            );
        }
    }
}
