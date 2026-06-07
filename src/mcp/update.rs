use serde_json::Value;

use crate::store::graph::Direction;
use crate::store::schema::EdgeKind;
use crate::store::{self, Store};

use super::write::{opt_str, record_decision, require_str};

// ── remove_decision ─────────────────────────────────────────────────────────

pub(crate) fn remove_decision(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;

    if !state.decisions.contains_key(name) {
        return Err(format!("decision `{name}` does not exist"));
    }

    // Cascade analysis: scope the graph borrow so we can mutate state afterward.
    let (blocked_by, would_warn) = {
        let graph = &state.graph;
        let involved = graph.edges_involving(name);

        let mut blocked_by: Vec<Value> = Vec::new();
        let mut would_warn: Vec<Value> = Vec::new();

        // Block: other decisions depend on this one.
        for (other, edge, dir) in &involved {
            if edge.kind == EdgeKind::DependsOn && *dir == Direction::Reverse {
                blocked_by.push(serde_json::json!({
                    "node": other.to_string(),
                    "edge": "depends_on",
                    "message": format!("decision `{other}` depends on `{name}`"),
                }));
            }
        }

        // Block: pattern would shrink below 2 members.
        for (other, edge, dir) in &involved {
            if edge.kind == EdgeKind::MemberOf && *dir == Direction::Reverse {
                let member_count = graph.forward_edge_count(other, EdgeKind::MemberOf);
                if member_count <= 2 {
                    blocked_by.push(serde_json::json!({
                        "node": other.to_string(),
                        "edge": "member_of",
                        "message": format!(
                            "pattern `{other}` would have fewer than 2 members"
                        ),
                    }));
                } else {
                    would_warn.push(serde_json::json!({
                        "node": other.to_string(),
                        "edge": "member_of",
                        "message": format!("pattern `{other}` will be updated"),
                    }));
                }
            }
        }

        // Warn: broken supersede chains.
        for (other, edge, dir) in &involved {
            if edge.kind == EdgeKind::Supersedes && *dir == Direction::Reverse {
                would_warn.push(serde_json::json!({
                    "node": other.to_string(),
                    "edge": "supersedes",
                    "message": format!("supersede chain broken for `{other}`"),
                }));
            }
        }

        // Warn: constrains edges pointing to this decision.
        for (other, edge, dir) in &involved {
            if edge.kind == EdgeKind::Constrains && *dir == Direction::Reverse {
                would_warn.push(serde_json::json!({
                    "node": other.to_string(),
                    "edge": "constrains",
                    "message": format!("constraint from `{other}` removed"),
                }));
            }
        }

        (blocked_by, would_warn)
    }; // graph borrow released

    if !blocked_by.is_empty() {
        return Ok(serde_json::json!({
            "removed": false,
            "blocked_by": blocked_by,
            "would_warn": would_warn,
        }));
    }

    // Execute removal.
    let lock = store.lock().map_err(|e| e.to_string())?;
    let graph_snapshot = state.graph_index.clone();
    let decision_snapshot = state.decisions.remove(name);

    state.graph_index.nodes.retain(|n| n.name != name);
    state
        .graph_index
        .edges
        .retain(|e| e.from != name && e.to != name);

    let removes = vec![store.decision_path(name)];

    if let Err(e) = store.commit_with_graph(&lock, vec![], removes, state) {
        // Rollback.
        if let Some(dec) = decision_snapshot {
            state.decisions.insert(name.into(), dec);
        }
        state.graph_index = graph_snapshot;
        return Err(e.to_string());
    }

    state.rebuild_graph();

    Ok(serde_json::json!({
        "removed": true,
        "blocked_by": [],
        "would_warn": would_warn,
    }))
}

// ── update_decision ─────────────────────────────────────────────────────────

pub(crate) fn update_decision(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    let mode = require_str(args, "mode")?;

    if !state.decisions.contains_key(name) {
        return Err(format!("decision `{name}` does not exist"));
    }

    match mode {
        "amend" => amend_decision(store, state, name, args),
        "supersede" => supersede_decision(store, state, name, args),
        _ => Err(format!(
            "invalid mode `{mode}` — expected \"amend\" or \"supersede\""
        )),
    }
}

