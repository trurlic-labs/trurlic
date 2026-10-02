//! The decision listings of a step prompt, one entry per decision, pushed
//! as [`PromptText`] listings so a prompt over its budget cuts them and
//! keeps the instructions around them.

use std::sync::Arc;

use crate::store::graph::InMemoryGraph;
use crate::store::schema::{Decision, DecisionFile};
use crate::workflow::Step;
use crate::workflow::concerns;
use crate::workflow::prompt::PromptText;

use super::{format_code_refs_line, sanitize, sanitize_short};

/// Which decisions the EXISTING DECISIONS section lists.
#[derive(Clone, Copy)]
pub(super) enum Existing {
    /// Project rules only: the step lists the component's decisions itself.
    RulesOnly,
    RulesAndDecisions,
}

impl Existing {
    pub(super) const fn for_step(step: &Step) -> Self {
        // The steps that walk the component's decisions one by one.
        if matches!(
            step,
            Step::WalkDecisions | Step::VerifyConstraints | Step::DriftCheck
        ) {
            Self::RulesOnly
        } else {
            Self::RulesAndDecisions
        }
    }
}

/// List existing constraints for context.
pub(super) fn existing_constraints(
    out: &mut PromptText,
    graph: &InMemoryGraph,
    component: &str,
    existing: Existing,
) {
    let project_rules = graph.project_decisions();
    let decisions = match existing {
        Existing::RulesOnly => Vec::new(),
        Existing::RulesAndDecisions => graph.decisions_for(component),
    };
    if project_rules.is_empty() && decisions.is_empty() {
        return;
    }

    let entries = |decisions: &[(&Arc<str>, &DecisionFile)], scope: &str| -> Vec<String> {
        decisions
            .iter()
            .map(|(name, d)| {
                let (choice, reason) = (sanitize(&d.decision.choice), sanitize(&d.decision.reason));
                format!("  {scope}{name}: {choice} ({reason})\n")
            })
            .collect()
    };
    out.push_str("EXISTING DECISIONS (do not re-ask):\n");
    out.push_listing("project rules", entries(&project_rules, "[project] "));
    out.push_listing("decisions", entries(&decisions, ""));
    out.push_str("\n");
}

/// The covered concern areas, each listing the decisions that cover it,
/// then the uncovered ones.
pub(super) fn concern_status(out: &mut PromptText, decisions: &[&DecisionFile]) {
    let (covered, uncovered): (Vec<_>, Vec<_>) = concerns::concern_status(decisions)
        .into_iter()
        .partition(|(_, choices)| !choices.is_empty());

    if !covered.is_empty() {
        out.push_str("COVERED (decisions exist \u{2014} do not re-ask):\n");
        for (name, choices) in &covered {
            out.push_str(&format!("  \u{2713} {name}:\n"));
            let entries = choices
                .iter()
                .map(|choice| format!("    \"{}\"\n", sanitize(choice)))
                .collect();
            out.push_listing("decisions", entries);
        }
        out.push_str("\n");
    }

    if !uncovered.is_empty() {
        out.push_str("UNCOVERED (systematically ask about each):\n");
        for (name, _) in &uncovered {
            out.push_str(&format!("  \u{25a1} {name}\n"));
        }
        out.push_str("\n");
    }
}

/// One decision of an interactive walk: the decision, then the question
/// the task type asks about it.
pub(super) fn walk_entry_interactive(
    name: &str,
    decision: &Decision,
    task_type: Option<&str>,
) -> String {
    let code_line = format_code_refs_line(decision);
    let history_note = decision.history.first().map_or_else(String::new, |first| {
        format!(
            "Revised {} time(s) \u{2014} earliest recorded: \"{}\"\n",
            decision.history.len(),
            sanitize_short(&first.choice, 60),
        )
    });
    let question = match task_type {
        Some("review") => format!(
            "\u{2192} Read the code at these locations\n\
             \u{2192} Ask: \"This decision is from {}. Does the code \
             still match? Has anything drifted?\"\n",
            decision.created.format("%Y-%m-%d"),
        ),
        Some("learn") => "\u{2192} Read the code where this lives\n\
             \u{2192} Ask: \"Why was this approach chosen over the \
             alternatives? What\u{2019}s the trade-off?\"\n"
            .into(),
        _ => format!(
            "\u{2192} Read the code where this lives\n\
             \u{2192} Ask: \"This was decided because of {reason} \u{2014} is \
             that still the right trade-off?\"\n",
            reason = sanitize_short(&decision.reason, 60),
        ),
    };
    format!(
        "DECISION: {name}\n\
         Choice: {choice}\n\
         Reason: {reason}\n\
         {code_line}\
         {history_note}\
         {question}\
         \u{2192} STOP. Wait.\n\n",
        choice = sanitize(&decision.choice),
        reason = sanitize(&decision.reason),
    )
}

/// One decision of an autonomous walk: the decision and where to verify it.
pub(super) fn walk_entry_agent(name: &str, decision: &Decision) -> String {
    let code = format_code_refs_line(decision);
    format!(
        "DECISION: {name} \u{2014} {}\n\
         Reason: {}\n\
         {code}\
         \u{2192} Locate in source code and verify accuracy\n\
         \u{2192} If drifted, call update_decision(mode=\"revise\")\n\n",
        sanitize(&decision.choice),
        sanitize(&decision.reason),
    )
}
