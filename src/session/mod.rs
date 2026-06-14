//! CLI design session driver.
//!
//! Uses `workflow/` for step determination and prompt generation. The session
//! module adds LLM API calls, stdin/stdout dialogue management, and session
//! persistence. It is the only place where LLM calls happen.

pub(crate) mod bootstrap;
mod driver;
pub(crate) mod extract;
pub(crate) mod files;
pub(crate) mod persistence;

use crate::provider::LlmProvider;
use crate::store::{self, Store};
use crate::workflow::TaskType;
use crate::{Error, Result};

use persistence::Session;

/// How the design session should start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Start a fresh session, inferring the task type from graph state.
    Fresh,
    /// Resume a previously saved session.
    Continue,
    /// Start fresh but force the Review task type to challenge existing decisions.
    Revisit,
}

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
    mode: SessionMode,
    task: Option<&str>,
) -> Result<()> {
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    // ── Exclusive session lock ────────────────────────────────────────
    persistence::try_acquire_lock(store, component)?;

    let result = run_design_inner(store, client, component, mode, task).await;

    // Always release the lock, whether we succeeded or failed.
    persistence::release_lock(store, component);
    result
}

async fn run_design_inner(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
    mode: SessionMode,
    task: Option<&str>,
) -> Result<()> {
    let mut session = if mode == SessionMode::Continue {
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
            "warning: .trurlic/ has {error_count} consistency issue(s) — run `trurlic check` for details"
        );
    }

    if component != "project" && !state.components.contains_key(component) {
        return Err(Error::ComponentNotFound(component.into()));
    }

    let task_type = match mode {
        SessionMode::Revisit => Some(TaskType::Review),
        _ => None,
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
        eprintln!("Session saved. Resume with: trurlic design {component} --continue");
    }
    result
}

/// Run an autonomous bootstrap for the full project.
///
/// No interactive dialogue — the LLM reads source files (provided as
/// context) and records components, decisions, and patterns directly.
/// Crash recovery is implicit: `advance()` deduces the next step from
/// the graph, so re-running picks up where it left off.
pub async fn run_bootstrap(store: &Store, client: &dyn LlmProvider) -> Result<()> {
    let project_root = store
        .root()
        .parent()
        .ok_or_else(|| Error::Validation("cannot determine project root".into()))?
        .to_path_buf();

    let mut state = store.load_state()?;

    let issues = state.validate();
    let error_count = issues
        .iter()
        .filter(|i| i.severity == crate::store::graph::Severity::Error)
        .count();
    if error_count > 0 {
        eprintln!(
            "warning: .trurlic/ has {error_count} consistency issue(s) — run `trurlic check` for details"
        );
    }

    bootstrap::run(store, client, &project_root, &mut state).await
}

/// Run an autonomous bootstrap for a single component.
pub async fn run_bootstrap_component(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
) -> Result<()> {
    if !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    let project_root = store
        .root()
        .parent()
        .ok_or_else(|| Error::Validation("cannot determine project root".into()))?
        .to_path_buf();

    let mut state = store.load_state()?;

    if !state.components.contains_key(component) {
        return Err(Error::ComponentNotFound(component.into()));
    }

    bootstrap::run_component(store, client, &project_root, &mut state, component).await
}
