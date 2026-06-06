//! Session persistence for `--continue` support.
//!
//! Sessions are stored as JSON in `.trurl/.state/sessions/<component>.json`.
//! Writes are atomic (tmp + rename) so a crash never truncates the session.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::provider::{Message, Role};
use crate::store::{STATE_DIR, Store, is_valid_kebab_case};
use crate::{Error, Result};

// ── Types ────────────────────────────────────────────────────────────────────

/// Persisted conversation state for `--continue` support.
#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    pub component: String,
    pub messages: Vec<SessionMessage>,
    pub decisions_recorded: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct SessionMessage {
    role: String,
    content: String,
}

impl Session {
    pub fn new(component: &str) -> Self {
        Self {
            component: component.into(),
            messages: Vec::new(),
            decisions_recorded: Vec::new(),
        }
    }

    pub fn add_message(&mut self, role: &str, content: &str) {
        self.messages.push(SessionMessage {
            role: role.into(),
            content: content.into(),
        });
    }

    pub fn to_provider_messages(&self) -> Vec<Message> {
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

// ── Persistence ──────────────────────────────────────────────────────────────

/// Build the session file path, validating the component name to prevent
/// path traversal.
fn session_path(store: &Store, component: &str) -> Result<PathBuf> {
    if component != "project" && !is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }
    Ok(store
        .root()
        .join(STATE_DIR)
        .join("sessions")
        .join(format!("{component}.json")))
}

pub(crate) fn save(store: &Store, session: &Session) -> Result<()> {
    let path = session_path(store, &session.component)?;
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

pub(crate) fn load(store: &Store, component: &str) -> Result<Session> {
    let path = session_path(store, component)?;
    if !path.exists() {
        return Err(Error::Validation(format!(
            "no session for `{component}` — run without --continue to start fresh"
        )));
    }
    let content = std::fs::read_to_string(&path)?;
    serde_json::from_str(&content)
        .map_err(|e| Error::Validation(format!("corrupted session file: {e}")))
}

pub(crate) fn cleanup(store: &Store, component: &str) {
    if let Ok(path) = session_path(store, component) {
        let _ = std::fs::remove_file(path);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn session_path_rejects_traversal() {
        use crate::store::Store;
        let store = Store::at("/tmp/fake/.trurl".into());
        let err = session_path(&store, "../escape").unwrap_err();
        assert!(matches!(err, Error::InvalidName(_)));
    }

    #[test]
    fn session_path_accepts_project() {
        use crate::store::Store;
        let store = Store::at("/tmp/fake/.trurl".into());
        let path = session_path(&store, "project").unwrap();
        assert!(path.to_string_lossy().contains("project.json"));
    }

    #[test]
    fn session_path_accepts_kebab_case() {
        use crate::store::Store;
        let store = Store::at("/tmp/fake/.trurl".into());
        let path = session_path(&store, "rate-limiter").unwrap();
        assert!(path.to_string_lossy().contains("rate-limiter.json"));
    }
}
