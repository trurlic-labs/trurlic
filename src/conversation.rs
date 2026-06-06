//! Conversational design engine for `trurl design`.
//!
//! Drives a Socratic design conversation with an LLM, recording each
//! answered question as a decision in `.trurl/`. Session state persists
//! in `.trurl/.state/sessions/` for `--continue` support.
//!
//! # Flow
//!
//! 1. Build system prompt with component context + existing decisions
//! 2. Stream LLM response (question) to terminal
//! 3. Extract inline decision JSON, write to `.trurl/decisions/` immediately
//! 4. Read user answer from stdin
//! 5. Repeat until `DESIGN_COMPLETE` or user exits (Ctrl+D / empty)
//!
//! On any error, the session is saved automatically for recovery.

use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands;
use crate::provider::{LlmClient, Message, Role};
use crate::schema::{Decision, DecisionFile, STATE_DIR};
use crate::store::{self, Store};
use crate::{Error, Result};

// ── Session types ────────────────────────────────────────────────────────────

/// Persisted conversation state for `--continue` support.
#[derive(Serialize, Deserialize)]
struct Session {
    component: String,
    messages: Vec<SessionMessage>,
    decisions_recorded: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SessionMessage {
    role: String,
    content: String,
}

impl Session {
    fn new(component: &str) -> Self {
        Self {
            component: component.into(),
            messages: Vec::new(),
            decisions_recorded: Vec::new(),
        }
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.messages.push(SessionMessage {
            role: role.into(),
            content: content.into(),
        });
    }

