//! Autonomous bootstrap driver for CLI `trurlic bootstrap -p <provider>`.
//!
//! Loops through `advance(task_type=bootstrap)`, gathers file context for
//! each step, calls the LLM, and records extracted artifacts. No user input
//! — every step is handled autonomously. The advance state machine provides
//! crash recovery: if interrupted, re-running picks up from the current
//! graph state.
//!
//! Unlike the design session driver, bootstrap does not persist a session
//! file — the graph IS the session state.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use crate::provider::{LlmProvider, Message, Role};
use crate::store::schema::Attribution;
use crate::store::{self, RecordDecisionParams, RecordPatternParams, Store};
use crate::workflow::{self, Mode, TaskType, steps};
use crate::{Error, Result};

use super::extract;
use super::files;

const MAX_BOOTSTRAP_ITERATIONS: usize = 200;

// ── Public API ──────────────────────────────────────────────────────────────

/// Run a full bootstrap session: project-wide.
///
/// Loops through advance → step prompt → file context → LLM → record
/// until `ready: true`. Each step acquires and releases the store lock
/// independently so filesystem watchers see changes in real time.
pub(crate) async fn run(
    store: &Store,
    client: &dyn LlmProvider,
    project_root: &Path,
    state: &mut store::ProjectState,
) -> Result<()> {
    let mut completed: BTreeMap<String, String> = BTreeMap::new();

    for _ in 0..MAX_BOOTSTRAP_ITERATIONS {
        let evidence: BTreeMap<&str, &str> = completed
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let result = workflow::advance::advance(
            state,
            "project",
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &evidence,
            chrono::Utc::now(),
        )
        .map_err(Error::Validation)?;

        if result["ready"].as_bool().unwrap_or(false) {
            eprintln!("\nBootstrap complete.");
            return Ok(());
        }

        let step = result["step"].as_str().unwrap_or("ready").to_string();
        let target = result["action"]["target_component"]
            .as_str()
            .map(String::from);

        eprintln!("\n── {} ──", step.replace('_', " "));
        if let Some(ref t) = target {
            eprintln!("   target: {t}");
        }

        let recorded =
            run_step(store, client, project_root, state, &step, target.as_deref()).await?;

        if recorded {
            completed.clear();
        } else {
            completed.insert(step, String::new());
        }
    }
    Err(Error::Validation(format!(
        "bootstrap exceeded {MAX_BOOTSTRAP_ITERATIONS} iterations"
    )))
}

/// Run a bootstrap session for a single component.
pub(crate) async fn run_component(
    store: &Store,
    client: &dyn LlmProvider,
    project_root: &Path,
    state: &mut store::ProjectState,
    component: &str,
) -> Result<()> {
    let mut completed: BTreeMap<String, String> = BTreeMap::new();

    for _ in 0..MAX_BOOTSTRAP_ITERATIONS {
        let evidence: BTreeMap<&str, &str> = completed
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let result = workflow::advance::advance(
            state,
            component,
            Some(TaskType::Bootstrap),
            None,
            Some(Mode::Agent),
            &evidence,
            chrono::Utc::now(),
        )
        .map_err(Error::Validation)?;

        if result["ready"].as_bool().unwrap_or(false) {
            eprintln!("\nBootstrap complete for [{component}].");
            return Ok(());
        }

        let step = result["step"].as_str().unwrap_or("ready").to_string();

        eprintln!("\n── {} [{component}] ──", step.replace('_', " "));

        let recorded = run_step(store, client, project_root, state, &step, Some(component)).await?;

        if recorded {
            completed.clear();
        } else {
            completed.insert(step, String::new());
        }
    }
    Err(Error::Validation(format!(
        "bootstrap for [{component}] exceeded {MAX_BOOTSTRAP_ITERATIONS} iterations"
    )))
}

// ── Step execution ──────────────────────────────────────────────────────────

