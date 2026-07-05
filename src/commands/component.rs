use std::path::Path;

use crate::{Error, Result};

use super::open_store_mut;

/// `trurlic add component` — register a new component node.
pub fn add_component(cwd: &Path, name: &str, description: Option<&str>) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;
    store.add_component(&lock, &mut state, name, description.unwrap_or_default())?;
    println!("Added component `{name}`");
    Ok(())
}

/// `trurlic connect` — add a directed `ConnectsTo` edge between components.
pub fn add_connection(cwd: &Path, from: &str, to: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;
    store.add_connection(&lock, &mut state, from, to)?;
    println!("Connected `{from}` → `{to}`");
    Ok(())
}

/// `trurlic rename component` — rename a component, rewriting every incident edge.
pub fn rename_component(cwd: &Path, old: &str, new: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;
    store.rename_component(&lock, &mut state, old, new)?;
    println!("Renamed component `{old}` → `{new}`");
    Ok(())
}

/// `trurlic remove component` — delete a component after a cascade pre-flight.
/// Refused with `CascadeBlocked` if decisions still reference it; non-blocking
/// warnings are printed but do not stop the removal.
pub fn remove_component(cwd: &Path, name: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;

    let cascade = state.graph().check_component_cascade(name);
    if cascade.is_blocked() {
        return Err(Error::CascadeBlocked(cascade.blocker_summary()));
    }
    for w in &cascade.warnings {
        eprintln!("warning: {}", w.message);
    }

    store.remove_component(&lock, &mut state, name)?;
    println!("Removed component `{name}`");
    Ok(())
}

