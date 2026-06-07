use std::path::Path;

use chrono::Utc;

use crate::store::graph::Direction;
use crate::store::schema::{Decision, DecisionFile, EdgeEntry, EdgeKind, NodeEntry, NodeKind};
use crate::store::{self};
use crate::{Error, Result};

use super::{open_store_mut, slugify, unique_decision_stem};

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

    if let Some(sup) = supersedes
        && !state.decisions.contains_key(sup)
    {
        return Err(Error::Validation(format!(
            "decision `{sup}` does not exist (cannot supersede)"
        )));
    }

    let stem = unique_decision_stem(&state.decisions, &slugify(choice))?;

    let decision = DecisionFile {
        decision: Decision {
            component: component.into(),
            choice: choice.into(),
            reason: reason.into(),
            alternatives: alternatives.to_vec(),
            created: Utc::now(),
        },
    };

    let write = store.prepare_write(&store.decision_path(&stem), &decision)?;
    let hash = write.content_hash();

    // Add node to graph index.
    state.graph_index.nodes.push(NodeEntry {
        name: stem.clone(),
        kind: NodeKind::Decision,
        tags: vec![],
        hash,
    });

    // Add BelongsTo edge.
    state.graph_index.edges.push(EdgeEntry {
        from: stem.clone(),
        to: component.into(),
        kind: EdgeKind::BelongsTo,
    });

    // Add Supersedes edge if applicable.
    if let Some(sup) = supersedes {
        state.graph_index.edges.push(EdgeEntry {
            from: stem.clone(),
            to: sup.into(),
            kind: EdgeKind::Supersedes,
        });
    }

    state.decisions.insert(stem.clone(), decision);

    store.commit_with_graph(&lock, vec![write], vec![], &state)?;
    println!("Recorded decision `{stem}`");
    Ok(())
}

pub fn remove_decision(cwd: &Path, name: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;

    if !state.decisions.contains_key(name) {
        return Err(Error::Validation(format!(
            "decision `{name}` does not exist"
        )));
    }

    // Build graph for cascade analysis.
    let graph = state.build_graph();
    let involved = graph.edges_involving(name);

    // Block: other decisions depend on this one via DependsOn.
    let dependents: Vec<String> = involved
        .iter()
        .filter(|(_, e, d)| e.kind == EdgeKind::DependsOn && *d == Direction::Reverse)
        .map(|(other, _, _)| other.to_string())
        .collect();
    if !dependents.is_empty() {
        return Err(Error::CascadeBlocked(format!(
            "decision `{name}` is depended on by: {}. \
             Remove or update them first.",
            dependents.join(", ")
        )));
    }

    // Block: pattern would have <2 members after removal.
    for (other, edge, dir) in &involved {
        if edge.kind == EdgeKind::MemberOf && *dir == Direction::Reverse {
            let member_count = graph
                .edges_involving(other)
                .iter()
                .filter(|(_, e, d)| e.kind == EdgeKind::MemberOf && *d == Direction::Forward)
                .count();
            if member_count <= 2 {
                return Err(Error::CascadeBlocked(format!(
                    "removing `{name}` would leave pattern `{other}` with \
                     fewer than 2 members. Remove or update the pattern first."
                )));
            }
        }
    }

    // Warn: incoming constrains edges (constraint source is being removed).
    let constrainers: Vec<String> = involved
        .iter()
        .filter(|(_, e, d)| e.kind == EdgeKind::Constrains && *d == Direction::Reverse)
        .map(|(other, _, _)| other.to_string())
        .collect();
    if !constrainers.is_empty() {
        eprintln!(
            "warning: removing constraint edges from: {}",
            constrainers.join(", ")
        );
    }

    // Warn: broken supersede chains.
    let supersede_refs: Vec<String> = involved
        .iter()
        .filter(|(_, e, d)| e.kind == EdgeKind::Supersedes && *d == Direction::Reverse)
        .map(|(other, _, _)| other.to_string())
        .collect();
    if !supersede_refs.is_empty() {
        eprintln!(
            "warning: supersede chain broken — these decisions reference `{name}`: {}",
            supersede_refs.join(", ")
        );
    }

    // Apply removal.
    state.decisions.remove(name);
    state.graph_index.nodes.retain(|n| n.name != name);
    state
        .graph_index
        .edges
        .retain(|e| e.from != name && e.to != name);

    let removes = vec![store.decision_path(name)];
    store.commit_with_graph(&lock, vec![], removes, &state)?;
    println!("Removed decision `{name}`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, init};
    use crate::store::Store;
    use crate::store::schema::EdgeKind;
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
    fn decide_supersedes_creates_edge() {
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
        let state = store.load_state().unwrap();
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "jwt-tokens"
                    && e.to == "session-cookies"
                    && e.kind == EdgeKind::Supersedes)
        );
    }

    #[test]
    fn decide_creates_belongs_to_edge() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();

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
    fn remove_decision_cleans_up_edges() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();

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

        // Removing session-cookies should succeed (with warning)
        remove_decision(tmp.path(), "session-cookies").unwrap();

        // jwt-tokens should still exist but its Supersedes edge is cleaned up
        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        assert!(state.decisions.contains_key("jwt-tokens"));
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.to == "session-cookies")
        );
    }

    #[test]
    fn remove_decision_blocks_when_depended_on() {
        use crate::store::schema::EdgeEntry;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();
        decide(tmp.path(), "auth", "Token expiry", "15 min", None, &[]).unwrap();

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
            .commit_batch(&lock, vec![], vec![], Some(&state.graph_index))
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
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();
        decide(tmp.path(), "auth", "Token refresh", "Rotate", None, &[]).unwrap();

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
            .commit_batch(&lock, vec![write], vec![], Some(&state.graph_index))
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
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Short lived tokens",
            "15 min",
            None,
            &[],
        )
        .unwrap();

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
            .commit_batch(&lock, vec![], vec![], Some(&state.graph_index))
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
