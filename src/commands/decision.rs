//! Decision operations: quick-record and remove.

use std::path::Path;

use chrono::Utc;

use crate::store::schema::{Decision, DecisionFile};
use crate::store::{self};
use crate::{Error, Result};

use super::{open_store_mut, slugify, unique_decision_stem, validate_mutation};

/// Record a quick decision without the full Socratic flow.
pub fn decide(
    cwd: &Path,
    component: &str,
    choice: &str,
    reason: &str,
    supersedes: Option<&str>,
    alternatives: &[String],
) -> Result<()> {
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    let (store, lock, mut state) = open_store_mut(cwd)?;

    if component != "project" && !state.components.contains_key(component) {
        return Err(Error::Validation(format!(
            "component `{component}` does not exist"
        )));
    }

    if let Some(sup) = supersedes {
        if !state.decisions.contains_key(sup) {
            return Err(Error::Validation(format!(
                "decision `{sup}` does not exist (cannot supersede)"
            )));
        }
    }

    let stem = unique_decision_stem(&state.decisions, &slugify(choice));

    let decision = DecisionFile {
        decision: Decision {
            component: component.into(),
            choice: choice.into(),
            reason: reason.into(),
            alternatives: alternatives.to_vec(),
            created: Utc::now(),
            supersedes: supersedes.map(String::from),
        },
    };

    state.decisions.insert(stem.clone(), decision.clone());
    validate_mutation(&state)?;

    store.write_atomic(&lock, &store.decision_path(&stem), &decision)?;
    println!("Recorded decision `{stem}`");
    Ok(())
}

/// Remove a decision. Warns if other decisions supersede it (broken chain).
pub fn remove_decision(cwd: &Path, name: &str) -> Result<()> {
    let (store, lock, state) = open_store_mut(cwd)?;

    if !state.decisions.contains_key(name) {
        return Err(Error::Validation(format!(
            "decision `{name}` does not exist"
        )));
    }

    let dependents: Vec<&str> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.supersedes.as_deref() == Some(name))
        .map(|(n, _)| n.as_str())
        .collect();

    if !dependents.is_empty() {
        eprintln!(
            "warning: supersede chain broken — these decisions reference `{name}`: {}",
            dependents.join(", ")
        );
    }

    store.remove_file(&lock, &store.decision_path(name))?;
    println!("Removed decision `{name}`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, init};
    use crate::store::Store;
    use tempfile::TempDir;

    #[test]
    fn decide_records_component_decision() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "JWT with DPoP", "Stateless", None, &[]).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-with-dpop").unwrap();
        assert_eq!(dec.decision.component, "auth");
        assert_eq!(dec.decision.choice, "JWT with DPoP");
    }

    #[test]
    fn decide_records_project_wide() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        decide(
            tmp.path(),
            "project",
            "Fail-closed on writes",
            "Never silently succeed",
            None,
            &[],
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let names = store.list_decisions().unwrap();
        assert_eq!(names.len(), 1);
        let dec = store.read_decision(&names[0]).unwrap();
        assert_eq!(dec.decision.component, "project");
    }

    #[test]
    fn decide_rejects_nonexistent_component() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = decide(tmp.path(), "ghost", "x", "y", None, &[]).unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn decide_rejects_nonexistent_supersede_target() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = decide(tmp.path(), "auth", "x", "y", Some("ghost"), &[]).unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn decide_supersedes_existing() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "Session cookies", "Simple", None, &[]).unwrap();
        decide(
            tmp.path(),
            "auth",
            "JWT tokens",
            "Stateless",
            Some("session-cookies"),
            &[],
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-tokens").unwrap();
        assert_eq!(dec.decision.supersedes.as_deref(), Some("session-cookies"));
    }

    #[test]
    fn decide_records_alternatives() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let alts = vec![
            "Session cookies — rejected: requires server-side state".into(),
            "Opaque tokens — rejected: introspection overhead".into(),
        ];
        decide(
            tmp.path(),
            "auth",
            "JWT with DPoP",
            "Stateless",
            None,
            &alts,
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-with-dpop").unwrap();
        assert_eq!(dec.decision.alternatives.len(), 2);
    }

    #[test]
    fn decide_deduplicates_filename() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "Use Redis", "Fast", None, &[]).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Use Redis",
            "Also for sessions",
            None,
            &[],
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let names = store.list_decisions().unwrap();
        assert_eq!(names, vec!["use-redis", "use-redis-2"]);
    }

    #[test]
    fn decide_sets_timestamp() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let before = Utc::now();
        decide(tmp.path(), "auth", "JWT", "Stateless", None, &[]).unwrap();
        let after = Utc::now();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt").unwrap();
        assert!(dec.decision.created >= before);
        assert!(dec.decision.created <= after);
    }

    #[test]
    fn decide_rejects_invalid_component_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = decide(tmp.path(), "../escape", "x", "y", None, &[]).unwrap_err();
        assert!(matches!(err, Error::InvalidName(_)));
    }

    #[test]
    fn decide_allows_project_component() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        decide(tmp.path(), "project", "Test decision", "Testing", None, &[]).unwrap();
    }

    #[test]
    fn decide_supersedes_is_none_when_omitted() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("use-jwt").unwrap();
        assert!(dec.decision.supersedes.is_none());
    }

    // ── remove decision ──────────────────────────────────────────────────

    #[test]
    fn remove_decision_deletes_file() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();

        remove_decision(tmp.path(), "use-jwt").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(store.list_decisions().unwrap().is_empty());
    }

    #[test]
    fn remove_decision_rejects_nonexistent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = remove_decision(tmp.path(), "ghost").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn remove_decision_warns_on_broken_supersede_chain() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Session cookies", "Simple", None, &[]).unwrap();
        decide(
            tmp.path(),
            "auth",
            "JWT tokens",
            "Stateless",
            Some("session-cookies"),
            &[],
        )
        .unwrap();

        remove_decision(tmp.path(), "session-cookies").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-tokens").unwrap();
        assert_eq!(dec.decision.supersedes.as_deref(), Some("session-cookies"));
    }
}