/// `trurlic disconnect` — remove a `ConnectsTo` edge between components.
pub fn remove_connection(cwd: &Path, from: &str, to: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;
    store.remove_connection(&lock, &mut state, from, to)?;
    println!("Disconnected `{from}` → `{to}`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{check, decide, init};
    use crate::store::Store;
    use crate::store::schema::{EdgeKind, NodeKind};
    use tempfile::TempDir;

    // ── add component ────────────────────────────────────────────────────

    #[test]
    fn add_component_creates_file() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let comp = store.read_component("auth").unwrap();
        assert_eq!(comp.component.name, "auth");
    }

    #[test]
    fn add_component_rejects_invalid_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        assert!(matches!(
            add_component(tmp.path(), "NotKebab", None).unwrap_err(),
            Error::InvalidName(_)
        ));
        assert!(matches!(
            add_component(tmp.path(), "", None).unwrap_err(),
            Error::InvalidName(_)
        ));
        assert!(matches!(
            add_component(tmp.path(), "-leading", None).unwrap_err(),
            Error::InvalidName(_)
        ));
    }

    #[test]
    fn add_component_rejects_duplicate() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = add_component(tmp.path(), "auth", None).unwrap_err();
        assert!(matches!(err, Error::ComponentExists(ref n) if n == "auth"));
    }

    #[test]
    fn add_component_rejects_reserved_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = add_component(tmp.path(), "project", None).unwrap_err();
        assert!(matches!(err, Error::ReservedName(ref n) if n == "project"));
    }

    #[test]
    fn rename_component_rejects_reserved_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = rename_component(tmp.path(), "auth", "project").unwrap_err();
        assert!(matches!(err, Error::ReservedName(ref n) if n == "project"));
    }

    #[test]
    fn add_component_stores_description() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(
            tmp.path(),
            "auth",
            Some("Authentication and token management"),
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let comp = store.read_component("auth").unwrap();
        assert_eq!(
            comp.component.description,
            "Authentication and token management"
        );
    }

    #[test]
    fn add_component_empty_description_when_omitted() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let comp = store.read_component("auth").unwrap();
        assert!(comp.component.description.is_empty());
    }

    #[test]
    fn add_component_creates_graph_node() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        assert!(
            state
                .graph_index
                .nodes
                .iter()
                .any(|n| n.name == "auth" && n.kind == NodeKind::Component)
        );
    }

    // ── add connection ───────────────────────────────────────────────────

    #[test]
    fn add_connection_creates_edge() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "auth" && e.to == "database" && e.kind == EdgeKind::ConnectsTo)
        );
    }

    #[test]
    fn add_connection_rejects_nonexistent_from() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = add_connection(tmp.path(), "ghost", "auth").unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn add_connection_rejects_nonexistent_to() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = add_connection(tmp.path(), "auth", "ghost").unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn add_connection_rejects_self() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = add_connection(tmp.path(), "auth", "auth").unwrap_err();
        assert!(matches!(err, Error::SelfConnection(ref n) if n == "auth"));
    }

    #[test]
    fn add_connection_rejects_duplicate() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();

        let err = add_connection(tmp.path(), "auth", "database").unwrap_err();
        assert!(matches!(err, Error::DuplicateConnection { .. }));
    }

    // ── rename component ─────────────────────────────────────────────────

    #[test]
    fn rename_component_updates_file_and_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        rename_component(tmp.path(), "auth", "authentication").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(!store.component_path("auth").exists());
        let comp = store.read_component("authentication").unwrap();
        assert_eq!(comp.component.name, "authentication");
    }

    #[test]
    fn rename_component_rejects_nonexistent_old() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = rename_component(tmp.path(), "ghost", "new").unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn rename_component_rejects_existing_new() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "auth2", None).unwrap();

        let err = rename_component(tmp.path(), "auth", "auth2").unwrap_err();
        assert!(matches!(err, Error::ComponentExists(ref n) if n == "auth2"));
    }

    #[test]
    fn rename_component_rejects_invalid_new_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        assert!(matches!(
            rename_component(tmp.path(), "auth", "NotKebab").unwrap_err(),
            Error::InvalidName(_)
        ));
    }

    #[test]
    fn rename_component_updates_edges() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        add_connection(tmp.path(), "database", "auth").unwrap();

        rename_component(tmp.path(), "auth", "authentication").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        // Forward edge should be updated
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "authentication"
                    && e.to == "database"
                    && e.kind == EdgeKind::ConnectsTo)
        );
        // Reverse edge should be updated
        assert!(state.graph_index.edges.iter().any(|e| e.from == "database"
            && e.to == "authentication"
            && e.kind == EdgeKind::ConnectsTo));
        // Old name should be gone from edges
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "auth" || e.to == "auth")
        );

        check(tmp.path(), false).unwrap();
    }

    #[test]
    fn rename_component_updates_decision_references() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[], &[]).unwrap();
        decide(tmp.path(), "auth", "Use Redis", "Fast sessions", &[], &[]).unwrap();

        rename_component(tmp.path(), "auth", "authentication").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        for name in store.list_decisions().unwrap() {
            let dec = store.read_decision(&name).unwrap();
            assert_eq!(dec.decision.component, "authentication");
        }
        check(tmp.path(), false).unwrap();
    }

    // ── remove component ─────────────────────────────────────────────────

    #[test]
    fn remove_component_deletes_file() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        remove_component(tmp.path(), "auth").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(store.list_components().unwrap().is_empty());
    }

    #[test]
    fn remove_component_rejects_nonexistent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = remove_component(tmp.path(), "ghost").unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
    }

    #[test]
    fn remove_component_refuses_if_decisions_reference_it() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[], &[]).unwrap();

        let err = remove_component(tmp.path(), "auth").unwrap_err();
        match err {
            Error::CascadeBlocked(msg) => {
                assert!(msg.contains("auth"), "should mention the component: {msg}");
                assert!(
                    msg.contains("use-jwt"),
                    "should list the blocking decision: {msg}"
                );
            }
            other => panic!("expected CascadeBlocked, got: {other}"),
        }
    }

    #[test]
    fn remove_component_cleans_up_edges() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_component(tmp.path(), "cache", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        add_connection(tmp.path(), "cache", "database").unwrap();

        remove_component(tmp.path(), "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        // No edges should reference "database"
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "database" || e.to == "database")
        );

        check(tmp.path(), false).unwrap();
    }

    // ── remove connection ───────────────────────────────────────────────

    #[test]
    fn remove_connection_deletes_edge() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();

        remove_connection(tmp.path(), "auth", "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "auth" && e.to == "database" && e.kind == EdgeKind::ConnectsTo)
        );
        check(tmp.path(), false).unwrap();
    }

    #[test]
    fn remove_connection_rejects_nonexistent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();

        let err = remove_connection(tmp.path(), "auth", "database").unwrap_err();
        assert!(matches!(err, Error::ConnectionNotFound { .. }));
    }

    #[test]
    fn remove_connection_preserves_reverse() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        add_connection(tmp.path(), "database", "auth").unwrap();

        remove_connection(tmp.path(), "auth", "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        // Forward edge removed.
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "auth" && e.to == "database")
        );
        // Reverse edge preserved.
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "database" && e.to == "auth" && e.kind == EdgeKind::ConnectsTo)
        );
    }
}