/// Execute a single bootstrap step: gather context, call LLM, record.
///
/// Returns `true` if any graph artifacts were recorded (decisions,
/// components, patterns), `false` otherwise.
async fn run_step(
    store: &Store,
    client: &dyn LlmProvider,
    project_root: &Path,
    state: &mut store::ProjectState,
    step: &str,
    target: Option<&str>,
) -> Result<bool> {
    // ── Build context ────────────────────────────────────────────────
    let context = match step {
        "scan_project" => files::gather_tree(project_root)?,
        "extract_decisions" => {
            let comp = target.unwrap_or("project");
            files::gather_sources(project_root, comp)?
        }
        "project_rules" => files::gather_project_config(project_root)?,
        _ => String::new(),
    };

    // ── Build system prompt ──────────────────────────────────────────
    let prompt_component = target.unwrap_or("project");
    let prompt = steps::build_step_prompt(
        state,
        prompt_component,
        step,
        None,
        Some("bootstrap"),
        Mode::Agent,
    )
    .map_err(Error::Validation)?;

    // ── LLM call ─────────────────────────────────────────────────────
    let messages = if context.is_empty() {
        vec![]
    } else {
        vec![Message {
            role: Role::User,
            content: context,
        }]
    };

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

    // ── Record artifacts ─────────────────────────────────────────────
    match step {
        "scan_project" => record_scan(store, state, &response),
        "extract_decisions" | "project_rules" => {
            let comp = target.unwrap_or("project");
            record_decisions(store, state, comp, &response)
        }
        "pattern_detection" => record_patterns(store, state, &response),
        _ => Ok(false),
    }
}

// ── Recording helpers ───────────────────────────────────────────────────────

/// Record components and connections from a scan_project response.
fn record_scan(store: &Store, state: &mut store::ProjectState, response: &str) -> Result<bool> {
    let components = extract::extract_components(response);
    let connections = extract::extract_connections(response);

    if components.is_empty() {
        return Ok(false);
    }

    let lock = store.lock()?;
    let mut recorded = false;

    for comp in &components {
        let name = &comp.name;
        let desc = comp.description.as_deref().unwrap_or("");
        match store.add_component(&lock, state, name, desc) {
            Ok(()) => {
                eprintln!("  ✓ component: {name}");
                recorded = true;
            }
            Err(e) => eprintln!("  ⚠ component {name}: {e}"),
        }
    }

    for conn in &connections {
        match store.add_connection(&lock, state, &conn.from, &conn.to) {
            Ok(()) => {
                eprintln!("  ✓ connection: {} → {}", conn.from, conn.to);
                recorded = true;
            }
            Err(e) => eprintln!("  ⚠ connection {} → {}: {e}", conn.from, conn.to),
        }
    }

    Ok(recorded)
}

/// Record decisions from an extract_decisions or project_rules response.
fn record_decisions(
    store: &Store,
    state: &mut store::ProjectState,
    component: &str,
    response: &str,
) -> Result<bool> {
    let decisions = extract::extract_decisions(response);
    if decisions.is_empty() {
        return Ok(false);
    }

    let lock = store.lock()?;
    let mut recorded = false;

    for dec in &decisions {
        match store.record_decision(
            &lock,
            state,
            RecordDecisionParams {
                component,
                choice: &dec.choice,
                reason: &dec.reason,
                alternatives: &dec.alternatives,
                depends_on: &[],
                constrains: &[],
                tags: &[],
                attribution: Attribution::Agent,
                code_refs: &[],
            },
        ) {
            Ok(stem) => {
                eprintln!("  ✓ decision: {stem}");
                recorded = true;
            }
            Err(e) => eprintln!("  ⚠ decision \"{}\": {e}", dec.choice),
        }
    }

    Ok(recorded)
}

/// Record patterns from a pattern_detection response.
fn record_patterns(store: &Store, state: &mut store::ProjectState, response: &str) -> Result<bool> {
    let patterns = extract::extract_patterns(response);
    if patterns.is_empty() {
        return Ok(false);
    }

    let lock = store.lock()?;
    let mut recorded = false;

    for pat in &patterns {
        if pat.decisions.len() < 2 {
            eprintln!("  ⚠ pattern \"{}\": needs ≥2 decisions", pat.name);
            continue;
        }
        match store.record_pattern(
            &lock,
            state,
            RecordPatternParams {
                name: &pat.name,
                description: &pat.description,
                decisions: &pat.decisions,
                components: &[],
                tags: &[],
            },
        ) {
            Ok(_) => {
                eprintln!("  ✓ pattern: {}", pat.name);
                recorded = true;
            }
            Err(e) => eprintln!("  ⚠ pattern \"{}\": {e}", pat.name),
        }
    }

    Ok(recorded)
}
