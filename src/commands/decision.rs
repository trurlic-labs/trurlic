use std::path::Path;

use crate::store::schema::Attribution;
use crate::store::{self, RecordDecisionParams};
use crate::{Error, Result};

use super::open_store_mut;

pub fn decide(
    cwd: &Path,
    component: &str,
    choice: &str,
    reason: &str,
    alternatives: &[String],
) -> Result<()> {
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    let (store, lock, mut state) = open_store_mut(cwd)?;

    if component != "project" && !state.components.contains_key(component) {
        return Err(Error::ComponentNotFound(component.into()));
    }

    let stem = store.record_decision(
        &lock,
        &mut state,
        RecordDecisionParams {
            component,
            choice,
            reason,
            alternatives,
            depends_on: &[],
            constrains: &[],
            tags: &[],
            attribution: Attribution::User,
            code_refs: &[],
        },
    )?;

    println!("Recorded decision `{stem}`");
    Ok(())
}

pub fn remove_decision(cwd: &Path, name: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;

    let cascade = state.graph().check_decision_cascade(name);
    if cascade.is_blocked() {
        return Err(Error::CascadeBlocked(cascade.blocker_summary()));
    }
    for w in &cascade.warnings {
        eprintln!("warning: {}", w.message);
    }

    store.remove_decision(&lock, &mut state, name)?;
    println!("Removed decision `{name}`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, init};
    use crate::store::Store;
    use crate::store::schema::EdgeKind;
    use chrono::Utc;
    use tempfile::TempDir;

    #[test]
    fn decide_records_component_decision() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "JWT with DPoP", "Stateless", &[]).unwrap();

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

        let err = decide(tmp.path(), "ghost", "x", "y", &[]).unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn decide_creates_belongs_to_edge() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[]).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "use-jwt" && e.to == "auth" && e.kind == EdgeKind::BelongsTo)
        );
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
        decide(tmp.path(), "auth", "JWT with DPoP", "Stateless", &alts).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-with-dpop").unwrap();
        assert_eq!(dec.decision.alternatives.len(), 2);
    }

    #[test]
    fn decide_deduplicates_filename() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "Use Redis", "Fast", &[]).unwrap();
        decide(tmp.path(), "auth", "Use Redis", "Also for sessions", &[]).unwrap();

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
        decide(tmp.path(), "auth", "JWT", "Stateless", &[]).unwrap();
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

        let err = decide(tmp.path(), "../escape", "x", "y", &[]).unwrap_err();
        assert!(matches!(err, Error::InvalidName(_)));
    }

    #[test]
    fn decide_allows_project_component() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        decide(tmp.path(), "project", "Test decision", "Testing", &[]).unwrap();
    }

    // ── remove decision ──────────────────────────────────────────────────

    #[test]
    fn remove_decision_deletes_file() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[]).unwrap();

        remove_decision(tmp.path(), "use-jwt").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(store.list_decisions().unwrap().is_empty());
    }

    #[test]
    fn remove_decision_cleans_up_edges() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[]).unwrap();

        remove_decision(tmp.path(), "use-jwt").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "use-jwt" || e.to == "use-jwt")
        );
    }

    #[test]
    fn remove_decision_rejects_nonexistent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = remove_decision(tmp.path(), "ghost").unwrap_err();
        assert!(matches!(err, Error::DecisionNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn remove_decision_blocks_when_depended_on() {
        use crate::store::schema::EdgeEntry;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[]).unwrap();
        decide(tmp.path(), "auth", "Token expiry", "15 min", &[]).unwrap();

        // Manually add DependsOn edge: token-expiry depends on use-jwt.
        let store = Store::discover(tmp.path()).unwrap();
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        state.graph_index.edges.push(EdgeEntry {
            from: "token-expiry".into(),
            to: "use-jwt".into(),
            kind: EdgeKind::DependsOn,
        });
        store
            .commit_batch(&lock, vec![], vec![], Some(state.graph_index))
            .unwrap();
        drop(lock);

        let err = remove_decision(tmp.path(), "use-jwt").unwrap_err();
        match err {
            Error::CascadeBlocked(msg) => {
                assert!(
                    msg.contains("token-expiry"),
                    "should name the dependent: {msg}"
                );
            }
            other => panic!("expected CascadeBlocked, got: {other}"),
        }
    }

    #[test]
    fn remove_decision_blocks_when_pattern_would_shrink() {
        use crate::store::schema::{EdgeEntry, NodeEntry, NodeKind, Pattern, PatternFile};

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[]).unwrap();
        decide(tmp.path(), "auth", "Token refresh", "Rotate", &[]).unwrap();

        // Create a pattern with exactly 2 member decisions.
        let store = Store::discover(tmp.path()).unwrap();
        let lock = store.lock().unwrap();

        let pat = PatternFile {
            pattern: Pattern {
                name: "auth-tokens".into(),
                description: "Token handling pattern".into(),
            },
        };
        let write = store
            .prepare_write(&store.pattern_path("auth-tokens"), &pat)
            .unwrap();
        let hash = write.content_hash();

        let mut state = store.load_state().unwrap();
        state.graph_index.nodes.push(NodeEntry {
            name: "auth-tokens".into(),
            kind: NodeKind::Pattern,
            tags: vec![],
            hash,
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "auth-tokens".into(),
            to: "use-jwt".into(),
            kind: EdgeKind::MemberOf,
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "auth-tokens".into(),
            to: "token-refresh".into(),
            kind: EdgeKind::MemberOf,
        });
        store
            .commit_batch(&lock, vec![write], vec![], Some(state.graph_index))
            .unwrap();
        drop(lock);

        let err = remove_decision(tmp.path(), "use-jwt").unwrap_err();
        match err {
            Error::CascadeBlocked(msg) => {
                assert!(
                    msg.contains("auth-tokens"),
                    "should name the pattern: {msg}"
                );
                assert!(
                    msg.contains("fewer than 2"),
                    "should explain the constraint: {msg}"
                );
            }
            other => panic!("expected CascadeBlocked, got: {other}"),
        }
    }

    #[test]
    fn remove_decision_allows_with_constrains_edge() {
        use crate::store::schema::EdgeEntry;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[]).unwrap();
        decide(tmp.path(), "auth", "Short lived tokens", "15 min", &[]).unwrap();

        // Manually add Constrains edge: short-lived-tokens constrains use-jwt.
        let store = Store::discover(tmp.path()).unwrap();
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        state.graph_index.edges.push(EdgeEntry {
            from: "short-lived-tokens".into(),
            to: "use-jwt".into(),
            kind: EdgeKind::Constrains,
        });
        store
            .commit_batch(&lock, vec![], vec![], Some(state.graph_index))
            .unwrap();
        drop(lock);

        // Removing the constrained decision should succeed (warn, allow).
        remove_decision(tmp.path(), "use-jwt").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        // Constrains edge must be cleaned up.
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Constrains),
            "Constrains edge should be removed"
        );
        // The constraining decision itself is unaffected.
        assert!(state.decisions.contains_key("short-lived-tokens"));
    }
}