/// Small correction: edit the file in place. `created` timestamp unchanged.
fn amend_decision(
    store: &Store,
    state: &mut store::ProjectState,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let new_choice = opt_str(args, "choice")?;
    let new_reason = opt_str(args, "reason")?;

    if new_choice.is_none() && new_reason.is_none() {
        return Err("amend requires at least one of `choice` or `reason`".into());
    }

    let lock = store.lock().map_err(|e| e.to_string())?;

    let dec = state
        .decisions
        .get_mut(name)
        .ok_or_else(|| format!("decision `{name}` disappeared during amend"))?;

    let old_choice = dec.decision.choice.clone();
    let old_reason = dec.decision.reason.clone();

    if let Some(c) = new_choice {
        dec.decision.choice = c.into();
    }
    if let Some(r) = new_reason {
        dec.decision.reason = r.into();
    }

    let write = store
        .prepare_write(&store.decision_path(name), dec)
        .map_err(|e| e.to_string())?;
    let hash = write.content_hash();

    // Update hash in graph index.
    if let Some(node) = state.graph_index.nodes.iter_mut().find(|n| n.name == name) {
        node.hash = hash;
    }

    if let Err(e) = store.commit_with_graph(&lock, vec![write], vec![], state) {
        // Rollback in-memory changes.
        if let Some(dec) = state.decisions.get_mut(name) {
            dec.decision.choice = old_choice;
            dec.decision.reason = old_reason;
        }
        return Err(e.to_string());
    }

    state.rebuild_graph();

    // Collect affected patterns and decisions.
    let (affected_patterns, affected_decisions) = collect_affected(&state.graph, name);

    Ok(serde_json::json!({
        "name": name,
        "path": store.decision_path(name).display().to_string(),
        "affected_patterns": affected_patterns,
        "affected_decisions": affected_decisions,
    }))
}

/// Substantive change: create a new decision that supersedes the old one.
fn supersede_decision(
    store: &Store,
    state: &mut store::ProjectState,
    old_name: &str,
    args: &Value,
) -> Result<Value, String> {
    let old_dec = state
        .decisions
        .get(old_name)
        .ok_or_else(|| format!("decision `{old_name}` does not exist"))?;

    let new_choice = opt_str(args, "choice")?;
    let new_reason = opt_str(args, "reason")?;

    if new_choice.is_none() && new_reason.is_none() {
        return Err("supersede requires at least one of `choice` or `reason`".into());
    }

    let choice = new_choice.unwrap_or(&old_dec.decision.choice);
    let reason = new_reason.unwrap_or(&old_dec.decision.reason);

    let component = old_dec.decision.component.clone();

    // Delegate to record_decision with supersedes set.
    let record_args = serde_json::json!({
        "component": component,
        "choice": choice,
        "reason": reason,
        "supersedes": old_name,
    });

    let result = record_decision(store, state, &record_args)?;
    let new_name = result["name"].as_str().unwrap_or_default();

    // Collect affected info from the OLD decision.
    let (affected_patterns, affected_decisions) = collect_affected(&state.graph, old_name);

    Ok(serde_json::json!({
        "name": new_name,
        "path": result["path"],
        "affected_patterns": affected_patterns,
        "affected_decisions": affected_decisions,
    }))
}

