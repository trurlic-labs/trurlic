//! Step-by-step dialogue driver for CLI design sessions.
//!
//! The driver uses the workflow engine to determine the current step, builds
//! focused prompts, and runs multi-turn LLM dialogue within each step. Step
//! transitions are driven by `advance()` — when the graph changes (decisions
//! recorded, patterns added), the next advance call returns a different step.
//!
//! The session driver is the only place where LLM calls happen. The workflow
//! engine never calls an LLM.

use std::io::Write;

use std::collections::BTreeMap;

use crate::provider::{LlmProvider, Message, Role};
use crate::store::{self, Store};
use crate::workflow::{self, Mode, TaskType};
use crate::{Error, Result};

use super::extract::{self, extract_decisions};
use super::persistence::{self, Session};

/// Warn when message count exceeds this threshold.
const MESSAGE_COUNT_WARNING: usize = 80;

// ── Context bundle ───────────────────────────────────────────────────────

struct SessionContext<'a> {
    store: &'a Store,
    client: &'a dyn LlmProvider,
    component: &'a str,
    task_type: Option<TaskType>,
    task: Option<&'a str>,
}

// ── Public API ────────────────────────────────────────────────────────────

/// Run a step-by-step design session for a component.
///
/// Calls `advance()` to determine each step, builds focused prompts via
/// `workflow::steps`, and runs LLM dialogue until `ready: true`. Session
/// state is persisted after every exchange for crash recovery.
///
/// Progression through steps without verifiable graph postconditions
/// (e.g. `VerifyConstraints`, `WalkDecisions`, `CoverageAudit`) relies
/// on `session.completed_steps`, which is passed to `advance()`. If a
/// step completes without changing the graph, it is added to the set;
/// `advance()` then skips it and returns the next step in the sequence.
/// A graph change clears the set so the full sequence re-evaluates.
pub(crate) async fn run(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
    task_type: Option<TaskType>,
    task: Option<&str>,
    session: &mut Session,
    state: &mut store::ProjectState,
) -> Result<()> {
    let ctx = SessionContext {
        store,
        client,
        component,
        task_type,
        task,
    };
    let mut messages = session.to_provider_messages();

    loop {
        // ── Advance: determine current step ───────────────────────────
        // Clone into owned strings so `evidence` does not borrow `session`,
        // which must remain mutably accessible for add_message / persist.
        let evidence_owned: BTreeMap<String, String> = session
            .step_evidence
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let evidence: BTreeMap<&str, &str> = evidence_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let advance_result = workflow::advance::advance(
            state,
            ctx.component,
            ctx.task_type,
            ctx.task,
            Some(Mode::Interactive),
            &evidence,
        )
        .map_err(Error::Validation)?;

        let ready = advance_result["ready"].as_bool().unwrap_or(false);
        if ready {
            eprintln!("\nDesign session complete — component is ready.");
            persistence::cleanup(ctx.store, ctx.component);
            return Ok(());
        }

        let step = advance_result["step"]
            .as_str()
            .unwrap_or("ready")
            .to_string();

        // ── Loop detection: skip already-completed steps ──────────────
        if session.step_evidence.contains_key(&step) {
            eprintln!("\nAll reachable steps complete.");
            persistence::cleanup(ctx.store, ctx.component);
            return Ok(());
        }

        // ── Build step prompt ─────────────────────────────────────────
        let tt_str = ctx.task_type.map(|tt| tt.as_str());
        let prompt = workflow::steps::build_step_prompt(
            state,
            ctx.component,
            &step,
            ctx.task,
            tt_str,
            Mode::Interactive,
        )
        .map_err(Error::Validation)?;

        let step_label = step.replace('_', " ");
        eprintln!("\n── {step_label} ──");

        // ── Run dialogue for this step ────────────────────────────────
        let decisions_before = session.decisions_recorded.len();
        let step_result = run_step_dialogue(
            &ctx,
            session,
            state,
            &mut messages,
            &step,
            &evidence,
            &prompt.instructions,
        )
        .await?;

        match step_result {
            StepOutcome::Complete => return Ok(()),
            StepOutcome::StepChanged { last_user_input } => {
                // ── Step completed ────────────────────────────────────
                let graph_changed = session.decisions_recorded.len() > decisions_before;
                if graph_changed {
                    session.step_evidence.clear();
                }
                session.step_evidence.insert(step, last_user_input);
                persistence::save(ctx.store, session)?;
            }
        }
    }
}

