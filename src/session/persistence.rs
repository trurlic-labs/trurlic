use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::provider::{Message, Role};
use crate::store::{STATE_DIR, Store, is_valid_kebab_case};
use crate::{Error, Result};

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    pub component: String,
    pub messages: Vec<SessionMessage>,
    pub decisions_recorded: Vec<String>,
    /// Evidence of user involvement for completed steps. Keys are step
    /// names, values are the user's last input for that step. Cleared
    /// when new decisions are recorded (graph changed).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub step_evidence: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct SessionMessage {
    role: Role,
    content: String,
}

impl Session {
    pub fn new(component: &str) -> Self {
        Self {
            component: component.into(),
            messages: Vec::new(),
            decisions_recorded: Vec::new(),
            step_evidence: BTreeMap::new(),
        }
    }

    pub fn add_message(&mut self, role: Role, content: &str) {
        self.messages.push(SessionMessage {
            role,
            content: content.into(),
        });
    }

    pub fn to_provider_messages(&self) -> Vec<Message> {
        self.messages
            .iter()
            .map(|m| Message {
                role: m.role,
                content: m.content.clone(),
            })
            .collect()
    }
}

// ── Persistence ──────────────────────────────────────────────────────────────

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

    // Atomic write: tmp → verify round-trip → rename.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &content)?;

    let readback = std::fs::read_to_string(&tmp_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        Error::Io(e)
    })?;
    if readback != content {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(Error::Validation(
            "session round-trip verification failed: written content differs".into(),
        ));
    }

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
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
}

// ── Session locking ─────────────────────────────────────────────────────────

/// Acquire an exclusive lock for a design session on this component.
///
/// Creates a `.lock` file atomically via `create_new`. If the lock file
/// already exists, the component has an active session (or a stale lock
/// from a crash). The error message includes the lock path for manual
/// removal.
pub(crate) fn try_acquire_lock(store: &Store, component: &str) -> Result<()> {
    let lock_path = session_path(store, component)?.with_extension("lock");

    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            let _ = write!(file, "{}", std::process::id());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let pid_info = std::fs::read_to_string(&lock_path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .map(|pid| format!(" (PID {pid})"))
                .unwrap_or_default();
            Err(Error::Validation(format!(
                "another session{pid_info} is active for `{component}` — \
                 if the previous session crashed, remove: {}",
                lock_path.display()
            )))
        }
        Err(e) => Err(Error::Io(e)),
    }
}

/// Release the session lock. Called on normal completion and error cleanup.
pub(crate) fn release_lock(store: &Store, component: &str) {
    if let Ok(path) = session_path(store, component) {
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_serializes_and_deserializes() {
        let mut session = Session::new("auth");
        session.add_message(Role::Assistant, "What token format?");
        session.add_message(Role::User, "JWT");
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
        session.add_message(Role::Assistant, "Q?");
        session.add_message(Role::User, "A.");

        let messages = session.to_provider_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[1].content, "A.");
    }

    #[test]
    fn session_path_rejects_traversal() {
        use crate::store::Store;
        let store = Store::at("/tmp/fake/.trurlic".into());
        let err = session_path(&store, "../escape").unwrap_err();
        assert!(matches!(err, Error::InvalidName(_)));
    }

    #[test]
    fn session_path_accepts_project() {
        use crate::store::Store;
        let store = Store::at("/tmp/fake/.trurlic".into());
        let path = session_path(&store, "project").unwrap();
        assert!(path.to_string_lossy().contains("project.json"));
    }

    #[test]
    fn session_path_accepts_kebab_case() {
        use crate::store::Store;
        let store = Store::at("/tmp/fake/.trurlic".into());
        let path = session_path(&store, "rate-limiter").unwrap();
        assert!(path.to_string_lossy().contains("rate-limiter.json"));
    }
}
