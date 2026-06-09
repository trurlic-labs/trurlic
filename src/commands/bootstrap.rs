use std::path::Path;

use crate::store::ProjectState;
use crate::workflow::{self, TaskType};
use crate::{Error, Result};

use super::{discover_store, open_store};

/// Show bootstrap progress and instructions.
///
/// Runs `advance(component="project", task_type="bootstrap")` internally
/// to determine the current phase, then prints status and clear
/// instructions for the user's coding agent.
///
/// Read-only — no locks, no writes, no LLM calls.
pub fn bootstrap(cwd: &Path) -> Result<()> {
    let (_store, state) = open_store(cwd)?;

    let result =
        workflow::advance::advance(&state, "project", Some(TaskType::Bootstrap), None, &[])
            .map_err(Error::Validation)?;

    let step = result["step"].as_str().unwrap_or("unknown");
    let ready = result["ready"].as_bool().unwrap_or(false);

    print_status(&state);

    if ready {
        println!();
        println!("Bootstrap complete. Review with:");
        println!("  trurlic map");
        println!("  trurlic status");
    } else {
        print_next_step(&result, step);
        print_agent_instructions();
    }

    Ok(())
}

/// Single-component bootstrap: show progress for one component.
pub fn bootstrap_component(cwd: &Path, component: &str) -> Result<()> {
    let (_store, state) = open_store(cwd)?;

    if !state.components.contains_key(component) {
        return Err(Error::ComponentNotFound(component.into()));
    }

    let result =
        workflow::advance::advance(&state, component, Some(TaskType::Bootstrap), None, &[])
            .map_err(Error::Validation)?;

    let step = result["step"].as_str().unwrap_or("unknown");
    let ready = result["ready"].as_bool().unwrap_or(false);
    let graph = state.graph();
    let decisions = graph.decisions_for(component);
    let patterns = graph.patterns_for(component);

    println!("component: {component}");
    println!("  decisions: {}", decisions.len());
    println!("  patterns:  {}", patterns.len());

    if ready {
        println!();
        println!("Component is bootstrapped. Review with:");
        println!("  trurlic design {component} --revisit");
    } else {
        print_next_step(&result, step);
        println!();
        println!("Run via your coding agent:");
        println!();
        println!("  Call advance(component=\"{component}\", task_type=\"bootstrap\")");
        println!("  and follow each step until ready: true.");
    }

    Ok(())
}

/// Run the bootstrap directly using an LLM API.
///
/// Resolves the provider, creates the client, and delegates to the
/// session bootstrap driver which loops advance → LLM → record until
/// `ready: true`.
pub fn bootstrap_direct(
    cwd: &Path,
    component: Option<&str>,
    provider_flag: Option<&str>,
    model_flag: Option<&str>,
) -> Result<()> {
    let store = discover_store(cwd)?;

    let config = crate::config::resolve_provider(provider_flag, model_flag)?;
    let model = config.model.clone();
    let client = crate::provider::create_provider(config)?;
    eprintln!("Using {} ({})", client.provider_name(), model);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|e| Error::Io(std::io::Error::other(e)))?;

    match component {
        Some(c) => rt.block_on(crate::session::run_bootstrap_component(&store, &*client, c)),
        None => rt.block_on(crate::session::run_bootstrap(&store, &*client)),
    }
}

// ── Output helpers ──────────────────────────────────────────────────────────

fn print_status(state: &ProjectState) {
    let graph = state.graph();
    let comp_count = state.components.len();
    let dec_count = state.decisions.len();
    let pat_count = state.patterns.len();
    let project_rules = graph.project_decisions().len();

    let undecided: Vec<&str> = state
        .components
        .keys()
        .filter(|c| graph.decisions_for(c).is_empty())
        .map(String::as_str)
        .collect();

    println!("components:    {comp_count}");
    println!("decisions:     {dec_count} ({project_rules} project-wide)");
    println!("patterns:      {pat_count}");
    if !undecided.is_empty() {
        let shown: Vec<&str> = undecided.iter().take(8).copied().collect();
        let suffix = if undecided.len() > 8 {
            format!(", … +{} more", undecided.len() - 8)
        } else {
            String::new()
        };
        println!("pending:       {}{suffix}", shown.join(", "));
    }
}

fn print_next_step(result: &serde_json::Value, step: &str) {
    println!();
    println!("next step:     {}", step.replace('_', " "));
    if let Some(target) = result["action"]["target_component"].as_str() {
        println!("target:        {target}");
    }
}

fn print_agent_instructions() {
    println!();
    println!("Run via your coding agent:");
    println!();
    println!("  Call advance(component=\"project\", task_type=\"bootstrap\")");
    println!("  and follow each step until ready: true.");
    println!();
    println!("Setup (Claude Code):");
    println!("  claude mcp add trurlic -- trurlic serve");
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, decide, init};
    use tempfile::TempDir;

    #[test]
    fn bootstrap_on_empty_project() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        bootstrap(tmp.path()).unwrap();
    }

    #[test]
    fn bootstrap_with_components() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();
        add_component(tmp.path(), "store", Some("Data persistence")).unwrap();
        bootstrap(tmp.path()).unwrap();
    }

    #[test]
    fn bootstrap_component_rejects_nonexistent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        let err = bootstrap_component(tmp.path(), "ghost").unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn bootstrap_component_with_decisions() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();
        decide(tmp.path(), "auth", "JWT tokens", "Stateless", None, &[]).unwrap();
        bootstrap_component(tmp.path(), "auth").unwrap();
    }
}