// ── Step dialogue ────────────────────────────────────────────────────────

enum StepOutcome {
    Complete,
    StepChanged { last_user_input: String },
}

async fn run_step_dialogue(
    ctx: &SessionContext<'_>,
    session: &mut Session,
    state: &mut store::ProjectState,
    messages: &mut Vec<Message>,
    step: &str,
    step_evidence: &BTreeMap<&str, &str>,
    system_prompt: &str,
) -> Result<StepOutcome> {
    let mut last_user_input = String::new();
    loop {
        if messages.len() >= MESSAGE_COUNT_WARNING && messages.len().is_multiple_of(20) {
            eprintln!(
                "warning: session has {} messages — consider saving and starting fresh",
                messages.len(),
            );
        }

        let response = {
            let result = ctx
                .client
                .stream_completion(messages, system_prompt, &mut |chunk| {
                    print!("{chunk}");
                    let _ = std::io::stdout().flush();
                })
                .await;
            println!();
            result?
        };

        // ── Extract and record decisions ──────────────────────────
        for dec in extract_decisions(&response) {
            let stem = extract::record_decision(
                ctx.store,
                state,
                ctx.component,
                &dec.choice,
                &dec.reason,
                &dec.alternatives,
            )?;
            session.decisions_recorded.push(stem.clone());
            eprintln!("  ✓ recorded: {stem}");
        }

        // ── DESIGN_COMPLETE signal ────────────────────────────────
        if extract::is_design_complete(&response) {
            eprintln!("\nDesign session complete.");
            persistence::cleanup(ctx.store, ctx.component);
            return Ok(StepOutcome::Complete);
        }

        // ── Persist ───────────────────────────────────────────────
        session.add_message(Role::Assistant, &response);
        messages.push(Message {
            role: Role::Assistant,
            content: response,
        });
        persistence::save(ctx.store, session)?;

        // ── Check if step changed ─────────────────────────────────
        let re_advance = workflow::advance::advance(
            state,
            ctx.component,
            ctx.task_type,
            ctx.task,
            Some(Mode::Interactive),
            step_evidence,
        )
        .map_err(Error::Validation)?;

        let new_ready = re_advance["ready"].as_bool().unwrap_or(false);
        if new_ready {
            eprintln!("\nDesign session complete — component is ready.");
            persistence::cleanup(ctx.store, ctx.component);
            return Ok(StepOutcome::Complete);
        }
        let new_step = re_advance["step"].as_str().unwrap_or("ready");
        if new_step != step {
            return Ok(StepOutcome::StepChanged { last_user_input });
        }

        // ── User input ────────────────────────────────────────────
        print!("\n> ");
        let _ = std::io::stdout().flush();

        let input = match read_input()? {
            Some(text) => text,
            None => {
                persistence::save(ctx.store, session)?;
                eprintln!(
                    "Session saved. Resume with: trurlic design {} --continue",
                    ctx.component,
                );
                return Ok(StepOutcome::Complete);
            }
        };

        last_user_input = input.clone();
        messages.push(Message {
            role: Role::User,
            content: input.clone(),
        });
        session.add_message(Role::User, &input);
        persistence::save(ctx.store, session)?;
    }
}

// ── Input ─────────────────────────────────────────────────────────────────

/// Read a line from stdin. Returns `None` on EOF or empty input.
fn read_input() -> Result<Option<String>> {
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => Ok(None),
        Ok(_) => {
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(e) => Err(Error::Io(e)),
    }
}
