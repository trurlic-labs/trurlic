//! In-memory project state and validation.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::{Error, Result};

use super::schema::{ComponentFile, DecisionFile, ProjectFile};

// ── ProjectState ─────────────────────────────────────────────────────────────

/// Complete in-memory snapshot of `.trurl/`.
///
/// Keyed by filename stem (e.g. `"auth"`, `"error-strategy"`).
pub struct ProjectState {
    pub project: ProjectFile,
    pub components: BTreeMap<String, ComponentFile>,
    pub decisions: BTreeMap<String, DecisionFile>,
}

impl ProjectState {
    /// Validate referential integrity and schema constraints.
    /// Returns a list of issues (empty = clean).
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        for (filename, comp) in &self.components {
            let name = &comp.component.name;

            if filename != name {
                issues.push(format!(
                    "component file `{filename}.toml` has internal name `{name}`"
                ));
            }

            if !is_valid_kebab_case(name) {
                issues.push(format!(
                    "component `{filename}` has invalid name `{name}` (must be kebab-case)"
                ));
            }

            let mut seen = HashSet::new();
            for target in &comp.component.connects_to {
                if !self.components.contains_key(target) {
                    issues.push(format!(
                        "component `{filename}` connects to `{target}` which does not exist"
                    ));
                }
                if target == filename {
                    issues.push(format!("component `{filename}` connects to itself"));
                }
                if !seen.insert(target) {
                    issues.push(format!(
                        "component `{filename}` has duplicate connection to `{target}`"
                    ));
                }
            }
        }

        for (filename, dec) in &self.decisions {
            let comp = &dec.decision.component;
            if comp != "project" && !self.components.contains_key(comp) {
                issues.push(format!(
                    "decision `{filename}` references component `{comp}` which does not exist"
                ));
            }

            if comp != "project" && !is_valid_kebab_case(comp) {
                issues.push(format!(
                    "decision `{filename}` has invalid component `{comp}` (must be kebab-case or \"project\")"
                ));
            }

            if dec.decision.choice.trim().is_empty() {
                issues.push(format!("decision `{filename}` has empty choice"));
            }

            if dec.decision.reason.trim().is_empty() {
                issues.push(format!("decision `{filename}` has empty reason"));
            }

            if let Some(ref sup) = dec.decision.supersedes {
                if sup == filename {
                    issues.push(format!("decision `{filename}` supersedes itself"));
                } else if !self.decisions.contains_key(sup.as_str()) {
                    issues.push(format!(
                        "decision `{filename}` supersedes `{sup}` which does not exist"
                    ));
                }
            }
        }

        issues
    }
}

// ── Validation helpers ───────────────────────────────────────────────────────

/// Check whether a name is valid kebab-case.
///
/// Rules: non-empty, lowercase ASCII letters + digits + hyphens only,
/// no leading/trailing/consecutive hyphens.
pub fn is_valid_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// List `.toml` file stems in a directory (sorted). Returns empty on `NotFound`.
pub(super) fn list_toml_stems(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut names = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let is_toml = path.extension().is_some_and(|ext| ext == "toml");
        if is_toml {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort_unstable();
    Ok(names)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::*;

    use tempfile::TempDir;

    // ── is_valid_kebab_case ──────────────────────────────────────────────

    #[test]
    fn kebab_valid_names() {
        assert!(is_valid_kebab_case("auth"));
        assert!(is_valid_kebab_case("rate-limiter"));
        assert!(is_valid_kebab_case("mcp-server"));
        assert!(is_valid_kebab_case("a"));
        assert!(is_valid_kebab_case("component1"));
        assert!(is_valid_kebab_case("my-app-v2"));
    }

    #[test]
    fn kebab_rejects_invalid() {
        assert!(!is_valid_kebab_case(""));
        assert!(!is_valid_kebab_case("-leading"));
        assert!(!is_valid_kebab_case("trailing-"));
        assert!(!is_valid_kebab_case("double--hyphen"));
        assert!(!is_valid_kebab_case("UpperCase"));
        assert!(!is_valid_kebab_case("has_underscore"));
        assert!(!is_valid_kebab_case("has space"));
        assert!(!is_valid_kebab_case("has.dot"));
        assert!(!is_valid_kebab_case("special!char"));
        assert!(!is_valid_kebab_case("über"));
    }

    // ── validate ─────────────────────────────────────────────────────────

    #[test]
    fn validate_clean_state() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut auth = sample_component("auth");
        auth.component.connects_to = vec!["database".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();

        let db = sample_component("database");
        store
            .write_atomic(&lock, &store.component_path("database"), &db)
            .unwrap();

        let dec = sample_decision("token-format", "auth");
        store
            .write_atomic(&lock, &store.decision_path("token-format"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        assert!(state.validate().is_empty());
    }

    #[test]
    fn validate_catches_dangling_connection() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut comp = sample_component("auth");
        comp.component.connects_to = vec!["nonexistent".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("nonexistent"));
    }

    #[test]
    fn validate_catches_self_connection() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut comp = sample_component("auth");
        comp.component.connects_to = vec!["auth".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("connects to itself")));
    }

    #[test]
    fn validate_catches_duplicate_connection() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let db = sample_component("database");
        store
            .write_atomic(&lock, &store.component_path("database"), &db)
            .unwrap();

        let mut auth = sample_component("auth");
        auth.component.connects_to = vec!["database".into(), "database".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("duplicate connection")));
    }

    #[test]
    fn validate_catches_dangling_decision_component() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("stale-decision", "deleted-component");
        store
            .write_atomic(&lock, &store.decision_path("stale-decision"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.contains("deleted-component") && i.contains("does not exist"))
        );
    }

    #[test]
    fn validate_allows_project_wide_decisions() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("error-strategy", "project");
        store
            .write_atomic(&lock, &store.decision_path("error-strategy"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        assert!(state.validate().is_empty());
    }

    #[test]
    fn validate_catches_dangling_supersedes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("new-choice", "project");
        dec.decision.supersedes = Some("ghost".into());
        store
            .write_atomic(&lock, &store.decision_path("new-choice"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("ghost")));
    }

    #[test]
    fn validate_catches_filename_mismatch() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("wrong-name");
        store
            .write_atomic(&lock, &store.component_path("actual-file"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.contains("actual-file") && i.contains("wrong-name"))
        );
    }

    // ── new validation checks ────────────────────────────────────────────

    #[test]
    fn validate_catches_non_kebab_component_name() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        // Hand-edit: a component file with an invalid internal name
        let mut comp = sample_component("Bad_Name");
        comp.component.name = "Bad_Name".into();
        store
            .write_atomic(&lock, &store.component_path("Bad_Name"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("kebab-case")));
    }

    #[test]
    fn validate_catches_empty_decision_choice() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("bad-decision", "project");
        dec.decision.choice = String::new();
        store
            .write_atomic(&lock, &store.decision_path("bad-decision"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("empty choice")));
    }

    #[test]
    fn validate_catches_whitespace_only_reason() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("bad-decision", "project");
        dec.decision.reason = "   ".into();
        store
            .write_atomic(&lock, &store.decision_path("bad-decision"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("empty reason")));
    }

    #[test]
    fn validate_catches_non_kebab_decision_component() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("bad-ref", "project");
        dec.decision.component = "Not Kebab".into();
        store
            .write_atomic(&lock, &store.decision_path("bad-ref"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("invalid component")));
    }

    #[test]
    fn validate_catches_self_supersede() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("loopy", "project");
        dec.decision.supersedes = Some("loopy".into());
        store
            .write_atomic(&lock, &store.decision_path("loopy"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("supersedes itself")));
    }
}
