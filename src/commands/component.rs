//! Component operations: add, connect, rename, remove.

use std::path::Path;

use crate::store::schema::{Component, ComponentFile};
use crate::store::{self};
use crate::{Error, Result};

use super::{open_store_mut, validate_mutation};

/// Add a new component to `.trurl/`.
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
            connects_to: vec![],
        },
    };

    state.components.insert(name.into(), comp.clone());
    validate_mutation(&state)?;

    store.write_atomic(&lock, &store.component_path(name), &comp)?;
    println!("Added component `{name}`");
    Ok(())
}

/// Connect two existing components (directional: from → to).
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

    if state.components[from]
        .component
        .connects_to
        .iter()
        .any(|t| t == to)
    {
        return Err(Error::Validation(format!(
            "connection `{from}` → `{to}` already exists"
        )));
    }

    let mut updated = state.components[from].clone();
    updated.component.connects_to.push(to.into());

    state.components.insert(from.into(), updated.clone());
    validate_mutation(&state)?;

    store.write_atomic(&lock, &store.component_path(from), &updated)?;
    println!("Connected `{from}` → `{to}`");
    Ok(())
}

/// Rename a component, updating all references via batch commit.
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

    let affected_components: Vec<String> = state
        .components
        .iter()
        .filter(|(cname, comp)| {
            *cname != old && comp.component.connects_to.iter().any(|t| t == old)
        })
        .map(|(cname, _)| cname.clone())
        .collect();

    let affected_decisions: Vec<String> = state
        .decisions
        .iter()
        .filter(|(_, dec)| dec.decision.component == old)
        .map(|(dname, _)| dname.clone())
        .collect();

    // Apply mutation in memory
    let mut renamed = state
        .components
        .remove(old)
        .ok_or_else(|| Error::Validation(format!("component `{old}` does not exist")))?;
    renamed.component.name = new.into();
    state.components.insert(new.into(), renamed);

    for comp in state.components.values_mut() {
        for target in &mut comp.component.connects_to {
            if target == old {
                *target = new.into();
            }
        }
    }

    for dec in state.decisions.values_mut() {
        if dec.decision.component == old {
            dec.decision.component = new.into();
        }
    }

    validate_mutation(&state)?;

    // Batch commit
    let mut writes = Vec::new();
    writes.push(store.prepare_write(&store.component_path(new), &state.components[new])?);
    for cname in &affected_components {
        writes.push(store.prepare_write(
            &store.component_path(cname),
            &state.components[cname.as_str()],
        )?);
    }
    for dname in &affected_decisions {
        writes.push(store.prepare_write(
            &store.decision_path(dname),
            &state.decisions[dname.as_str()],
        )?);
    }

    let removes = vec![store.component_path(old)];
    store.commit_batch(&lock, writes, removes)?;
    println!("Renamed component `{old}` → `{new}`");
    Ok(())
}

/// Remove a component. Refuses if any decisions reference it.
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
        return Err(Error::Validation(format!(
            "cannot remove component `{name}`: referenced by decisions: {}",
            referencing.join(", ")
        )));
    }

    let affected: Vec<String> = state
        .components
        .iter()
        .filter(|(comp_name, comp)| {
            *comp_name != name && comp.component.connects_to.iter().any(|t| t == name)
        })
        .map(|(comp_name, _)| comp_name.clone())
        .collect();

    state.components.remove(name);
    for comp in state.components.values_mut() {
        comp.component.connects_to.retain(|t| t != name);
    }

    validate_mutation(&state)?;

    let mut writes = Vec::new();
    for comp_name in &affected {
        writes.push(store.prepare_write(
            &store.component_path(comp_name),
            &state.components[comp_name.as_str()],
        )?);
    }
    let removes = vec![store.component_path(name)];

    store.commit_batch(&lock, writes, removes)?;
    println!("Removed component `{name}`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{check, decide, init};
    use crate::store::Store;
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

    // ── add connection ───────────────────────────────────────────────────

    #[test]
    fn add_connection_links_components() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let comp = store.read_component("auth").unwrap();
        assert_eq!(comp.component.connects_to, vec!["database"]);
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
    fn rename_component_updates_connections() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        add_connection(tmp.path(), "database", "auth").unwrap();

        rename_component(tmp.path(), "auth", "authentication").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let authn = store.read_component("authentication").unwrap();
        assert_eq!(authn.component.connects_to, vec!["database"]);
        let db = store.read_component("database").unwrap();
        assert_eq!(db.component.connects_to, vec!["authentication"]);
        check(tmp.path()).unwrap();
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
        check(tmp.path()).unwrap();
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
            Error::Validation(msg) => {
                assert!(msg.contains("cannot remove"));
                assert!(msg.contains("use-jwt"));
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn remove_component_cleans_up_incoming_connections() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_component(tmp.path(), "cache", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        add_connection(tmp.path(), "cache", "database").unwrap();

        remove_component(tmp.path(), "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let auth = store.read_component("auth").unwrap();
        assert!(auth.component.connects_to.is_empty());
        let cache = store.read_component("cache").unwrap();
        assert!(cache.component.connects_to.is_empty());
        check(tmp.path()).unwrap();
    }
}
