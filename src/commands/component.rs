use std::path::Path;

use crate::store::schema::{Component, ComponentFile, EdgeEntry, EdgeKind, NodeEntry, NodeKind};
use crate::store::{self};
use crate::{Error, Result};

use super::{open_store_mut, validate_mutation};

pub fn add_component(cwd: &Path, name: &str, description: Option<&str>) -> Result<()> {
    if !store::is_valid_kebab_case(name) {
        return Err(Error::InvalidName(name.into()));
    }

    let (store, lock, mut state) = open_store_mut(cwd)?;

    if state.components.contains_key(name) {
        return Err(Error::Validation(format!(
            "component `{name}` already exists"
        )));
    }

    let comp = ComponentFile {
        component: Component {
            name: name.into(),
            description: description.unwrap_or_default().into(),
        },
    };

    let write = store.prepare_write(&store.component_path(name), &comp)?;
    let hash = write.content_hash();

    state.graph_index.nodes.push(NodeEntry {
        name: name.into(),
        kind: NodeKind::Component,
        tags: vec![],
        hash,
    });

    state.components.insert(name.into(), comp);
    validate_mutation(&state)?;

    store.commit_batch(&lock, vec![write], vec![], Some(&state.graph_index))?;
    println!("Added component `{name}`");
    Ok(())
}

pub fn add_connection(cwd: &Path, from: &str, to: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;

    if !state.components.contains_key(from) {
        return Err(Error::Validation(format!(
            "component `{from}` does not exist"
        )));
    }
    if !state.components.contains_key(to) {
        return Err(Error::Validation(format!(
            "component `{to}` does not exist"
        )));
    }
    if from == to {
        return Err(Error::Validation(format!(
            "component `{from}` cannot connect to itself"
        )));
    }

    let duplicate = state
        .graph_index
        .edges
        .iter()
        .any(|e| e.from == from && e.to == to && e.kind == EdgeKind::ConnectsTo);
    if duplicate {
        return Err(Error::Validation(format!(
            "connection `{from}` → `{to}` already exists"
        )));
    }

    state.graph_index.edges.push(EdgeEntry {
        from: from.into(),
        to: to.into(),
        kind: EdgeKind::ConnectsTo,
    });

    validate_mutation(&state)?;

    // Only graph.toml changes — no node files modified.
    store.commit_batch(&lock, vec![], vec![], Some(&state.graph_index))?;
    println!("Connected `{from}` → `{to}`");
    Ok(())
}

pub fn rename_component(cwd: &Path, old: &str, new: &str) -> Result<()> {
    if !store::is_valid_kebab_case(new) {
        return Err(Error::InvalidName(new.into()));
    }

    let (store, lock, mut state) = open_store_mut(cwd)?;

    if !state.components.contains_key(old) {
        return Err(Error::Validation(format!(
            "component `{old}` does not exist"
        )));
    }
    if state.components.contains_key(new) {
        return Err(Error::Validation(format!(
            "component `{new}` already exists"
        )));
    }

    let affected_decisions: Vec<String> = state
        .decisions
        .iter()
        .filter(|(_, dec)| dec.decision.component == old)
        .map(|(dname, _)| dname.clone())
        .collect();

    // Apply mutation in memory.
    let mut renamed = state
        .components
        .remove(old)
        .ok_or_else(|| Error::Validation(format!("component `{old}` does not exist")))?;
    renamed.component.name = new.into();
    state.components.insert(new.into(), renamed);

    for dec in state.decisions.values_mut() {
        if dec.decision.component == old {
            dec.decision.component = new.into();
        }
    }

    // Update graph index: node name.
    for node in &mut state.graph_index.nodes {
        if node.name == old {
            node.name = new.into();
        }
    }

    // Update graph index: edge references.
    for edge in &mut state.graph_index.edges {
        if edge.from == old {
            edge.from = new.into();
        }
        if edge.to == old {
            edge.to = new.into();
        }
    }

    validate_mutation(&state)?;

    // Prepare writes.
    let mut writes = Vec::new();

    let comp_write = store.prepare_write(&store.component_path(new), &state.components[new])?;
    // Update hash for renamed component.
    let new_hash = comp_write.content_hash();
    if let Some(node) = state.graph_index.nodes.iter_mut().find(|n| n.name == new) {
        node.hash = new_hash;
    }
    writes.push(comp_write);

    for dname in &affected_decisions {
        let dec_write = store.prepare_write(
            &store.decision_path(dname),
            &state.decisions[dname.as_str()],
        )?;
        let dec_hash = dec_write.content_hash();
        if let Some(node) = state
            .graph_index
            .nodes
            .iter_mut()
            .find(|n| n.name == *dname)
        {
            node.hash = dec_hash;
        }
        writes.push(dec_write);
    }

    let removes = vec![store.component_path(old)];
    store.commit_batch(&lock, writes, removes, Some(&state.graph_index))?;
    println!("Renamed component `{old}` → `{new}`");
    Ok(())
}

pub fn remove_component(cwd: &Path, name: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;

    if !state.components.contains_key(name) {
        return Err(Error::Validation(format!(
            "component `{name}` does not exist"
        )));
    }

    let referencing: Vec<&str> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == name)
        .map(|(n, _)| n.as_str())
        .collect();

    if !referencing.is_empty() {
        return Err(Error::CascadeBlocked(format!(
            "component `{name}` is referenced by decisions: {}. \
             Remove or reassign them first.",
            referencing.join(", ")
        )));
    }

    state.components.remove(name);

    // Remove node and all edges involving this component from graph index.
    state.graph_index.nodes.retain(|n| n.name != name);
    state
        .graph_index
        .edges
        .retain(|e| e.from != name && e.to != name);

    validate_mutation(&state)?;

    let removes = vec![store.component_path(name)];
    store.commit_batch(&lock, vec![], removes, Some(&state.graph_index))?;
    println!("Removed component `{name}`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{check, decide, init};
    use crate::store::Store;
    use crate::store::schema::EdgeKind;
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
        match err {
            Error::Validation(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Validation, got: {other}"),
        }
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
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn add_connection_rejects_nonexistent_to() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = add_connection(tmp.path(), "auth", "ghost").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn add_connection_rejects_self() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let err = add_connection(tmp.path(), "auth", "auth").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("cannot connect to itself")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn add_connection_rejects_duplicate() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();

        let err = add_connection(tmp.path(), "auth", "database").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Validation, got: {other}"),
        }
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
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn rename_component_rejects_existing_new() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "auth2", None).unwrap();

        let err = rename_component(tmp.path(), "auth", "auth2").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Validation, got: {other}"),
        }
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
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();
        decide(tmp.path(), "auth", "Use Redis", "Fast sessions", None, &[]).unwrap();

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
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn remove_component_refuses_if_decisions_reference_it() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();

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
}
