use std::path::Path;
use std::sync::Arc;

use crate::store::schema::{Attribution, DecisionFile};
use crate::store::{self, RecordDecisionParams};
use crate::workflow::concerns;
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

/// Remove every agent-recorded decision in a component at once.
///
/// Collects each decision whose `attribution` is [`Attribution::Agent`] within
/// `component`, then runs a cascade pre-flight over the whole set. The batch is
/// atomic: if removing any candidate would break a dependent or shrink a
/// pattern below its minimum, none are removed. Human-recorded decisions are
/// never touched. On success, the concern coverage the batch erased is reported
/// against what remains in the component.
pub fn remove_agent_decisions(cwd: &Path, component: &str) -> Result<()> {
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    let (store, lock, mut state) = open_store_mut(cwd)?;

    if component != "project" && !state.components.contains_key(component) {
        return Err(Error::ComponentNotFound(component.into()));
    }

    let candidates: Vec<String> = state
        .decisions
        .iter()
        .filter(|(_, dec)| {
            dec.decision.component == component && dec.decision.attribution == Attribution::Agent
        })
        .map(|(name, _)| name.clone())
        .collect();

    if candidates.is_empty() {
        println!("No agent decisions in [{component}] to remove.");
        return Ok(());
    }

    // Pre-flight the whole set before touching disk: a single blocked candidate
    // aborts the entire batch, so a partial removal can never leave the graph
    // in a half-collapsed state.
    for name in &candidates {
        let cascade = state.graph().check_decision_cascade(name);
        if cascade.is_blocked() {
            return Err(Error::CascadeBlocked(format!(
                "`{name}` — {}; no decisions removed",
                cascade.blocker_summary()
            )));
        }
    }

    // Snapshot before removal so the coverage the batch carried can be reported
    // once the decisions are gone from the graph.
    let snapshots: Vec<Arc<DecisionFile>> = candidates
        .iter()
        .filter_map(|name| state.decisions.get(name).map(Arc::clone))
        .collect();

    let names: Vec<&str> = candidates.iter().map(String::as_str).collect();
    store.remove_decisions(&lock, &mut state, &names)?;
    drop(lock);

    let count = names.len();
    let plural = if count == 1 { "" } else { "s" };
    println!("Removed {count} agent decision{plural} from [{component}]");

    let remaining: Vec<&DecisionFile> = state
        .graph()
        .decisions_for(component)
        .into_iter()
        .map(|(_, dec)| dec)
        .collect();
    let mut lost: Vec<&'static str> = Vec::new();
    for dec in &snapshots {
        for area in concerns::coverage_lost(dec, &remaining) {
            if !lost.contains(&area) {
                lost.push(area);
            }
        }
    }
    if !lost.is_empty() {
        println!("⚠ [{component}] lost coverage: {}", lost.join(", "));
    }

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

    // Capture the decision before removal so its lost concern coverage can be
    // reported once it is gone from the graph.
    let removed = state.decisions.get(name).map(Arc::clone);

    store.remove_decision(&lock, &mut state, name)?;
    println!("Removed decision `{name}`");

    if let Some(removed) = removed {
        let component = &removed.decision.component;
        let remaining: Vec<&DecisionFile> = state
            .graph()
            .decisions_for(component)
            .into_iter()
            .map(|(_, dec)| dec)
            .collect();
        let lost = concerns::coverage_lost(&removed, &remaining);
        if !lost.is_empty() {
            println!("⚠ [{component}] lost coverage: {}", lost.join(", "));
        }
    }

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
    fn remove_decision_reports_lost_coverage() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Encrypt credentials at rest",
            "Protect secrets from disk exposure",
            &[],
        )
        .unwrap();

        // The removed decision uniquely covered Security boundaries — the
        // report branch must run and the removal must complete cleanly.
        remove_decision(tmp.path(), "encrypt-credentials-at-rest").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(store.list_decisions().unwrap().is_empty());
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

    // ── bulk remove agent decisions ──────────────────────────────────────

    /// Record a decision through the store write path so it carries a real
    /// `BelongsTo` edge and an explicit attribution.
    fn record(
        store: &Store,
        state: &mut store::ProjectState,
        lock: &store::StoreLock,
        component: &str,
        choice: &str,
        attribution: Attribution,
    ) -> String {
        store
            .record_decision(
                lock,
                state,
                RecordDecisionParams {
                    component,
                    choice,
                    reason: "Recorded for a bulk-removal test",
                    alternatives: &[],
                    depends_on: &[],
                    constrains: &[],
                    tags: &[],
                    attribution,
                    code_refs: &[],
                },
            )
            .unwrap()
    }

    #[test]
    fn remove_agent_decisions_removes_only_agent_decisions() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        record(
            &store,
            &mut state,
            &lock,
            "auth",
            "Human choice",
            Attribution::User,
        );
        record(
            &store,
            &mut state,
            &lock,
            "auth",
            "Agent one",
            Attribution::Agent,
        );
        record(
            &store,
            &mut state,
            &lock,
            "auth",
            "Agent two",
            Attribution::Agent,
        );
        drop(lock);

        remove_agent_decisions(tmp.path(), "auth").unwrap();

        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(state.decisions.contains_key("human-choice"));
        assert!(!state.decisions.contains_key("agent-one"));
        assert!(!state.decisions.contains_key("agent-two"));
    }

    #[test]
    fn remove_agent_decisions_aborts_when_any_blocked() {
        use crate::store::schema::EdgeEntry;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        let base = record(
            &store,
            &mut state,
            &lock,
            "auth",
            "Agent base",
            Attribution::Agent,
        );
        record(
            &store,
            &mut state,
            &lock,
            "auth",
            "Agent spare",
            Attribution::Agent,
        );
        let dependent = record(
            &store,
            &mut state,
            &lock,
            "auth",
            "Human gate",
            Attribution::User,
        );

        // The human decision depends on one agent decision, so removing that
        // agent decision is cascade-blocked — which must abort the batch.
        state.graph_index.edges.push(EdgeEntry {
            from: dependent,
            to: base,
            kind: EdgeKind::DependsOn,
        });
        store
            .commit_batch(&lock, vec![], vec![], Some(state.graph_index.clone()))
            .unwrap();
        drop(lock);

        let err = remove_agent_decisions(tmp.path(), "auth").unwrap_err();
        assert!(matches!(err, Error::CascadeBlocked(_)));

        // Nothing was removed — not even the unblocked agent decision.
        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(state.decisions.contains_key("agent-base"));
        assert!(state.decisions.contains_key("agent-spare"));
    }

    #[test]
    fn remove_agent_decisions_noop_when_none_present() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Human only", "Reason enough here", &[]).unwrap();

        remove_agent_decisions(tmp.path(), "auth").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert_eq!(store.list_decisions().unwrap().len(), 1);
    }

    #[test]
    fn remove_agent_decisions_rejects_nonexistent_component() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = remove_agent_decisions(tmp.path(), "ghost").unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
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