    fn to_provider_messages(&self) -> Vec<Message> {
        self.messages
            .iter()
            .map(|m| Message {
                role: if m.role == "user" {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: m.content.clone(),
            })
            .collect()
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Run a design conversation for a component.
///
/// Manages the full lifecycle: session loading, conversation loop, session
/// cleanup on completion. On any error, the session is saved for `--continue`.
pub async fn run_design(
    store: &Store,
    client: &LlmClient,
    component: &str,
    continue_session: bool,
    revisit: bool,
) -> Result<()> {
    // Defense-in-depth: validate before any session I/O keyed on this name.
    // The caller (commands::design) validates too, but this function is pub.
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    // Load or create session
    let mut session = if continue_session {
        let s = load_session(store, component)?;
        eprintln!(
            "Resuming session ({} messages, {} decisions recorded)",
            s.messages.len(),
            s.decisions_recorded.len()
        );
        s
    } else {
        Session::new(component)
    };

    // Load state once — reused across the entire conversation.
    let mut state = store.load_state()?;

    // Startup courtesy diagnostic (same as other commands).
    let issues = state.validate();
    if !issues.is_empty() {
        eprintln!(
            "warning: .trurl/ has {} consistency issue(s) — run `trurl check` for details",
            issues.len()
        );
    }

    // Build system prompt from current project state
    if component != "project" && !state.components.contains_key(component) {
        return Err(Error::Validation(format!(
            "component `{component}` does not exist"
        )));
    }
    let system = build_system_prompt(component, &state, revisit);

    eprintln!("(Ctrl+D or empty line to save and exit)\n");

    // Run conversation — save session on any error
    let result =
        conversation_loop(store, client, component, &system, &mut session, &mut state).await;
    if result.is_err() {
        let _ = save_session(store, &session);
        eprintln!("Session saved. Resume with: trurl design {component} --continue");
    }
    result
}

// ── Conversation loop ────────────────────────────────────────────────────────

async fn conversation_loop(
    store: &Store,
    client: &LlmClient,
    component: &str,
    system: &str,
    session: &mut Session,
    state: &mut store::ProjectState,
) -> Result<()> {
    let mut messages = session.to_provider_messages();

    loop {
        // Stream LLM response to terminal
        let response = {
            let result = client
                .stream_completion(&messages, system, &mut |chunk| {
                    print!("{chunk}");
                    let _ = std::io::stdout().flush();
                })
                .await;
            println!();
            result?
        };

        // Extract and write decisions from the response
        for dec in extract_decisions(&response) {
            let stem = record_decision(
                store,
                state,
                component,
                &dec.choice,
                &dec.reason,
                &dec.alternatives,
                dec.supersedes.as_deref(),
            )?;
            session.decisions_recorded.push(stem.clone());
            eprintln!("  ✓ recorded: {stem}");
        }

        // Check for completion
        if is_design_complete(&response) {
            eprintln!("\nDesign session complete.");
            cleanup_session(store, component);
            return Ok(());
        }

        // Update session with assistant response
        messages.push(Message {
            role: Role::Assistant,
            content: response,
        });
        session.add_message("assistant", messages.last().map_or("", |m| &m.content));
        save_session(store, session)?;

        // Read user input
        print!("\n> ");
        let _ = std::io::stdout().flush();

        let input = match read_input()? {
            Some(text) => text,
            None => {
                save_session(store, session)?;
                eprintln!("Session saved. Resume with: trurl design {component} --continue");
                return Ok(());
            }
        };

        // Update session with user input
        messages.push(Message {
            role: Role::User,
            content: input.clone(),
        });
        session.add_message("user", &input);
        save_session(store, session)?;
    }
}

// ── System prompt ────────────────────────────────────────────────────────────

fn build_system_prompt(component: &str, state: &store::ProjectState, revisit: bool) -> String {
    let mut p = String::with_capacity(2048);

    p.push_str(
        "You are Trurl, a meticulous architectural design assistant. \
         You conduct focused Socratic design conversations, one question at a time.\n\n",
    );

    // Component context
    if let Some(comp) = state.components.get(component) {
        p.push_str(&format!("## Component: {}\n", comp.component.name));
        if !comp.component.description.is_empty() {
            p.push_str(&format!("Description: {}\n", comp.component.description));
        }
        if !comp.component.connects_to.is_empty() {
            p.push_str(&format!(
                "Connects to: {}\n",
                comp.component.connects_to.join(", ")
            ));
        }
        p.push('\n');
    } else if component == "project" {
        p.push_str(&format!("## Project: {}\n\n", state.project.project.name));
    }

    // Existing decisions for this component
    let comp_decisions: Vec<_> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == component)
        .collect();

    if !comp_decisions.is_empty() {
        p.push_str("## Existing decisions for this component\n");
        for (name, dec) in &comp_decisions {
            p.push_str(&format!(
                "- {}: {} (reason: {})\n",
                name, dec.decision.choice, dec.decision.reason
            ));
        }
        p.push('\n');
    }

    // Project-wide decisions
    if component != "project" {
        let project_decisions: Vec<_> = state
            .decisions
            .iter()
            .filter(|(_, d)| d.decision.component == "project")
            .collect();

        if !project_decisions.is_empty() {
            p.push_str("## Project-wide decisions (apply everywhere)\n");
            for (name, dec) in &project_decisions {
                p.push_str(&format!(
                    "- {}: {} (reason: {})\n",
                    name, dec.decision.choice, dec.decision.reason
                ));
            }
            p.push('\n');
        }
    }

    // Instructions
    if revisit {
        p.push_str(
            "## Mode: Revisit\n\
             Challenge each existing decision. Ask if the reasoning still holds \
             and if better alternatives exist. For changed decisions, output the \
             new decision JSON with a \"supersedes\" field naming the decision \
             being replaced (e.g. \"supersedes\": \"auth-token-format\"). \
             Skip decisions the user wants to keep.\n\n",
        );
    }

    p.push_str(
        "## Instructions\n\n\
         Ask ONE design question at a time. After the user answers, summarize \
         their decision as a JSON object on its own line:\n\n\
         {\"choice\": \"concise decision title\", \"reason\": \"the reasoning\", \
         \"alternatives\": [\"Option A — rejected: why\"]}\n\n\
         Include \"alternatives\" only when other options were discussed or \
         are worth noting. Omit the field when there are none.\n\n\
         Then continue with the next question. Cover key technical choices, \
         patterns, constraints, and integration points. Reference existing \
         decisions and connections for consistency.\n\n\
         When all important design aspects are covered, output DESIGN_COMPLETE \
         on its own line.\n",
    );

    p
}

// ── Decision extraction ──────────────────────────────────────────────────────

/// A decision parsed from an LLM response line.
struct ExtractedDecision {
    choice: String,
    reason: String,
    alternatives: Vec<String>,
    supersedes: Option<String>,
}

/// Extract decision JSON objects from an LLM response.
///
/// Looks for lines containing `{"choice": "...", "reason": "..."}` with
/// optional `"alternatives"` array and `"supersedes"` string (revisit mode).
fn extract_decisions(response: &str) -> Vec<ExtractedDecision> {
    let mut decisions = Vec::new();

    for line in response.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
            if let (Some(choice), Some(reason)) = (
                json.get("choice").and_then(|v| v.as_str()),
                json.get("reason").and_then(|v| v.as_str()),
            ) {
                if !choice.is_empty() && !reason.is_empty() {
                    let alternatives = json
                        .get("alternatives")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();

                    let supersedes = json
                        .get("supersedes")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(String::from);

                    decisions.push(ExtractedDecision {
                        choice: choice.to_string(),
                        reason: reason.to_string(),
                        alternatives,
                        supersedes,
                    });
                }
            }
        }
    }