/// Collect pattern and decision names affected by edges involving a decision.
fn collect_affected(
    graph: &crate::store::graph::InMemoryGraph,
    decision: &str,
) -> (Vec<String>, Vec<String>) {
    let involved = graph.edges_involving(decision);

    let patterns: Vec<String> = involved
        .iter()
        .filter(|(_, e, d)| e.kind == EdgeKind::MemberOf && *d == Direction::Reverse)
        .map(|(other, _, _)| other.to_string())
        .collect();

    let decisions: Vec<String> = involved
        .iter()
        .filter(|(_, e, d)| {
            matches!(
                e.kind,
                EdgeKind::DependsOn | EdgeKind::Constrains | EdgeKind::Supersedes
            ) && *d == Direction::Reverse
        })
        .map(|(other, _, _)| other.to_string())
        .collect();

    (patterns, decisions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;
    use crate::mcp::write::{record_decision, record_pattern};
    use crate::store::Store;
    use serde_json::json;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Store, store::ProjectState) {
        let tmp = TempDir::new().unwrap();
        commands::init(tmp.path()).unwrap();
        commands::add_component(tmp.path(), "auth", Some("Authentication")).unwrap();
        commands::add_component(tmp.path(), "database", Some("Database layer")).unwrap();
        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        (tmp, store, state)
    }

    // ── remove_decision ─────────────────────────────────────────────────

    #[test]
    fn remove_decision_basic() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["removed"], true);
        assert!(!state.decisions.contains_key("use-jwt"));
    }

    #[test]
    fn remove_decision_blocked_by_dependent() {
        let (_tmp, store, mut state) = setup();

        let d1 = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        record_decision(&store, &mut state, &d1).unwrap();

        let d2 = json!({
            "component": "auth", "choice": "Token expiry", "reason": "15 min",
            "depends_on": ["use-jwt"],
        });
        record_decision(&store, &mut state, &d2).unwrap();

        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["removed"], false);
        let blocked = result["blocked_by"].as_array().unwrap();
        assert!(!blocked.is_empty());
        assert!(state.decisions.contains_key("use-jwt"));
    }

    #[test]
    fn remove_decision_blocked_by_pattern() {
        let (_tmp, store, mut state) = setup();

        let d1 = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        let d2 = json!({ "component": "auth", "choice": "Token refresh", "reason": "Rotate" });
        record_decision(&store, &mut state, &d1).unwrap();
        record_decision(&store, &mut state, &d2).unwrap();

        let pat = json!({
            "name": "Auth tokens",
            "description": "Token handling",
            "decisions": ["use-jwt", "token-refresh"],
        });
        record_pattern(&store, &mut state, &pat).unwrap();

        // Pattern has exactly 2 members — removing one would violate minimum.
        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["removed"], false);
        let blocked = result["blocked_by"].as_array().unwrap();
        assert!(blocked.iter().any(|b| b["edge"] == "member_of"));
    }

    #[test]
    fn remove_decision_rejects_nonexistent() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "ghost" });
        let err = remove_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    // ── update_decision ─────────────────────────────────────────────────

    #[test]
    fn update_decision_amend_choice() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": "Use JWT v2" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT v2");
        assert_eq!(dec.decision.reason, "Stateless"); // unchanged
    }

    #[test]
    fn update_decision_amend_reason() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "reason": "Better reason" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT"); // unchanged
        assert_eq!(dec.decision.reason, "Better reason");
    }

    #[test]
    fn update_decision_amend_preserves_timestamp() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        record_decision(&store, &mut state, &d).unwrap();
        let original_ts = state.decisions["use-jwt"].decision.created;

        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": "JWT v2" });
        update_decision(&store, &mut state, &args).unwrap();

        assert_eq!(state.decisions["use-jwt"].decision.created, original_ts);
    }

    #[test]
    fn update_decision_amend_rejects_no_changes() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("at least one"));
    }

    #[test]
    fn update_decision_supersede_creates_new() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Session cookies", "reason": "Simple" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "session-cookies",
            "mode": "supersede",
            "choice": "JWT tokens",
            "reason": "Stateless",
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        let new_name = result["name"].as_str().unwrap();
        assert_ne!(new_name, "session-cookies");

        // Old decision still exists.
        assert!(state.decisions.contains_key("session-cookies"));
        // New decision exists.
        assert!(state.decisions.contains_key(new_name));
        // Supersedes edge exists.
        assert!(state.graph_index.edges.iter().any(|e| e.from == new_name
            && e.to == "session-cookies"
            && e.kind == EdgeKind::Supersedes));
    }

    #[test]
    fn update_decision_supersede_inherits_component() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "use-jwt",
            "mode": "supersede",
            "choice": "Use PASETO",
            "reason": "Better defaults",
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        let new_name = result["name"].as_str().unwrap();
        let new_dec = state.decisions.get(new_name).unwrap();
        assert_eq!(new_dec.decision.component, "auth");
    }

    #[test]
    fn update_decision_rejects_invalid_mode() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "X", "reason": "Y" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "x", "mode": "delete" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("invalid mode"));
    }

    #[test]
    fn update_decision_rejects_nonexistent() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "ghost", "mode": "amend", "choice": "X" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }
}
