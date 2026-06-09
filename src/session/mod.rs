//! CLI design session driver.
//!
//! Uses `workflow/` for step determination and prompt generation. The session
//! module adds LLM API calls, stdin/stdout dialogue management, and session
//! persistence. It is the only place where LLM calls happen.

mod driver;
pub(crate) mod extract;
pub(crate) mod persistence;

use crate::provider::LlmProvider;
use crate::store::{self, Store};
use crate::workflow::TaskType;
use crate::{Error, Result};

use persistence::Session;

/// Run a design session for a component.
///
/// The session is step-driven: `advance()` determines the current step,
/// `build_step_prompt()` produces focused instructions, and the LLM runs
/// the dialogue. Step transitions happen automatically as the graph changes
/// (decisions recorded → advance returns a different step).
///
/// Acquires an exclusive session lock to prevent concurrent sessions on
/// the same component. The lock is released on completion or error.
pub async fn run_design(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
    continue_session: bool,
    revisit: bool,
    task: Option<&str>,
) -> Result<()> {
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    // ── Exclusive session lock ────────────────────────────────────────
    persistence::try_acquire_lock(store, component)?;

    let result = run_design_inner(store, client, component, continue_session, revisit, task).await;

    // Always release the lock, whether we succeeded or failed.
    persistence::release_lock(store, component);
    result
}

async fn run_design_inner(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
    continue_session: bool,
    revisit: bool,
    task: Option<&str>,
) -> Result<()> {
    let mut session = if continue_session {
        let s = persistence::load(store, component)?;
        eprintln!(
            "Resuming session ({} messages, {} decisions recorded)",
            s.messages.len(),
            s.decisions_recorded.len()
        );
        s
    } else {
        Session::new(component)
    };

    let mut state = store.load_state()?;

    let issues = state.validate();
    let error_count = issues
        .iter()
        .filter(|i| i.severity == crate::store::graph::Severity::Error)
        .count();
    if error_count > 0 {
        eprintln!(
            "warning: .trurl/ has {error_count} consistency issue(s) — run `trurl check` for details"
        );
    }

    if component != "project" && !state.components.contains_key(component) {
        return Err(Error::ComponentNotFound(component.into()));
    }

    // Map CLI flags to task type.
    let task_type = if revisit {
        Some(TaskType::Review)
    } else {
        None // Let advance infer from graph state.
    };

    eprintln!("(Ctrl+D or empty line to save and exit)\n");

    let result = driver::run(
        store,
        client,
        component,
        task_type,
        task,
        &mut session,
        &mut state,
    )
    .await;
    if result.is_err() {
        let _ = persistence::save(store, &session);
        eprintln!("Session saved. Resume with: trurl design {component} --continue");
    }
    result
}