    decisions
}

/// Check if the response signals design completion.
fn is_design_complete(response: &str) -> bool {
    response
        .lines()
        .any(|line| line.trim() == "DESIGN_COMPLETE")
}

// ── Decision recording ───────────────────────────────────────────────────────

/// Write a single decision to the store, with full validation.
///
/// Uses the caller's cached [`ProjectState`] — no re-load from disk.
/// On success, `state` is updated in-place so subsequent calls see
/// the new decision. On failure, `state` is rolled back.
///
/// If `supersedes` names a decision that doesn't exist (LLM hallucination),
/// it is dropped with a warning rather than failing the entire write —
/// the choice and reason are still valuable.
fn record_decision(
    store: &Store,
    state: &mut store::ProjectState,
    component: &str,
    choice: &str,
    reason: &str,
    alternatives: &[String],
    supersedes: Option<&str>,
) -> Result<String> {
    // Validate supersedes target — warn and drop if the LLM hallucinated
    let validated_supersedes = match supersedes {
        Some(target) if state.decisions.contains_key(target) => Some(target.to_string()),
        Some(target) => {
            eprintln!("  ⚠ ignoring supersedes `{target}` — decision not found");
            None
        }
        None => None,
    };

    let stem = commands::unique_decision_stem(&state.decisions, &commands::slugify(choice));

    let decision = DecisionFile {
        decision: Decision {
            component: component.into(),
            choice: choice.into(),
            reason: reason.into(),
            alternatives: alternatives.to_vec(),
            created: Utc::now(),
            supersedes: validated_supersedes,
        },
    };

    // Insert into in-memory state for validation
    state.decisions.insert(stem.clone(), decision.clone());

    if let Err(e) = commands::validate_mutation(state) {
        state.decisions.remove(&stem);
        return Err(e);
    }

    // Acquire lock only for the write, release immediately after
    let lock = store.lock()?;
    if let Err(e) = store.write_atomic(&lock, &store.decision_path(&stem), &decision) {
        state.decisions.remove(&stem);
        return Err(e);
    }

    Ok(stem)
}

// ── User I/O ─────────────────────────────────────────────────────────────────

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

// ── Session persistence ──────────────────────────────────────────────────────

fn session_path(store: &Store, component: &str) -> PathBuf {
    store
        .root()
        .join(STATE_DIR)
        .join("sessions")
        .join(format!("{component}.json"))
}

fn save_session(store: &Store, session: &Session) -> Result<()> {
    let path = session_path(store, &session.component);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(session)
        .map_err(|e| Error::Validation(format!("session serialization failed: {e}")))?;

    // Atomic write: tmp file then rename, so a crash never truncates the session.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, content)?;
    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(Error::Io(e));
    }
    Ok(())
}

fn load_session(store: &Store, component: &str) -> Result<Session> {
    let path = session_path(store, component);
    if !path.exists() {
        return Err(Error::Validation(format!(
            "no session for `{component}` — run without --continue to start fresh"
        )));
    }
    let content = std::fs::read_to_string(&path)?;
    serde_json::from_str(&content)
        .map_err(|e| Error::Validation(format!("corrupted session file: {e}")))
}

