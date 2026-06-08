mod extract;
mod prompt;
mod session;

use std::io::Write;

use crate::provider::{LlmProvider, Message, Role};
use crate::store::{self, Store};
use crate::{Error, Result};

use extract::extract_decisions;
use session::Session;

/// Run a design conversation for a component.
pub async fn run_design(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
    continue_session: bool,
    revisit: bool,
) -> Result<()> {
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    let mut session = if continue_session {
        let s = session::load(store, component)?;
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
    let system = prompt::build_system_prompt(component, &state, revisit);

    eprintln!("(Ctrl+D or empty line to save and exit)\n");

    let result =
        conversation_loop(store, client, component, &system, &mut session, &mut state).await;
    if result.is_err() {
        let _ = session::save(store, &session);
        eprintln!("Session saved. Resume with: trurl design {component} --continue");
    }
    result
}

async fn conversation_loop(
    store: &Store,
    client: &dyn LlmProvider,
    component: &str,
    system: &str,
    session: &mut Session,
    state: &mut store::ProjectState,
) -> Result<()> {
    let mut messages = session.to_provider_messages();

    loop {
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

        if extract::is_design_complete(&response) {
            eprintln!("\nDesign session complete.");
            session::cleanup(store, component);
            return Ok(());
        }

        session.add_message(Role::Assistant, &response);
        messages.push(Message {
            role: Role::Assistant,
            content: response,
        });
        session::save(store, session)?;

        print!("\n> ");
        let _ = std::io::stdout().flush();

        let input = match read_input()? {
            Some(text) => text,
            None => {
                session::save(store, session)?;
                eprintln!("Session saved. Resume with: trurl design {component} --continue");
                return Ok(());
            }
        };

        messages.push(Message {
            role: Role::User,
            content: input.clone(),
        });
        session.add_message(Role::User, &input);
        session::save(store, session)?;
    }
}

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
