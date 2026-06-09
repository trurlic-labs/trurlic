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

use crate::provider::{LlmProvider, Message, Role};
use crate::store::{self, Store};
use crate::workflow::{self, TaskType};
use crate::{Error, Result};

use super::extract::{self, extract_decisions};
use super::persistence::{self, Session};

// ── Public API ────────────────────────────────────────────────────────────

/// Run a step-by-step design session for a component.
///
/// Calls `advance()` to determine each step, builds focused prompts via
/// `workflow::steps`, and runs LLM dialogue until `ready: true`. Session
/// state is persisted after every exchange for crash recovery.
pub(crate) async fn run(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
    task_type: Option<TaskType>,
    session: &mut Session,
    state: &mut store::ProjectState,
) -> Result<()> {
    let mut messages = session.to_provider_messages();

    loop {
        // ── Advance: determine current step ───────────────────────────
        let advance_result = workflow::advance::advance(state, component, task_type, None)
            .map_err(Error::Validation)?;

        let ready = advance_result["ready"].as_bool().unwrap_or(false);
        if ready {
            eprintln!("\nDesign session complete — component is ready.");
            persistence::cleanup(store, component);
            return Ok(());
        }

        let step = advance_result["step"]
            .as_str()
            .unwrap_or("ready")
            .to_string();

        // ── Build step prompt ─────────────────────────────────────────
        let prompt = workflow::steps::build_step_prompt(state, component, &step, None)
            .map_err(Error::Validation)?;

        let step_label = step.replace('_', " ");
        eprintln!("\n── {step_label} ──");

        // ── Step dialogue loop ────────────────────────────────────────
        // Run LLM exchanges until advance returns a different step
        // (i.e., the graph changed enough to progress the workflow).
        loop {
            let response = {
                let result = client
                    .stream_completion(&messages, &prompt.instructions, &mut |chunk| {
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
                    store,
                    state,
                    component,
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
                persistence::cleanup(store, component);
                return Ok(());
            }

            // ── Persist ───────────────────────────────────────────────
            session.add_message(Role::Assistant, &response);
            messages.push(Message {
                role: Role::Assistant,
                content: response,
            });
            persistence::save(store, session)?;

            // ── Check if step changed ─────────────────────────────────
            let re_advance = workflow::advance::advance(state, component, task_type, None)
                .map_err(Error::Validation)?;

            let new_ready = re_advance["ready"].as_bool().unwrap_or(false);
            if new_ready {
                eprintln!("\nDesign session complete — component is ready.");
                persistence::cleanup(store, component);
                return Ok(());
            }
            let new_step = re_advance["step"].as_str().unwrap_or("ready");
            if new_step != step {
                // Step changed — break inner loop, outer loop picks up new step.
                break;
            }

            // ── User input ────────────────────────────────────────────
            print!("\n> ");
            let _ = std::io::stdout().flush();

            let input = match read_input()? {
                Some(text) => text,
                None => {
                    persistence::save(store, session)?;
                    eprintln!("Session saved. Resume with: trurl design {component} --continue");
                    return Ok(());
                }
            };

            messages.push(Message {
                role: Role::User,
                content: input.clone(),
            });
            session.add_message(Role::User, &input);
            persistence::save(store, session)?;
        }
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