fn cleanup_session(store: &Store, component: &str) {
    let path = session_path(store, component);
    let _ = std::fs::remove_file(path);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Component, ComponentFile, Project, ProjectFile};
    use std::collections::BTreeMap;

    // ── extract_decisions ────────────────────────────────────────────────

    #[test]
    fn extracts_decision_json() {
        let response = "Great question!\n\
            {\"choice\": \"Use JWT\", \"reason\": \"Stateless auth\"}\n\
            Next, let's talk about storage.";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].choice, "Use JWT");
        assert_eq!(decisions[0].reason, "Stateless auth");
        assert!(decisions[0].alternatives.is_empty());
        assert!(decisions[0].supersedes.is_none());
    }

    #[test]
    fn extracts_decision_with_alternatives() {
        let response = "{\"choice\": \"Redis\", \"reason\": \"Persistent and fast\", \
            \"alternatives\": [\"Memcached — rejected: no persistence\", \
            \"In-memory — rejected: lost on restart\"]}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].choice, "Redis");
        assert_eq!(decisions[0].alternatives.len(), 2);
        assert!(decisions[0].alternatives[0].contains("Memcached"));
        assert!(decisions[0].alternatives[1].contains("In-memory"));
        assert!(decisions[0].supersedes.is_none());
    }

    #[test]
    fn extracts_decision_with_supersedes() {
        let response = "{\"choice\": \"Session cookies\", \"reason\": \"Simpler\", \
            \"supersedes\": \"auth-token-format\"}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions[0].supersedes.as_deref(),
            Some("auth-token-format")
        );
    }

    #[test]
    fn extracts_decision_ignores_empty_supersedes() {
        let response = "{\"choice\": \"X\", \"reason\": \"Y\", \"supersedes\": \"\"}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].supersedes.is_none());
    }

    #[test]
    fn extracts_multiple_decisions() {
        let response = "{\"choice\": \"A\", \"reason\": \"R1\"}\ntext\n\
            {\"choice\": \"B\", \"reason\": \"R2\"}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 2);
    }

    #[test]
    fn ignores_non_decision_json() {
        let response = "{\"type\": \"greeting\", \"text\": \"hello\"}";
        assert!(extract_decisions(response).is_empty());
    }

    #[test]
    fn ignores_plain_text() {
        let response = "What token format will you use?";
        assert!(extract_decisions(response).is_empty());
    }

    #[test]
    fn ignores_empty_choice_or_reason() {
        let response = "{\"choice\": \"\", \"reason\": \"something\"}";
        assert!(extract_decisions(response).is_empty());
    }

    #[test]
    fn handles_whitespace_around_json() {
        let response = "  {\"choice\": \"X\", \"reason\": \"Y\"}  ";
        assert_eq!(extract_decisions(response).len(), 1);
    }

    // ── is_design_complete ──────────────────────────────────────────────

    #[test]
    fn detects_completion() {
        assert!(is_design_complete("all done\nDESIGN_COMPLETE\n"));
        assert!(is_design_complete("DESIGN_COMPLETE"));
        assert!(is_design_complete("  DESIGN_COMPLETE  "));
    }

    #[test]
    fn no_false_completion() {
        assert!(!is_design_complete("the DESIGN_COMPLETE flag"));
        assert!(!is_design_complete("almost done"));
    }

    // ── build_system_prompt ─────────────────────────────────────────────

    fn test_state() -> store::ProjectState {
        let mut components = BTreeMap::new();
        components.insert(
            "auth".into(),
            ComponentFile {
                component: Component {
                    name: "auth".into(),
                    description: "Authentication service".into(),
                    connects_to: vec!["database".into()],
                },
            },
        );

        let project = ProjectFile {
            trurl_version: "0.1.0".into(),
            project: Project {
                name: "test-project".into(),
                description: String::new(),
            },
        };

        store::ProjectState {
            project,
            components,
            decisions: BTreeMap::new(),
        }
    }

    #[test]
    fn system_prompt_includes_component_context() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("auth"), "should mention component name");
        assert!(prompt.contains("database"), "should mention connections");
        assert!(prompt.contains("Authentication service"));
    }

    #[test]
    fn system_prompt_includes_instructions() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, false);
        assert!(prompt.contains("DESIGN_COMPLETE"));
        assert!(prompt.contains("\"choice\""));
        assert!(prompt.contains("\"alternatives\""));
    }

    #[test]
    fn system_prompt_revisit_mode() {
        let state = test_state();
        let prompt = build_system_prompt("auth", &state, true);
        assert!(prompt.contains("Revisit"));
        assert!(prompt.contains("Challenge"));
        assert!(prompt.contains("\"supersedes\""));
    }

    #[test]
    fn system_prompt_project_wide() {
        let state = test_state();
        let prompt = build_system_prompt("project", &state, false);
        assert!(prompt.contains("test-project"));
    }

    // ── Session round-trip ──────────────────────────────────────────────

    #[test]
    fn session_serializes_and_deserializes() {
        let mut session = Session::new("auth");
        session.add_message("assistant", "What token format?");
        session.add_message("user", "JWT");
        session.decisions_recorded.push("use-jwt".into());

        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.component, "auth");
        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.decisions_recorded, vec!["use-jwt"]);
    }

    #[test]
    fn session_to_provider_messages() {
        let mut session = Session::new("auth");
        session.add_message("assistant", "Q?");
        session.add_message("user", "A.");

        let messages = session.to_provider_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[1].content, "A.");
    }
}
